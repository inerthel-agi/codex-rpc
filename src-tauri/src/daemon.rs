use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(windows)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
use windows::{
    core::PWSTR,
    Win32::{
        Foundation::{CloseHandle, FILETIME, HANDLE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                TH32CS_SNAPPROCESS,
            },
            Pipes::PeekNamedPipe,
            Threading::{
                GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
                PROCESS_QUERY_LIMITED_INFORMATION,
            },
        },
    },
};

const DEFAULT_DISCORD_CLIENT_ID: &str = "1494452015504293908";
const SCAN_INTERVAL_MS: u64 = 5000;
const UI_REFRESH_INTERVAL_MS: u64 = 500;
const CODEX_METADATA_REFRESH_MS: u64 = 2000;
const IPC_RETRY_MS: u64 = 10_000;
const IPC_READ_TIMEOUT_MS: u64 = 5000;
const RPC_REFRESH_INTERVAL_MS: u64 = 15_000;
const IDLE_GRACE_MS: u64 = 10_000;
const LOCAL_USAGE_REFRESH_MS: u64 = 60_000;
const ACCOUNT_USAGE_CACHE_MS: u64 = 30_000;
const ACTIVITY_PLAYING: u8 = 0;
const ACTIVITY_LISTENING: u8 = 2;
const ACTIVITY_WATCHING: u8 = 3;
const ACTIVITY_COMPETING: u8 = 5;
static LAST_LOCAL_USAGE_REFRESH_MS: AtomicU64 = AtomicU64::new(0);
static ACCOUNT_USAGE_CACHE: std::sync::OnceLock<std::sync::Mutex<AccountUsageCache>> =
    std::sync::OnceLock::new();

#[derive(Default)]
struct AccountUsageCache {
    checked_at_ms: u64,
    usage: Option<CodexUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RpcButton {
    label: String,
    url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RpcSettings {
    mode: String,
    buttons: Vec<RpcButton>,
    #[serde(default, skip_serializing)]
    show_usage: Option<bool>,
    #[serde(default = "default_show_usage")]
    show_primary_usage: bool,
    #[serde(default = "default_show_usage")]
    show_weekly_usage: bool,
    #[serde(default = "default_show_usage")]
    show_effort: bool,
    #[serde(default = "default_show_usage")]
    show_fast_mode: bool,
    #[serde(default = "default_show_usage")]
    show_credits: bool,
}

impl Default for RpcSettings {
    fn default() -> Self {
        Self {
            mode: "playing".into(),
            buttons: vec![
                RpcButton {
                    label: "Open Codex".into(),
                    url: "https://chatgpt.com/codex".into(),
                },
                RpcButton {
                    label: "Usage".into(),
                    url: "https://chatgpt.com/codex/settings/analytics".into(),
                },
            ],
            show_usage: None,
            show_primary_usage: true,
            show_weekly_usage: true,
            show_effort: true,
            show_fast_mode: true,
            show_credits: true,
        }
    }
}

fn default_show_usage() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PresenceState {
    Idle,
    Cli,
    App,
    Both,
}

#[derive(Debug, Clone, Default)]
struct ProcessCounts {
    cli: usize,
    app: usize,
    unknown: usize,
}

#[derive(Debug, Clone)]
struct DetectionResult {
    state: PresenceState,
    started_at_ms: Option<u64>,
    codex: Option<CodexConfig>,
    session: Option<CodexSession>,
    usage: Option<CodexUsage>,
}

impl Default for DetectionResult {
    fn default() -> Self {
        Self {
            state: PresenceState::Idle,
            started_at_ms: None,
            codex: None,
            session: None,
            usage: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct CodexConfig {
    model: Option<String>,
    effort: Option<String>,
    service_tier: Option<String>,
}

#[derive(Debug, Clone)]
struct CodexSession {
    cwd: String,
    repo_name: String,
}

#[derive(Debug, Clone)]
struct LimitSnapshot {
    used_percent: f64,
    /// Length of the rate-limit window as reported by Codex. Plans no longer all
    /// expose a 5h window, so labels are derived from this instead of the slot
    /// (`primary`/`secondary`) the limit arrives in.
    window_minutes: Option<u64>,
    resets_at_ms: Option<u64>,
    observed_at_ms: u64,
}

#[derive(Debug, Clone)]
struct CodexUsage {
    limit_id: Option<String>,
    plan_type: Option<String>,
    primary: Option<LimitSnapshot>,
    secondary: Option<LimitSnapshot>,
    credits_remaining: Option<f64>,
}

#[derive(Debug, Clone)]
struct ProcessSnapshot {
    owner: Option<PresenceState>,
    parent_name: Option<String>,
    executable_path: Option<String>,
    creation_date_ms: Option<u64>,
}

#[cfg(windows)]
struct ProcessEntry {
    process_id: u32,
    parent_process_id: u32,
    name: String,
}

#[derive(Default)]
struct StateMachine {
    last_non_idle: Option<DetectionResult>,
    last_non_idle_at_ms: u64,
    last_emitted: DetectionResult,
    anchor_start_ms: Option<u64>,
}

pub fn run(stop: Arc<AtomicBool>, settings_path: Option<PathBuf>, status_path: Option<PathBuf>) {
    let settings_path = settings_path.unwrap_or_else(|| app_data_dir().join("rpc-buttons.json"));
    let status_path = status_path.unwrap_or_else(|| app_data_dir().join("status.txt"));
    let client_id = std::env::var("DISCORD_CLIENT_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_DISCORD_CLIENT_ID.to_string());
    let scan_interval_ms = parse_env_u64("SCAN_INTERVAL_MS", SCAN_INTERVAL_MS, 2000);
    let idle_grace_ms = parse_env_u64("IDLE_GRACE_MS", IDLE_GRACE_MS, 0);

    let mut machine = StateMachine::default();
    let mut ipc: Option<DiscordIpc> = None;
    let mut next_ipc_attempt_at = 0;
    let mut last_key = String::new();
    let mut last_status_line: Option<String> = None;
    let mut last_rpc_refresh_at = 0;
    let mut settings_modified = modified_ms(&settings_path);
    let mut settings = read_rpc_settings(&settings_path);
    let mut result = detect(&mut machine, idle_grace_ms);
    let mut last_scan_at = now_ms();
    let mut last_metadata_refresh_at = last_scan_at;

    while !stop.load(Ordering::SeqCst) {
        if modified_ms(&settings_path) != settings_modified {
            settings_modified = modified_ms(&settings_path);
            settings = read_rpc_settings(&settings_path);
            last_key.clear();
        }

        if ipc.is_none() && now_ms() >= next_ipc_attempt_at {
            ipc = DiscordIpc::connect(&client_id).ok();
            if ipc.is_some() {
                last_key.clear();
            } else {
                next_ipc_attempt_at = now_ms() + IPC_RETRY_MS;
            }
        }

        let now = now_ms();
        if now.saturating_sub(last_scan_at) >= scan_interval_ms {
            result = detect(&mut machine, idle_grace_ms);
            last_scan_at = now;
            last_metadata_refresh_at = now;
        } else if now.saturating_sub(last_metadata_refresh_at) >= CODEX_METADATA_REFRESH_MS {
            refresh_codex_metadata(&mut result);
            last_metadata_refresh_at = now;
        }

        let mut display_result = result.clone();
        // Status line keeps unfiltered usage: the show_* flags only gate what is
        // published to Discord, not what the tray popup displays locally.
        let status_line = format_status_line(
            &display_result,
            &settings,
            ipc.as_ref().and_then(|client| client.username.as_deref()),
        );
        filter_usage(&mut display_result, &settings);
        if last_status_line.as_deref() != Some(status_line.as_str()) {
            write_status(&status_path, &status_line);
            last_status_line = Some(status_line);
        }

        let key = presence_key(&display_result, &settings);
        let should_refresh_rpc = now.saturating_sub(last_rpc_refresh_at) >= RPC_REFRESH_INTERVAL_MS;
        if key != last_key || should_refresh_rpc {
            if let Some(client) = ipc.as_mut() {
                let sent = match build_activity(&display_result, &settings) {
                    Some(activity) => client.set_activity(activity),
                    None => client.clear_activity(),
                };

                if sent.is_ok() {
                    last_key = key;
                    last_rpc_refresh_at = now;
                } else {
                    ipc = None;
                    next_ipc_attempt_at = now + IPC_RETRY_MS;
                    last_key.clear();
                    last_rpc_refresh_at = 0;
                }
            }
        }

        sleep_polling(&stop, UI_REFRESH_INTERVAL_MS);
    }

    if let Some(client) = ipc.as_mut() {
        let _ = client.clear_activity();
    }
    clear_status(&status_path);
}

fn detect(machine: &mut StateMachine, idle_grace_ms: u64) -> DetectionResult {
    let mut counts = ProcessCounts::default();
    let mut oldest: Option<u64> = None;

    for process in scan_codex_processes() {
        match classify_process(&process) {
            PresenceState::Cli => {
                counts.cli += 1;
                oldest = min_option(oldest, process.creation_date_ms);
            }
            PresenceState::App => {
                counts.app += 1;
                oldest = min_option(oldest, process.creation_date_ms);
            }
            PresenceState::Idle => counts.unknown += 1,
            PresenceState::Both => {}
        }
    }

    let state = if counts.cli > 0 && counts.app > 0 {
        PresenceState::Both
    } else if counts.cli > 0 {
        PresenceState::Cli
    } else if counts.app > 0 {
        PresenceState::App
    } else {
        PresenceState::Idle
    };

    let session = if state == PresenceState::Idle {
        None
    } else {
        read_codex_session()
    };

    let result = DetectionResult {
        state,
        started_at_ms: oldest,
        codex: read_codex_config(session.as_ref().map(|session| session.cwd.as_str())),
        session,
        usage: read_codex_usage(),
    };
    machine.step(result, idle_grace_ms)
}

fn refresh_codex_metadata(result: &mut DetectionResult) {
    if result.state == PresenceState::Idle {
        return;
    }
    let session = if result.state == PresenceState::Idle {
        None
    } else {
        read_codex_session()
    };
    result.codex = read_codex_config(session.as_ref().map(|session| session.cwd.as_str()));
    if result.state != PresenceState::Idle {
        result.session = session;
    }
}

impl StateMachine {
    fn step(&mut self, result: DetectionResult, idle_grace_ms: u64) -> DetectionResult {
        let now = now_ms();
        if result.state != PresenceState::Idle {
            if self.anchor_start_ms.is_none() || self.last_emitted.state == PresenceState::Idle {
                self.anchor_start_ms = result.started_at_ms;
            } else {
                self.anchor_start_ms = min_option(self.anchor_start_ms, result.started_at_ms);
            }

            let mut merged = result;
            merged.started_at_ms = self.anchor_start_ms;
            self.last_non_idle = Some(merged.clone());
            self.last_non_idle_at_ms = now;
            self.last_emitted = merged.clone();
            return merged;
        }

        if let Some(last) = &self.last_non_idle {
            if now.saturating_sub(self.last_non_idle_at_ms) < idle_grace_ms {
                return last.clone();
            }
        }

        self.last_non_idle = None;
        self.anchor_start_ms = None;
        self.last_emitted = result.clone();
        result
    }
}

fn classify_process(process: &ProcessSnapshot) -> PresenceState {
    if let Some(owner) = process.owner {
        return owner;
    }
    let exe = process
        .executable_path
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let exe_unix = exe.replace('\\', "/");
    let parent = process
        .parent_name
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let parent_name = command_basename(&parent);

    if exe_unix.contains("/openai/codex/")
        || exe_unix.contains("/programs/codex/")
        || exe_unix.contains("/program files/codex/")
        || exe_unix.contains("/codex.app/contents/")
    {
        return PresenceState::App;
    }

    if exe.contains("\\node_modules\\@openai\\codex\\")
        || exe_unix.contains("/node_modules/@openai/codex/")
    {
        return PresenceState::Cli;
    }

    let shell_parent = matches!(
        parent_name.as_str(),
        "cmd.exe"
            | "powershell.exe"
            | "pwsh.exe"
            | "windowsterminal.exe"
            | "wt.exe"
            | "bash.exe"
            | "code.exe"
            | "cursor.exe"
            | "conemu.exe"
            | "conemu64.exe"
            | "conemuc.exe"
            | "conemuc64.exe"
            | "alacritty.exe"
            | "tabby.exe"
            | "fluent-terminal.exe"
            | "hyper.exe"
            | "zsh"
            | "bash"
            | "sh"
            | "fish"
            | "nu"
            | "terminal"
            | "iterm2"
            | "warp"
            | "ghostty"
            | "alacritty"
            | "tabby"
            | "hyper"
            | "code"
            | "cursor"
    ) || parent.contains(".app/contents/macos/code")
        || parent.contains(".app/contents/macos/cursor")
        || parent.contains(".app/contents/macos/terminal")
        || parent.contains(".app/contents/macos/iterm2");

    if shell_parent {
        return PresenceState::Cli;
    }
    if !exe.is_empty() {
        return PresenceState::App;
    }
    PresenceState::Idle
}

#[cfg(windows)]
fn scan_codex_processes() -> Vec<ProcessSnapshot> {
    scan_codex_processes_windows()
}

#[cfg(target_os = "macos")]
fn scan_codex_processes() -> Vec<ProcessSnapshot> {
    scan_codex_processes_macos()
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn scan_codex_processes() -> Vec<ProcessSnapshot> {
    Vec::new()
}

#[cfg(windows)]
fn scan_codex_processes_windows() -> Vec<ProcessSnapshot> {
    let entries = list_process_entries();
    let names = entries
        .iter()
        .map(|entry| (entry.process_id, entry.name.clone()))
        .collect::<HashMap<_, _>>();
    let parents = entries
        .iter()
        .map(|entry| {
            (
                entry.process_id,
                (entry.parent_process_id, entry.name.clone()),
            )
        })
        .collect();

    entries
        .into_iter()
        .filter(|entry| entry.name.eq_ignore_ascii_case("codex.exe"))
        .map(|entry| ProcessSnapshot {
            owner: process_owner(entry.parent_process_id, &parents),
            parent_name: names.get(&entry.parent_process_id).cloned(),
            executable_path: query_process_path(entry.process_id),
            creation_date_ms: query_process_creation_ms(entry.process_id),
        })
        .collect()
}

fn process_owner(mut pid: u32, parents: &HashMap<u32, (u32, String)>) -> Option<PresenceState> {
    // Bound traversal even if a snapshot contains a recycled PID or a cycle.
    for _ in 0..parents.len() {
        if pid == std::process::id() {
            return Some(PresenceState::Idle);
        }
        let (parent_pid, name) = parents.get(&pid)?;
        let name = name.to_ascii_lowercase();
        if matches!(
            command_basename(&name).as_str(),
            "codex-rich-presence.exe"
                | "codex_rich_presence_tray.exe"
                | "codex-rich-presence"
                | "codex_rich_presence_tray"
        ) {
            return Some(PresenceState::Idle);
        }
        if matches!(command_basename(&name).as_str(), "chatgpt.exe" | "chatgpt")
            || name.contains("/codex.app/contents/")
        {
            return Some(PresenceState::App);
        }
        pid = *parent_pid;
    }
    None
}

#[cfg(windows)]
fn list_process_entries() -> Vec<ProcessEntry> {
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return Vec::new();
    };

    let mut entries = Vec::new();
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };

    if unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok() {
        loop {
            entries.push(ProcessEntry {
                process_id: entry.th32ProcessID,
                parent_process_id: entry.th32ParentProcessID,
                name: wide_to_string(&entry.szExeFile),
            });

            if unsafe { Process32NextW(snapshot, &mut entry) }.is_err() {
                break;
            }
        }
    }

    close_handle(snapshot);
    entries
}

#[cfg(windows)]
fn query_process_path(process_id: u32) -> Option<String> {
    let handle = open_process_query(process_id)?;
    let mut buffer = vec![0u16; 32_768];
    let mut len = buffer.len() as u32;
    let result = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        )
    };
    close_handle(handle);
    result.ok()?;
    Some(String::from_utf16_lossy(&buffer[..len as usize]))
}

#[cfg(windows)]
fn query_process_creation_ms(process_id: u32) -> Option<u64> {
    let handle = open_process_query(process_id)?;
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let result =
        unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
    close_handle(handle);
    result.ok()?;
    filetime_to_unix_ms(creation)
}

#[cfg(windows)]
fn open_process_query(process_id: u32) -> Option<HANDLE> {
    unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }.ok()
}

#[cfg(windows)]
fn close_handle(handle: HANDLE) {
    let _ = unsafe { CloseHandle(handle) };
}

#[cfg(windows)]
fn wide_to_string(value: &[u16]) -> String {
    let len = value.iter().position(|ch| *ch == 0).unwrap_or(value.len());
    String::from_utf16_lossy(&value[..len])
}

#[cfg(windows)]
fn filetime_to_unix_ms(value: FILETIME) -> Option<u64> {
    const WINDOWS_TO_UNIX_EPOCH_MS: u64 = 11_644_473_600_000;
    let ticks = ((value.dwHighDateTime as u64) << 32) | value.dwLowDateTime as u64;
    let ms = ticks / 10_000;
    ms.checked_sub(WINDOWS_TO_UNIX_EPOCH_MS)
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
struct MacProcessEntry {
    process_id: u32,
    parent_process_id: u32,
    command: String,
}

#[cfg(target_os = "macos")]
fn scan_codex_processes_macos() -> Vec<ProcessSnapshot> {
    let entries = list_macos_process_entries();
    let commands = entries
        .iter()
        .map(|entry| (entry.process_id, entry.command.clone()))
        .collect::<HashMap<_, _>>();
    let parents = entries
        .iter()
        .map(|entry| {
            (
                entry.process_id,
                (entry.parent_process_id, entry.command.clone()),
            )
        })
        .collect();

    entries
        .into_iter()
        .filter(|entry| is_macos_codex_candidate(&entry.command))
        .map(|entry| ProcessSnapshot {
            owner: process_owner(entry.parent_process_id, &parents),
            parent_name: commands.get(&entry.parent_process_id).cloned(),
            executable_path: Some(entry.command),
            creation_date_ms: None,
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn list_macos_process_entries() -> Vec<MacProcessEntry> {
    let Ok(output) = std::process::Command::new("/bin/ps")
        .args(["-axo", "pid=,ppid=,command="])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_macos_process_line)
        .collect()
}

#[cfg(target_os = "macos")]
fn parse_macos_process_line(line: &str) -> Option<MacProcessEntry> {
    let (process_id, rest) = split_process_field(line)?;
    let (parent_process_id, rest) = split_process_field(rest)?;
    let process_id = process_id.parse().ok()?;
    let parent_process_id = parent_process_id.parse().ok()?;
    let command = rest.trim_start().to_string();
    if command.is_empty() {
        return None;
    }
    Some(MacProcessEntry {
        process_id,
        parent_process_id,
        command,
    })
}

#[cfg(target_os = "macos")]
fn split_process_field(input: &str) -> Option<(&str, &str)> {
    let input = input.trim_start();
    if input.is_empty() {
        return None;
    }
    let end = input.find(char::is_whitespace).unwrap_or(input.len());
    Some((&input[..end], &input[end..]))
}

#[cfg(target_os = "macos")]
fn is_macos_codex_candidate(command: &str) -> bool {
    let command = command.to_ascii_lowercase();
    if command.contains("codex-rich-presence") {
        return false;
    }
    command.contains("/node_modules/@openai/codex/")
        || command.contains("/@openai/codex/")
        || command.contains(".app/contents/macos/codex")
        || command_basename(&command) == "codex"
}

fn command_basename(command: &str) -> String {
    let executable = command.split_whitespace().next().unwrap_or(command);
    executable
        .trim_matches('"')
        .trim_end_matches(['\\', '/'])
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(executable)
        .to_ascii_lowercase()
}

fn build_activity(result: &DetectionResult, settings: &RpcSettings) -> Option<Value> {
    if result.state == PresenceState::Idle {
        return None;
    }
    let mode = normalize_mode(&settings.mode);
    let activity_type = match mode.as_str() {
        "watching" => ACTIVITY_WATCHING,
        "listening" => ACTIVITY_LISTENING,
        "competing" => ACTIVITY_COMPETING,
        _ => ACTIVITY_PLAYING,
    };

    let mut activity = json!({
        "name": "Codex",
        "type": activity_type,
        "created_at": now_ms(),
        "instance": false,
        "details": build_details(result, &mode),
        "state": build_state_line(result, settings),
        "assets": {
            "large_image": "codex_logo",
            "large_text": build_large_image_text(result),
            "small_image": small_image_key(result.state),
            "small_text": small_image_text(result.state),
        },
    });

    if let Some(started_at_ms) = result.started_at_ms {
        activity["timestamps"] = json!({ "start": started_at_ms / 1000 });
    }
    if mode == "watching" && !settings.buttons.is_empty() {
        activity["buttons"] =
            serde_json::to_value(settings.buttons.iter().take(2).collect::<Vec<_>>()).ok()?;
    }

    Some(activity)
}

fn build_details(result: &DetectionResult, mode: &str) -> String {
    let base = match (result.state, mode) {
        (PresenceState::Cli, "watching") => "Watching Codex CLI",
        (PresenceState::App, "watching") => "Watching Codex",
        (PresenceState::Both, "watching") => "Watching Codex (CLI + Desktop)",
        (PresenceState::Cli, _) => "Coding with Codex CLI",
        (PresenceState::App, _) => "Using Codex",
        (PresenceState::Both, _) => "Coding with Codex (CLI + Desktop)",
        (PresenceState::Idle, _) => "",
    };
    if let Some(repo) = result
        .session
        .as_ref()
        .and_then(|session| sanitize_field(Some(&session.repo_name), 32))
    {
        let candidate = format!("{base} - {repo}");
        if candidate.len() <= 96 {
            return candidate;
        }
    }
    base.to_string()
}

fn build_state_line(result: &DetectionResult, settings: &RpcSettings) -> String {
    let model = result
        .codex
        .as_ref()
        .and_then(|cfg| cfg.model.as_deref())
        .and_then(format_model);
    let effort = result
        .codex
        .as_ref()
        .and_then(|cfg| cfg.effort.as_deref())
        .and_then(format_effort);
    let speed = settings
        .show_fast_mode
        .then(|| {
            format_speed(
                result
                    .codex
                    .as_ref()
                    .and_then(|cfg| cfg.service_tier.as_deref()),
            )
        })
        .flatten();
    let mut parts = Vec::new();
    if let Some(model) = model {
        parts.push(model);
    }
    if let Some(effort) = effort {
        parts.push(effort);
    }
    if let Some(speed) = speed {
        parts.push(speed);
    }
    let base = if parts.is_empty() {
        match result.state {
            PresenceState::Cli => "Terminal session active".into(),
            PresenceState::App => "Desktop session".into(),
            PresenceState::Both => "CLI + Desktop".into(),
            PresenceState::Idle => String::new(),
        }
    } else {
        parts.join(" - ")
    };

    let usage = compact_usage_parts(result);
    for count in (0..=usage.len()).rev() {
        let suffix = usage[..count].join(" - ");
        let candidate = if suffix.is_empty() {
            base.clone()
        } else {
            format!("{base} - {suffix}")
        };
        // Discord's state field allows 128 characters.
        if candidate.len() <= 128 {
            return candidate;
        }
    }
    truncate(base, 128)
}

fn build_large_image_text(result: &DetectionResult) -> String {
    let usage = compact_usage_parts(result);
    if usage.is_empty() {
        "OpenAI Codex".into()
    } else {
        truncate(format!("OpenAI Codex - {}", usage.join(" - ")), 128)
    }
}

fn compact_usage_parts(result: &DetectionResult) -> Vec<String> {
    let mut parts = Vec::new();
    if let Some(usage) = &result.usage {
        if let Some(primary) = &usage.primary {
            parts.push(format!(
                "{} {}%",
                usage_label(primary, "5h"),
                remaining_percent(primary)
            ));
        }
        if let Some(secondary) = &usage.secondary {
            parts.push(format!(
                "{} {}%",
                usage_label(secondary, "week"),
                remaining_percent(secondary)
            ));
        }
    }
    parts
}

fn format_status_line(
    result: &DetectionResult,
    settings: &RpcSettings,
    discord_user: Option<&str>,
) -> String {
    let state = match result.state {
        PresenceState::Both => "Codex: CLI/Desktop",
        PresenceState::Cli => "Codex: CLI",
        PresenceState::App => "Codex: Desktop",
        PresenceState::Idle => "Codex: Off",
    };
    let model = result
        .codex
        .as_ref()
        .and_then(|cfg| cfg.model.as_deref())
        .and_then(format_model);
    let effort = result
        .codex
        .as_ref()
        .and_then(|cfg| cfg.effort.as_deref())
        .and_then(format_effort);
    let speed = settings
        .show_fast_mode
        .then(|| {
            format_speed(
                result
                    .codex
                    .as_ref()
                    .and_then(|cfg| cfg.service_tier.as_deref()),
            )
        })
        .flatten();
    let model_line = [model, effort, speed]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" - ");
    let usage_line = format_usage(result.usage.as_ref()).unwrap_or_default();
    let discord = match discord_user {
        Some(user) => format!("Discord: Connected ({user})"),
        None => "Discord: Not connected".into(),
    };
    let plan = result
        .usage
        .as_ref()
        .and_then(|usage| usage.plan_type.as_deref())
        .unwrap_or("");
    format!("{state}|{model_line}|{usage_line}|{discord}|{plan}")
}

fn format_usage(usage: Option<&CodexUsage>) -> Option<String> {
    let usage = usage?;
    let mut parts = Vec::new();
    if let Some(primary) = &usage.primary {
        parts.push(format!(
            "{} {}% left",
            usage_label(primary, "5h"),
            remaining_percent(primary)
        ));
    }
    if let Some(secondary) = &usage.secondary {
        parts.push(format!(
            "{} {}% left",
            usage_label(secondary, "week"),
            remaining_percent(secondary)
        ));
    }
    if let Some(credits) = usage.credits_remaining {
        parts.push(format!("credits {}", credits.round()));
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!("Usage: {}", parts.join(" / ")))
    }
}

fn read_rpc_settings(path: &Path) -> RpcSettings {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(_) => return RpcSettings::default(),
    };
    let mut settings =
        serde_json::from_str::<RpcSettings>(raw.trim_start_matches('\u{feff}')).unwrap_or_default();
    if settings.show_usage == Some(false) {
        settings.show_primary_usage = false;
        settings.show_weekly_usage = false;
    }
    settings.show_usage = None;
    settings.mode = normalize_mode(&settings.mode);
    settings.buttons = settings
        .buttons
        .into_iter()
        .filter_map(|button| {
            let label = clean_label(&button.label)?;
            let url = clean_url(&button.url)?;
            Some(RpcButton { label, url })
        })
        .take(2)
        .collect();
    settings
}

fn filter_usage(result: &mut DetectionResult, settings: &RpcSettings) {
    if let Some(usage) = result.usage.as_mut() {
        if !settings.show_primary_usage {
            usage.primary = None;
        }
        if !settings.show_weekly_usage {
            usage.secondary = None;
        }
        if !settings.show_credits {
            usage.credits_remaining = None;
        }
    }
    if !settings.show_effort {
        if let Some(codex) = result.codex.as_mut() {
            codex.effort = None;
        }
    }
}

fn read_codex_config(project_cwd: Option<&str>) -> Option<CodexConfig> {
    let config_path = home_dir().join(".codex").join("config.toml");
    let mut cfg = if let Ok(raw) = fs::read_to_string(&config_path) {
        parse_codex_config(&raw, project_cwd)
    } else {
        CodexConfig::default()
    };
    let config_mtime_ms = fs::metadata(&config_path)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0);
    // A `/model` change rewrites config.toml immediately, but the last
    // turn_context in the rollout log still carries the previous model until
    // the next turn starts. When the config file is newer than the last
    // recorded turn, trust the file over the stale runtime snapshot.
    if let Some((runtime_cfg, turn_ms)) = read_turn_context_config() {
        if turn_ms >= config_mtime_ms {
            if runtime_cfg.model.is_some() {
                cfg.model = runtime_cfg.model;
            }
            if runtime_cfg.effort.is_some() {
                cfg.effort = runtime_cfg.effort;
            }
            if runtime_cfg.service_tier.is_some() {
                cfg.service_tier = runtime_cfg.service_tier;
            }
        }
    }
    if cfg.model.is_none() && cfg.effort.is_none() && cfg.service_tier.is_none() {
        None
    } else {
        Some(cfg)
    }
}

fn parse_codex_config(raw: &str, project_cwd: Option<&str>) -> CodexConfig {
    let mut cfg = CodexConfig::default();
    let project_cwd = project_cwd.map(normalize_project_path);
    let mut in_top_level = true;
    let mut in_matching_project = false;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_top_level = false;
            in_matching_project = project_cwd
                .as_deref()
                .zip(extract_project_section_path(trimmed).as_deref())
                .map(|(cwd, section)| cwd == normalize_project_path(section))
                .unwrap_or(false);
            continue;
        }
        if !(in_top_level || in_matching_project) {
            continue;
        }
        if let Some(value) = extract_toml_string(trimmed, "model") {
            cfg.model = Some(value);
        }
        if let Some(value) = extract_toml_string(trimmed, "model_reasoning_effort") {
            cfg.effort = Some(value);
        }
        if let Some(value) = extract_toml_string(trimmed, "service_tier") {
            cfg.service_tier = Some(value);
        }
    }
    cfg
}

fn extract_project_section_path(line: &str) -> Option<String> {
    let value = line.strip_prefix("[projects.")?.strip_suffix(']')?.trim();
    if value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2 {
        return Some(value[1..value.len() - 1].to_string());
    }
    if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
        return Some(value[1..value.len() - 1].replace("\\\"", "\""));
    }
    None
}

/// Returns the latest turn_context config plus the moment that turn started
/// (line timestamp when present, otherwise the rollout file's mtime).
fn read_turn_context_config() -> Option<(CodexConfig, u64)> {
    for rollout in find_recent_rollout_files(&sessions_dir(), 24 * 60 * 60 * 1000) {
        let Some(lines) = read_tail_lines(&rollout.0, 1024 * 1024) else {
            continue;
        };
        for line in lines.iter().rev() {
            if let Some((cfg, ts)) = parse_turn_context_line(line) {
                return Some((cfg, ts.unwrap_or(rollout.1)));
            }
        }
    }
    None
}

fn parse_turn_context_line(line: &str) -> Option<(CodexConfig, Option<u64>)> {
    let obj: Value = serde_json::from_str(line).ok()?;
    if obj.get("type").and_then(Value::as_str) != Some("turn_context") {
        return None;
    }
    let payload = obj.get("payload")?;
    let cfg = CodexConfig {
        model: payload
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string),
        effort: payload
            .get("effort")
            .and_then(Value::as_str)
            .map(str::to_string),
        service_tier: payload
            .get("service_tier")
            .or_else(|| payload.get("serviceTier"))
            .and_then(Value::as_str)
            .map(str::to_string),
    };
    if cfg.model.is_none() && cfg.effort.is_none() && cfg.service_tier.is_none() {
        None
    } else {
        let ts = obj
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_utc_timestamp_ms);
        Some((cfg, ts))
    }
}

/// Parses a rollout line timestamp ("2026-07-09T18:00:59.694Z") to unix ms.
/// Rollout timestamps are always UTC; anything else returns None.
fn parse_utc_timestamp_ms(ts: &str) -> Option<u64> {
    let ts = ts.strip_suffix('Z')?;
    let (date, time) = ts.split_once('T')?;
    let mut d = date.splitn(3, '-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    let (hms, frac) = time.split_once('.').unwrap_or((time, ""));
    let mut t = hms.splitn(3, ':');
    let hour: i64 = t.next()?.parse().ok()?;
    let min: i64 = t.next()?.parse().ok()?;
    let sec: i64 = t.next()?.parse().ok()?;
    let millis: i64 = format!("{frac}000")[..3].parse().ok()?;
    // days-from-civil (Howard Hinnant): days since 1970-01-01 without chrono.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let ms = (days * 86400 + hour * 3600 + min * 60 + sec) * 1000 + millis;
    u64::try_from(ms).ok()
}

fn read_codex_session() -> Option<CodexSession> {
    let latest = find_latest_rollout_file(&sessions_dir(), 24 * 60 * 60 * 1000)?;
    let first_line = read_first_line(&latest.0)?;
    let obj: Value = serde_json::from_str(first_line.trim()).ok()?;
    if obj.get("type").and_then(Value::as_str) != Some("session_meta") {
        return None;
    }
    let cwd = obj
        .get("payload")
        .and_then(|payload| payload.get("cwd"))
        .and_then(Value::as_str)?;
    Some(CodexSession {
        cwd: strip_windows_long_prefix(cwd).to_string(),
        repo_name: basename_safe(strip_windows_long_prefix(cwd)),
    })
}

fn read_codex_usage() -> Option<CodexUsage> {
    if let Some(usage) = read_codex_account_usage() {
        return Some(usage);
    }
    refresh_local_codex_usage();

    for rollout in find_recent_rollout_files(&sessions_dir(), 24 * 60 * 60 * 1000) {
        let Some(lines) = read_tail_lines(&rollout.0, 256 * 1024) else {
            continue;
        };
        for line in lines.iter().rev() {
            if let Some(usage) = parse_usage_line(line, rollout.1) {
                if usage.limit_id.as_deref().is_none_or(|id| id == "codex") {
                    return Some(usage);
                }
            }
        }
    }
    None
}

fn read_codex_account_usage() -> Option<CodexUsage> {
    let cache =
        ACCOUNT_USAGE_CACHE.get_or_init(|| std::sync::Mutex::new(AccountUsageCache::default()));
    let now = now_ms();
    {
        let guard = cache.lock().ok()?;
        if guard.checked_at_ms != 0
            && now.saturating_sub(guard.checked_at_ms) < ACCOUNT_USAGE_CACHE_MS
        {
            return guard.usage.clone();
        }
    }
    let usage = read_codex_account_usage_uncached();
    if let Ok(mut guard) = cache.lock() {
        guard.checked_at_ms = now_ms();
        guard.usage = usage.clone();
    }
    usage
}

fn read_codex_account_usage_uncached() -> Option<CodexUsage> {
    const ACCOUNT_USAGE_TIMEOUT_MS: u64 = 5000;
    const INIT_REQUEST: &[u8] = b"{\"jsonrpc\":\"2.0\",\"id\":0,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"codex-rpc\",\"version\":\"0\"}}}\n";
    // Codex 0.144+ rejects requests sent before the `initialized` notification.
    const INITIALIZED_NOTIFICATION: &[u8] = b"{\"jsonrpc\":\"2.0\",\"method\":\"initialized\"}\n";
    const READ_REQUEST: &[u8] =
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"account/rateLimits/read\",\"params\":null}\n";

    for command in codex_command_candidates() {
        let mut cmd = std::process::Command::new(&command);
        cmd.arg("app-server")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            cmd.creation_flags(0x08000000);
        }
        let Ok(mut child) = cmd.spawn() else {
            continue;
        };

        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(INIT_REQUEST);
            let _ = stdin.write_all(INITIALIZED_NOTIFICATION);
            let _ = stdin.write_all(READ_REQUEST);
            let _ = stdin.flush();
        }

        let mut stdout = match child.stdout.take() {
            Some(pipe) => pipe,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                continue;
            }
        };

        let started = now_ms();
        let mut buffer = Vec::with_capacity(4096);
        let mut chunk = [0u8; 1024];
        let mut completed_response: Option<String> = None;
        while now_ms().saturating_sub(started) < ACCOUNT_USAGE_TIMEOUT_MS {
            #[cfg(windows)]
            if wait_pipe_readable(
                &stdout,
                ACCOUNT_USAGE_TIMEOUT_MS.saturating_sub(now_ms().saturating_sub(started)),
            )
            .is_err()
            {
                break;
            }
            match stdout.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if buffer.len() + n > 1024 * 1024 {
                        break;
                    }
                    buffer.extend_from_slice(&chunk[..n]);
                    if let Some(idx) = buffer.iter().rposition(|byte| *byte == b'\n') {
                        let text = String::from_utf8_lossy(&buffer[..idx]).into_owned();
                        if text.lines().any(line_is_id_one) {
                            completed_response = Some(text);
                            break;
                        }
                    }
                }
                Err(_) => break,
            }
        }
        let _ = child.kill();
        let _ = child.wait();

        if let Some(raw) = completed_response {
            if let Some(usage) = parse_account_usage_response(&raw, now_ms()) {
                return Some(usage);
            }
        }
    }
    None
}

fn line_is_id_one(line: &str) -> bool {
    let trimmed = line.trim();
    if !trimmed.starts_with('{') {
        return false;
    }
    serde_json::from_str::<Value>(trimmed)
        .ok()
        .and_then(|value| {
            value.get("id").and_then(|id| {
                id.as_u64()
                    .map(|value| value == 1)
                    .or_else(|| id.as_str().map(|value| value == "1"))
            })
        })
        .unwrap_or(false)
}

fn refresh_local_codex_usage() {
    let now = now_ms();
    let last = LAST_LOCAL_USAGE_REFRESH_MS.load(Ordering::Relaxed);
    if now.saturating_sub(last) < LOCAL_USAGE_REFRESH_MS {
        return;
    }
    LAST_LOCAL_USAGE_REFRESH_MS.store(now, Ordering::Relaxed);

    for command in codex_command_candidates() {
        if run_codex_command(&command, ["login", "status"]).unwrap_or(false) {
            return;
        }
    }
}

fn run_codex_command<const N: usize>(command: &Path, args: [&str; N]) -> Option<bool> {
    let mut cmd = std::process::Command::new(command);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        cmd.creation_flags(0x08000000);
    }
    let mut child = cmd.spawn().ok()?;
    let started = now_ms();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status.success()),
            Ok(None) if now_ms().saturating_sub(started) < 2500 => {
                thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Some(false);
            }
        }
    }
}

fn codex_command_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    #[cfg(windows)]
    {
        if let Some(app_data) = std::env::var_os("APPDATA") {
            let npm = PathBuf::from(app_data).join("npm");
            let arch = if cfg!(target_arch = "aarch64") {
                "arm64"
            } else {
                "x64"
            };
            let target = if cfg!(target_arch = "aarch64") {
                "aarch64-pc-windows-msvc"
            } else {
                "x86_64-pc-windows-msvc"
            };
            let native = npm
                .join("node_modules/@openai/codex/node_modules/@openai")
                .join(format!("codex-win32-{arch}"))
                .join("vendor")
                .join(target)
                .join("codex/codex.exe");
            candidates.push(if native.is_file() {
                native
            } else {
                npm.join("codex.cmd")
            });
        }
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            candidates.push(
                PathBuf::from(program_files)
                    .join("nodejs")
                    .join("codex.cmd"),
            );
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(PathBuf::from(home).join(".local").join("bin").join("codex"));
        }
        candidates.push(PathBuf::from("/opt/homebrew/bin/codex"));
        candidates.push(PathBuf::from("/usr/local/bin/codex"));
        candidates.push(PathBuf::from("/usr/bin/codex"));
    }
    candidates.retain(|p| p.is_file());
    candidates
}

fn parse_account_usage_response(raw: &str, observed_at_ms: u64) -> Option<CodexUsage> {
    for line in raw.lines() {
        let Ok(msg) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let id_matches = msg
            .get("id")
            .and_then(|id| {
                id.as_u64()
                    .map(|value| value == 1)
                    .or_else(|| id.as_str().map(|value| value == "1"))
            })
            .unwrap_or(false);
        if !id_matches {
            continue;
        }
        if let Some(usage) = parse_account_usage_payload(msg.get("result")?, observed_at_ms) {
            return Some(usage);
        }
    }
    None
}

fn parse_account_usage_payload(payload: &Value, observed_at_ms: u64) -> Option<CodexUsage> {
    let by_id = payload
        .get("rateLimitsByLimitId")
        .and_then(Value::as_object);
    let codex_entry = by_id.and_then(|map| map.get("codex"));
    let limits = codex_entry.or_else(|| payload.get("rateLimits"))?;
    Some(normalize_usage(CodexUsage {
        limit_id: limits
            .get("limitId")
            .and_then(Value::as_str)
            .map(str::to_string),
        primary: parse_account_limit(limits.get("primary"), observed_at_ms),
        secondary: parse_account_limit(limits.get("secondary"), observed_at_ms),
        credits_remaining: parse_credits(limits.get("credits")),
        plan_type: limits
            .get("planType")
            .and_then(Value::as_str)
            .map(str::to_string),
    }))
}

fn parse_usage_line(line: &str, observed_at_ms: u64) -> Option<CodexUsage> {
    let obj: Value = serde_json::from_str(line).ok()?;
    if obj.get("type").and_then(Value::as_str) != Some("event_msg") {
        return None;
    }
    let payload = obj.get("payload")?;
    if payload.get("type").and_then(Value::as_str) != Some("token_count") {
        return None;
    }
    let limits = payload.get("rate_limits")?;
    Some(normalize_usage(CodexUsage {
        limit_id: limits
            .get("limit_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        primary: parse_limit(limits.get("primary"), observed_at_ms),
        secondary: parse_limit(limits.get("secondary"), observed_at_ms),
        credits_remaining: limits
            .get("credits")
            .and_then(|credits| credits.get("remaining").or_else(|| credits.get("balance")))
            .and_then(Value::as_f64),
        plan_type: limits
            .get("plan_type")
            .and_then(Value::as_str)
            .map(str::to_string),
    }))
}

// Slots vary by plan: classify windows before applying the visibility switches.
fn normalize_usage(mut usage: CodexUsage) -> CodexUsage {
    let primary = usage.primary.take();
    let secondary = usage.secondary.take();
    for (limit, fallback) in [(primary, 300), (secondary, 10080)] {
        if let Some(limit) = limit {
            match limit.window_minutes.unwrap_or(fallback) {
                10080.. => usage.secondary = Some(limit),
                300 => usage.primary = Some(limit),
                _ => {}
            }
        }
    }
    if usage
        .plan_type
        .as_deref()
        .is_some_and(|plan| plan.to_ascii_lowercase().starts_with("pro"))
    {
        usage.primary = None;
    }
    usage
}

fn parse_limit(value: Option<&Value>, observed_at_ms: u64) -> Option<LimitSnapshot> {
    let value = value?;
    Some(LimitSnapshot {
        used_percent: value.get("used_percent")?.as_f64()?,
        window_minutes: value
            .get("window_minutes")
            .and_then(Value::as_f64)
            .map(|minutes| minutes.round().max(0.0) as u64),
        resets_at_ms: value
            .get("resets_at")
            .and_then(Value::as_u64)
            .map(|seconds| seconds.saturating_mul(1000)),
        observed_at_ms,
    })
}

fn parse_account_limit(value: Option<&Value>, observed_at_ms: u64) -> Option<LimitSnapshot> {
    let value = value?;
    Some(LimitSnapshot {
        used_percent: value.get("usedPercent")?.as_f64()?,
        window_minutes: value
            .get("windowDurationMins")
            .and_then(Value::as_f64)
            .map(|minutes| minutes.round().max(0.0) as u64),
        resets_at_ms: value
            .get("resetsAt")
            .and_then(Value::as_u64)
            .map(|seconds| seconds.saturating_mul(1000)),
        observed_at_ms,
    })
}

fn parse_credits(value: Option<&Value>) -> Option<f64> {
    let credits = value?;
    credits
        .get("remaining")
        .or_else(|| credits.get("balance"))
        .and_then(|value| value.as_f64().or_else(|| value.as_str()?.parse().ok()))
}

fn find_latest_rollout_file(root: &Path, max_age_ms: u64) -> Option<(PathBuf, u64)> {
    fn walk(dir: &Path, now: u64, max_age_ms: u64, best: &mut Option<(PathBuf, u64)>) {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                walk(&path, now, max_age_ms, best);
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !file_type.is_file() || !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
                continue;
            }
            let Some(mtime) = modified_ms(&path) else {
                continue;
            };
            if now.saturating_sub(mtime) > max_age_ms {
                continue;
            }
            if best
                .as_ref()
                .map(|(_, best_time)| mtime > *best_time)
                .unwrap_or(true)
            {
                *best = Some((path, mtime));
            }
        }
    }

    let mut best = None;
    walk(root, now_ms(), max_age_ms, &mut best);
    best
}

fn find_recent_rollout_files(root: &Path, max_age_ms: u64) -> Vec<(PathBuf, u64)> {
    fn walk(dir: &Path, now: u64, max_age_ms: u64, files: &mut Vec<(PathBuf, u64)>) {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                walk(&path, now, max_age_ms, files);
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !file_type.is_file() || !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
                continue;
            }
            let Some(mtime) = modified_ms(&path) else {
                continue;
            };
            if now.saturating_sub(mtime) <= max_age_ms {
                files.push((path, mtime));
            }
        }
    }

    let mut files = Vec::new();
    walk(root, now_ms(), max_age_ms, &mut files);
    files.sort_by_key(|file| std::cmp::Reverse(file.1));
    files
}

fn read_first_line(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut buf = vec![0; 8192];
    let len = file.read(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf[..len]);
    Some(text.split('\n').next().unwrap_or_default().to_string())
}

fn read_tail_lines(path: &Path, max_bytes: u64) -> Option<Vec<String>> {
    let mut file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    let len = size.min(max_bytes);
    let offset = size.saturating_sub(len);
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0; len as usize];
    file.read_exact(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let mut lines = text
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if offset > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    Some(lines)
}

fn normalize_project_path(path: &str) -> String {
    strip_windows_long_prefix(path)
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

struct DiscordIpc {
    connection: IpcConnection,
    username: Option<String>,
    nonce: u64,
}

enum IpcConnection {
    #[cfg(windows)]
    File(File),
    #[cfg(unix)]
    Unix(UnixStream),
}

impl Read for IpcConnection {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(windows)]
            Self::File(file) => {
                wait_pipe_readable(file, IPC_READ_TIMEOUT_MS)?;
                file.read(buf)
            }
            #[cfg(unix)]
            Self::Unix(stream) => stream.read(buf),
        }
    }
}

// Named pipes opened as files block forever on read; poll with PeekNamedPipe
// so a stalled Discord client cannot hang the daemon loop.
#[cfg(windows)]
fn wait_pipe_readable(file: &impl AsRawHandle, timeout_ms: u64) -> std::io::Result<()> {
    let handle = HANDLE(file.as_raw_handle());
    let deadline = now_ms() + timeout_ms;
    loop {
        let mut available = 0u32;
        unsafe { PeekNamedPipe(handle, None, 0, None, Some(&mut available), None) }.map_err(
            |_| std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "discord ipc closed"),
        )?;
        if available > 0 {
            return Ok(());
        }
        if now_ms() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "discord ipc read timeout",
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

impl Write for IpcConnection {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(windows)]
            Self::File(file) => file.write(buf),
            #[cfg(unix)]
            Self::Unix(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(windows)]
            Self::File(file) => file.flush(),
            #[cfg(unix)]
            Self::Unix(stream) => stream.flush(),
        }
    }
}

impl DiscordIpc {
    fn connect(client_id: &str) -> std::io::Result<Self> {
        let mut client = Self {
            connection: connect_discord_ipc()?,
            username: None,
            nonce: 0,
        };
        client.send_frame(0, &json!({ "v": 1, "client_id": client_id }))?;
        let ready = client.read_frame()?;
        client.username = ready
            .get("data")
            .and_then(|data| data.get("user"))
            .and_then(|user| user.get("username"))
            .and_then(Value::as_str)
            .map(|value| sanitize_discord_user(value).unwrap_or_else(|| value.to_string()));
        Ok(client)
    }

    fn set_activity(&mut self, activity: Value) -> std::io::Result<()> {
        let nonce = self.next_nonce();
        self.send_frame(
            1,
            &json!({
                "cmd": "SET_ACTIVITY",
                "args": { "pid": std::process::id(), "activity": activity },
                "nonce": nonce,
            }),
        )?;
        self.read_response(&nonce)
    }

    fn clear_activity(&mut self) -> std::io::Result<()> {
        let nonce = self.next_nonce();
        self.send_frame(
            1,
            &json!({
                "cmd": "SET_ACTIVITY",
                "args": { "pid": std::process::id() },
                "nonce": nonce,
            }),
        )?;
        self.read_response(&nonce)
    }

    fn read_response(&mut self, nonce: &str) -> std::io::Result<()> {
        for _ in 0..4 {
            let frame = self.read_frame()?;
            if frame.get("nonce").and_then(Value::as_str) == Some(nonce) {
                if frame.get("evt").and_then(Value::as_str) == Some("ERROR") {
                    return Err(std::io::Error::other("discord rpc error"));
                }
                return Ok(());
            }
        }
        Ok(())
    }

    fn next_nonce(&mut self) -> String {
        self.nonce += 1;
        format!("codex-rpc-{}-{}", std::process::id(), self.nonce)
    }

    fn send_frame(&mut self, opcode: u32, payload: &Value) -> std::io::Result<()> {
        let data = serde_json::to_vec(payload)?;
        self.connection.write_all(&opcode.to_le_bytes())?;
        self.connection
            .write_all(&(data.len() as u32).to_le_bytes())?;
        self.connection.write_all(&data)?;
        self.connection.flush()
    }

    fn read_frame(&mut self) -> std::io::Result<Value> {
        loop {
            let mut header = [0u8; 8];
            self.connection.read_exact(&mut header)?;
            let opcode = u32::from_le_bytes(header[0..4].try_into().unwrap());
            let len = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
            if len > 1024 * 1024 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "discord ipc frame too large",
                ));
            }
            let mut payload = vec![0u8; len];
            self.connection.read_exact(&mut payload)?;
            let value: Value = serde_json::from_slice(&payload)?;
            match opcode {
                1 => return Ok(value),
                2 => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionAborted,
                        "discord closed ipc",
                    ));
                }
                3 => {
                    let _ = self.send_frame(4, &value);
                }
                4 => {}
                _ => {}
            }
        }
    }
}

#[cfg(windows)]
fn connect_discord_ipc() -> std::io::Result<IpcConnection> {
    for id in 0..10 {
        let path = format!(r"\\?\pipe\discord-ipc-{id}");
        if let Ok(candidate) = OpenOptions::new().read(true).write(true).open(path) {
            return Ok(IpcConnection::File(candidate));
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "discord ipc",
    ))
}

#[cfg(unix)]
fn connect_discord_ipc() -> std::io::Result<IpcConnection> {
    for base in discord_ipc_roots() {
        for id in 0..10 {
            let path = base.join(format!("discord-ipc-{id}"));
            if let Ok(stream) = UnixStream::connect(path) {
                let _ = stream.set_read_timeout(Some(Duration::from_millis(IPC_READ_TIMEOUT_MS)));
                return Ok(IpcConnection::Unix(stream));
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "discord ipc",
    ))
}

#[cfg(unix)]
fn discord_ipc_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for name in ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"] {
        if let Some(path) = std::env::var_os(name).map(PathBuf::from) {
            push_unique_path(&mut roots, path);
        }
    }
    for path in ["/tmp", "/var/tmp", "/usr/tmp"] {
        push_unique_path(&mut roots, PathBuf::from(path));
    }
    roots
}

#[cfg(unix)]
fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

fn clean_label(value: &str) -> Option<String> {
    let cleaned = value
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.chars().take(32).collect())
    }
}

fn clean_url(value: &str) -> Option<String> {
    let value = value.trim();
    if value.starts_with("http://") || value.starts_with("https://") {
        Some(value.to_string())
    } else {
        None
    }
}

fn clean_status_line(line: &str) -> String {
    line.replace(['\r', '\n'], " ").chars().take(256).collect()
}

fn write_status(path: &Path, line: &str) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("txt.tmp");
    if fs::write(&tmp, clean_status_line(line)).is_ok() {
        let _ = fs::rename(tmp, path);
    }
}

fn clear_status(path: &Path) {
    let _ = fs::remove_file(path);
}

fn normalize_mode(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "watching" | "tv" => "watching",
        "listening" | "listen" => "listening",
        "competing" | "compete" => "competing",
        _ => "playing",
    }
    .into()
}

fn extract_toml_string(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?.trim_start();
    let value = rest.strip_prefix('=')?.trim();
    if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
        return Some(value[1..value.len() - 1].replace("\\\"", "\""));
    }
    if value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2 {
        return Some(value[1..value.len() - 1].to_string());
    }
    Some(value.to_string())
}

fn format_model(model: &str) -> Option<String> {
    sanitize_field(
        Some(
            &model
                .split('-')
                .enumerate()
                .map(|(i, segment)| {
                    if i == 0 && segment.chars().all(|ch| ch.is_ascii_lowercase()) {
                        segment.to_ascii_uppercase()
                    } else if segment
                        .chars()
                        .next()
                        .map(char::is_lowercase)
                        .unwrap_or(false)
                    {
                        let mut chars = segment.chars();
                        match chars.next() {
                            Some(first) => {
                                first.to_uppercase().collect::<String>() + chars.as_str()
                            }
                            None => String::new(),
                        }
                    } else {
                        segment.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("-"),
        ),
        24,
    )
}

fn format_effort(effort: &str) -> Option<String> {
    let label = match effort.to_ascii_lowercase().as_str() {
        "minimal" => "Minimal",
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" | "extra-high" => "Extra High",
        "max" => "Max",
        "ultra" => "Ultra",
        _ => effort,
    };
    sanitize_field(Some(label), 16)
}

fn format_speed(service_tier: Option<&str>) -> Option<String> {
    let label = match service_tier.map(|value| value.to_ascii_lowercase()) {
        Some(value) if value == "fast" || value == "priority" => "Fast",
        Some(value) if value == "standard" => "Standard",
        _ => "Standard",
    };
    sanitize_field(Some(label), 16)
}

fn sanitize_field(raw: Option<&str>, max_len: usize) -> Option<String> {
    let cleaned = raw?
        .chars()
        .filter(|ch| !ch.is_control() && !matches!(*ch as u32, 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2069 | 0xFEFF))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.is_empty() {
        None
    } else {
        Some(truncate(cleaned, max_len))
    }
}

fn sanitize_discord_user(raw: &str) -> Option<String> {
    sanitize_field(Some(raw), 32)
}

fn truncate(value: String, max_len: usize) -> String {
    if value.chars().count() <= max_len {
        return value;
    }
    let mut result = value
        .chars()
        .take(max_len.saturating_sub(3))
        .collect::<String>();
    result.push_str("...");
    result
}

fn small_image_key(state: PresenceState) -> &'static str {
    match state {
        PresenceState::Cli => "cli_badge",
        PresenceState::App => "app_badge",
        PresenceState::Both => "combo_badge",
        PresenceState::Idle => "codex_logo",
    }
}

fn small_image_text(state: PresenceState) -> &'static str {
    match state {
        PresenceState::Cli => "Codex CLI",
        PresenceState::App => "Codex Desktop",
        PresenceState::Both => "CLI + Desktop",
        PresenceState::Idle => "Codex",
    }
}

fn presence_key(result: &DetectionResult, settings: &RpcSettings) -> String {
    format!(
        "{:?}|{}",
        result,
        serde_json::to_string(settings).unwrap_or_default()
    )
}

fn modified_ms(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn min_option(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

/// Names a limit after the window Codex actually reports. Accounts without a 5h
/// window now receive the weekly limit in the `primary` slot, so the slot alone
/// can no longer name the row. `fallback` covers older rollout lines that carry
/// no window at all.
fn usage_label(limit: &LimitSnapshot, fallback: &str) -> String {
    match limit.window_minutes {
        Some(minutes) if minutes >= 7 * 24 * 60 => "week".into(),
        Some(minutes) if minutes >= 24 * 60 => format!("{}d", minutes / (24 * 60)),
        Some(minutes) if minutes >= 60 => format!("{}h", minutes / 60),
        Some(minutes) => format!("{minutes}m"),
        None => fallback.into(),
    }
}

fn remaining_percent(limit: &LimitSnapshot) -> i64 {
    if limit
        .resets_at_ms
        .map(|reset| reset <= now_ms() && limit.observed_at_ms < reset)
        .unwrap_or(false)
    {
        return 100;
    }
    (100.0 - limit.used_percent).max(0.0).round() as i64
}

fn parse_env_u64(name: &str, fallback: u64, min: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value >= min)
        .unwrap_or(fallback)
}

fn sleep_polling(stop: &AtomicBool, total_ms: u64) {
    let mut remaining = total_ms;
    while remaining > 0 && !stop.load(Ordering::SeqCst) {
        let chunk = remaining.min(200);
        thread::sleep(Duration::from_millis(chunk));
        remaining -= chunk;
    }
}

fn sessions_dir() -> PathBuf {
    home_dir().join(".codex").join("sessions")
}

fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn app_data_dir() -> PathBuf {
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local_app_data).join("codex-rich-presence");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("codex-rich-presence");
    }
    PathBuf::from(".").join("codex-rich-presence")
}

fn strip_windows_long_prefix(path: &str) -> &str {
    path.strip_prefix(r"\\?\").unwrap_or(path)
}

fn basename_safe(path: &str) -> String {
    let trimmed = path.trim_end_matches(['\\', '/']);
    trimmed
        .rsplit(['\\', '/'])
        .find(|part| !part.is_empty())
        .unwrap_or(trimmed)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_helpers_and_rpc_probes_are_not_interactive_cli_sessions() {
        let parents = HashMap::from([
            (1, (0, "ChatGPT.exe".into())),
            (2, (1, "powershell.exe".into())),
            (3, (0, "codex-rich-presence.exe".into())),
            (4, (3, "cmd.exe".into())),
        ]);
        let mut process = ProcessSnapshot {
            owner: process_owner(2, &parents),
            parent_name: Some("powershell.exe".into()),
            executable_path: Some(r"C:\npm\node_modules\@openai\codex\codex.exe".into()),
            creation_date_ms: None,
        };
        assert_eq!(classify_process(&process), PresenceState::App);
        process.owner = process_owner(4, &parents);
        assert_eq!(classify_process(&process), PresenceState::Idle);
        process.owner = None;
        assert_eq!(classify_process(&process), PresenceState::Cli);
        process.executable_path =
            Some(r"C:\Users\test\AppData\Local\OpenAI\Codex\bin\version\codex.exe".into());
        assert_eq!(classify_process(&process), PresenceState::App);
    }

    #[test]
    fn subscription_windows_and_discord_toggles_are_independent() {
        for plan in ["pro", "prolite", "plus"] {
            let payload = json!({"rateLimits": {"limitId":"codex", "planType":plan,
                "primary":{"usedPercent":20,"windowDurationMins":300},
                "secondary":{"usedPercent":11,"windowDurationMins":10080}}});
            let usage = parse_account_usage_payload(&payload, 0).unwrap();
            assert_eq!(usage.primary.is_some(), plan == "plus");
            assert_eq!(remaining_percent(usage.secondary.as_ref().unwrap()), 89);
            let mut result = DetectionResult {
                usage: Some(usage),
                ..DetectionResult::default()
            };
            let settings = RpcSettings {
                show_primary_usage: false,
                show_weekly_usage: false,
                ..RpcSettings::default()
            };
            assert!(format_status_line(&result, &settings, None).contains("week 89% left"));
            filter_usage(&mut result, &settings);
            assert!(result.usage.as_ref().unwrap().secondary.is_none());
            assert!(build_activity(&result, &settings).is_none());
        }
    }

    #[test]
    #[ignore = "Requires an installed Codex CLI and a signed-in account"]
    fn live_account_usage() {
        let usage = read_codex_account_usage_uncached().expect("Codex account usage unavailable");
        println!(
            "plan={:?} {}",
            usage.plan_type,
            format_usage(Some(&usage)).unwrap_or_default()
        );
        assert!(usage.primary.is_some() || usage.secondary.is_some());
    }

    #[test]
    fn state_and_status_include_fast_service_tier() {
        let result = DetectionResult {
            state: PresenceState::Cli,
            codex: Some(CodexConfig {
                model: Some("gpt-5.5".into()),
                effort: Some("high".into()),
                service_tier: Some("fast".into()),
            }),
            ..DetectionResult::default()
        };

        let settings = RpcSettings::default();

        assert_eq!(
            build_state_line(&result, &settings),
            "GPT-5.5 - High - Fast"
        );
        assert!(format_status_line(&result, &settings, None)
            .starts_with("Codex: CLI|GPT-5.5 - High - Fast|"));
    }

    #[test]
    fn project_config_overrides_top_level_service_tier() {
        let raw = r#"
model = "gpt-5.5"
model_reasoning_effort = "medium"
service_tier = "standard"

[projects.'d:\users\inerthel\documents\github\codex-rpc']
service_tier = "fast"
"#;
        let cfg = parse_codex_config(raw, Some(r"D:\Users\inerthel\Documents\GitHub\codex-rpc"));

        assert_eq!(cfg.model.as_deref(), Some("gpt-5.5"));
        assert_eq!(cfg.effort.as_deref(), Some("medium"));
        assert_eq!(cfg.service_tier.as_deref(), Some("fast"));
    }

    #[test]
    fn priority_service_tier_displays_as_fast() {
        assert_eq!(format_speed(Some("priority")).as_deref(), Some("Fast"));
    }

    #[test]
    fn state_and_status_include_standard_when_not_fast() {
        let result = DetectionResult {
            state: PresenceState::App,
            codex: Some(CodexConfig {
                model: Some("gpt-5.5".into()),
                effort: Some("high".into()),
                service_tier: None,
            }),
            ..DetectionResult::default()
        };
        let settings = RpcSettings::default();

        assert_eq!(
            build_state_line(&result, &settings),
            "GPT-5.5 - High - Standard"
        );
        assert!(format_status_line(&result, &settings, None)
            .starts_with("Codex: Desktop|GPT-5.5 - High - Standard|"));
    }

    #[test]
    fn parse_utc_timestamp_ms_matches_known_epoch() {
        assert_eq!(
            parse_utc_timestamp_ms("2026-07-09T18:00:59.694Z"),
            Some(1_783_620_059_694)
        );
        assert_eq!(parse_utc_timestamp_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_utc_timestamp_ms("not-a-date"), None);
    }

    #[test]
    fn turn_context_line_carries_timestamp() {
        let line = r#"{"timestamp":"2026-07-09T18:00:59.694Z","type":"turn_context","payload":{"model":"gpt-5.6-sol","effort":"ultra"}}"#;
        let (cfg, ts) = parse_turn_context_line(line).unwrap();
        assert_eq!(cfg.model.as_deref(), Some("gpt-5.6-sol"));
        assert_eq!(cfg.effort.as_deref(), Some("ultra"));
        assert_eq!(ts, Some(1_783_620_059_694));
    }

    #[test]
    fn usage_label_follows_the_reported_window() {
        let limit = |window_minutes| LimitSnapshot {
            used_percent: 11.0,
            window_minutes,
            resets_at_ms: None,
            observed_at_ms: 0,
        };
        assert_eq!(usage_label(&limit(Some(10080)), "5h"), "week");
        assert_eq!(usage_label(&limit(Some(300)), "week"), "5h");
        assert_eq!(usage_label(&limit(Some(2880)), "5h"), "2d");
        // Older rollout lines carry no window; the slot keeps naming the limit.
        assert_eq!(usage_label(&limit(None), "week"), "week");
    }

    #[test]
    fn weekly_only_limits_are_not_labelled_5h() {
        let line = r#"{"type":"event_msg","payload":{"type":"token_count","rate_limits":{"limit_id":"codex","primary":{"used_percent":11.0,"window_minutes":10080,"resets_at":null},"secondary":null}}}"#;
        let usage = parse_usage_line(line, 0).unwrap();
        assert_eq!(
            usage.secondary.as_ref().unwrap().window_minutes,
            Some(10080)
        );
        assert_eq!(format_usage(Some(&usage)).unwrap(), "Usage: week 89% left");
    }

    #[test]
    fn format_effort_maps_new_levels() {
        assert_eq!(format_effort("ultra").as_deref(), Some("Ultra"));
        assert_eq!(format_effort("max").as_deref(), Some("Max"));
    }
}
