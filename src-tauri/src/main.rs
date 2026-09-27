#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod daemon;
mod monitor;

use daemon::{RpcSettings, PAUSE_FOREVER};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tauri::{
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, PhysicalPosition, WindowEvent,
};

#[cfg(windows)]
const RUN_REGISTRY_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const RUN_REGISTRY_NAME: &str = "CodexRichPresence";
#[cfg(target_os = "macos")]
const MACOS_LAUNCH_AGENT_LABEL: &str = "io.github.inerthel-agi.codex-rich-presence";
/// Label used before 0.4.1; removed on the next startup toggle.
#[cfg(target_os = "macos")]
const MACOS_LEGACY_LAUNCH_AGENT_LABEL: &str = "eu.stealthylabs.codex-rich-presence";

#[derive(Default)]
struct DaemonState {
    running: Arc<Mutex<bool>>,
    error: Mutex<Option<String>>,
    stop: Arc<AtomicBool>,
    handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

#[derive(Default)]
struct TrayAnchor(Mutex<Option<PhysicalPosition<f64>>>);

/// Structured view of the `state|model|usage|discord|plan|presence|resets|since`
/// line written by the daemon.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct AppStatus {
    state: String,
    model_parts: Vec<String>,
    credits: Option<String>,
    discord: String,
    plan: String,
    usage: Vec<UsageEntry>,
    /// `live`, `paused`, `idle` or `off`: whether Discord currently shows the activity.
    presence: String,
    started_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
struct DaemonStatus {
    running: bool,
    pid: Option<u32>,
    error: Option<String>,
}

fn read_settings() -> Result<RpcSettings, String> {
    let path = settings_path()?;
    match fs::read_to_string(&path) {
        Ok(raw) => Ok(daemon::normalize_settings(
            serde_json::from_str::<RpcSettings>(raw.trim_start_matches('\u{feff}'))
                .unwrap_or_default(),
        )),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(RpcSettings::default()),
        Err(err) => Err(err.to_string()),
    }
}

/// Read-modify-write used by the tray, which only changes one field at a time.
fn update_settings(change: impl FnOnce(&mut RpcSettings)) -> Result<RpcSettings, String> {
    let _guard = settings_guard();
    let mut settings = read_settings()?;
    change(&mut settings);
    let settings = daemon::normalize_settings(settings);
    write_settings(&settings)?;
    Ok(settings)
}

#[tauri::command]
fn load_settings() -> Result<RpcSettings, String> {
    read_settings()
}

#[tauri::command]
fn save_settings(settings: RpcSettings) -> Result<(), String> {
    let mut settings = daemon::normalize_settings(settings);
    let _guard = settings_guard();
    // The pause belongs to the tray; a settings window opened earlier must not undo it.
    settings.paused_until_ms = read_settings()?.paused_until_ms;
    write_settings(&settings)
}

#[tauri::command]
fn pause_presence(minutes: u64) -> Result<u64, String> {
    let until = if minutes == 0 {
        PAUSE_FOREVER
    } else {
        now_ms().saturating_add(minutes.min(24 * 60) * 60_000)
    };
    update_settings(|settings| settings.paused_until_ms = until).map(|s| s.paused_until_ms)
}

#[tauri::command]
fn resume_presence() -> Result<(), String> {
    update_settings(|settings| settings.paused_until_ms = 0).map(|_| ())
}

#[tauri::command]
fn set_hide_model(enabled: bool) -> Result<bool, String> {
    update_settings(|settings| settings.hide_model = enabled).map(|s| s.hide_model)
}

#[tauri::command]
fn reconnect_discord() {
    daemon::request_reconnect();
}

#[derive(Debug, Clone, Serialize)]
struct AppInfo {
    version: &'static str,
    update: Option<String>,
}

#[tauri::command]
fn app_info(shared: tauri::State<'_, monitor::Shared>) -> AppInfo {
    AppInfo {
        version: env!("CARGO_PKG_VERSION"),
        update: shared.latest_version.lock().ok().and_then(|slot| slot.clone()),
    }
}

/// `[utc_hour, weekly_percent_used]` pairs for the last eight days.
#[tauri::command]
fn usage_history() -> Vec<(u64, f64)> {
    monitor::load_history().hours.into_iter().collect()
}

#[tauri::command]
fn open_codex() -> Result<(), String> {
    #[cfg(windows)]
    {
        let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
        // Codex Desktop ships as a Store package; the shell link also focuses a running window.
        let store = local
            .as_ref()
            .map(|dir| dir.join("Packages").join("OpenAI.Codex_2p2nqsd0c76g0"));
        if store.is_some_and(|dir| dir.is_dir()) {
            return open_with_shell(r"shell:AppsFolder\OpenAI.Codex_2p2nqsd0c76g0!App");
        }
        if let Some(exe) = local
            .map(|dir| dir.join("Programs").join("Codex").join("Codex.exe"))
            .filter(|exe| exe.is_file())
        {
            return std::process::Command::new(exe)
                .spawn()
                .map(|_| ())
                .map_err(|err| err.to_string());
        }
    }
    #[cfg(target_os = "macos")]
    if Path::new("/Applications/Codex.app").is_dir() {
        return open_with_shell("/Applications/Codex.app");
    }
    open_with_shell("https://chatgpt.com/codex")
}

/// Opens a terminal in the home folder and starts the Codex CLI in it.
#[tauri::command]
fn open_codex_cli() -> Result<(), String> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .filter(|dir| dir.is_dir());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        let cmd = Path::new(&root).join("System32").join("cmd.exe");
        let terminal = std::env::var_os("LOCALAPPDATA")
            .map(|dir| Path::new(&dir).join("Microsoft").join("WindowsApps").join("wt.exe"))
            .filter(|wt| wt.is_file());
        let mut command = match &terminal {
            Some(wt) => {
                let mut command = std::process::Command::new(wt);
                if let Some(home) = &home {
                    command.arg("-d").arg(home);
                }
                command.arg(&cmd);
                command
            }
            None => {
                let mut command = std::process::Command::new(&cmd);
                command.creation_flags(CREATE_NEW_CONSOLE);
                command
            }
        };
        // `/k` keeps the prompt open after Codex exits.
        command.args(["/k", "codex"]);
        if let Some(home) = &home {
            command.current_dir(home);
        }
        command.spawn().map(|_| ()).map_err(|err| err.to_string())
    }
    #[cfg(target_os = "macos")]
    {
        let _ = home;
        std::process::Command::new("/usr/bin/osascript")
            .args([
                "-e",
                r#"tell application "Terminal" to do script "codex""#,
                "-e",
                r#"tell application "Terminal" to activate"#,
            ])
            .spawn()
            .map(|_| ())
            .map_err(|err| err.to_string())
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let _ = home;
        Err("Opening a terminal is not supported on this platform".into())
    }
}

#[tauri::command]
fn open_release_page() -> Result<(), String> {
    open_with_shell("https://github.com/inerthel-agi/codex-rpc/releases/latest")
}

/// Opens one of the app's fixed links; the page only passes a key.
#[tauri::command]
fn open_link(kind: String) -> Result<(), String> {
    let url = match kind.as_str() {
        "repo" => "https://github.com/inerthel-agi/codex-rpc",
        "profile" => "https://github.com/inerthel-agi",
        "issues" => "https://github.com/inerthel-agi/codex-rpc/issues",
        _ => return Err("Unknown link".into()),
    };
    open_with_shell(url)
}

#[tauri::command]
fn open_data_folder() -> Result<(), String> {
    let dir = app_data_dir()?;
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    open_with_shell(&dir.to_string_lossy())
}

/// Hands a fixed URL, folder or shell link to the OS; never called with page input.
fn open_with_shell(target: &str) -> Result<(), String> {
    #[cfg(windows)]
    let mut command = {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        std::process::Command::new(Path::new(&root).join("explorer.exe"))
    };
    #[cfg(not(windows))]
    let mut command = std::process::Command::new("/usr/bin/open");
    command.arg(target).spawn().map(|_| ()).map_err(|err| err.to_string())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn write_settings(settings: &RpcSettings) -> Result<(), String> {
    let path = settings_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }

    let json = serde_json::to_string_pretty(settings).map_err(|err| err.to_string())?;
    // The daemon polls this file; a rename means it never reads a half-written copy.
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(|err| err.to_string())?;
    fs::rename(&tmp, &path).map_err(|err| err.to_string())
}

/// Serializes read-modify-write cycles between the tray and the settings window.
static SETTINGS_LOCK: Mutex<()> = Mutex::new(());

fn settings_guard() -> std::sync::MutexGuard<'static, ()> {
    SETTINGS_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn read_status_line() -> String {
    status_path()
        .ok()
        .and_then(|path| fs::read_to_string(path).ok())
        .map(|raw| raw.trim().to_string())
        .unwrap_or_default()
}

fn parse_status_line(line: &str) -> AppStatus {
    let mut parts = line.split('|').map(str::trim);
    let state = parts.next().filter(|value| !value.is_empty()).unwrap_or("Codex: Off");
    let model_parts = parts
        .next()
        .unwrap_or("")
        .split(" - ")
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect();
    let credits = parts.next().and_then(|usage| {
        usage
            .strip_prefix("Usage:")
            .unwrap_or(usage)
            .split('/')
            .map(str::trim)
            .find(|part| part.to_ascii_lowercase().starts_with("credits"))
            .map(str::to_string)
    });
    let discord = parts.next().unwrap_or("").to_string();
    let plan = parts.next().unwrap_or("").to_string();
    let presence = match parts.next().filter(|value| !value.is_empty()) {
        Some(value) => value.to_string(),
        None if state == "Codex: Off" => "off".into(),
        None => "live".into(),
    };
    let resets: Vec<(&str, u64)> = parts
        .next()
        .unwrap_or("")
        .split(',')
        .filter_map(|pair| {
            let (label, ms) = pair.split_once('=')?;
            Some((label.trim(), ms.trim().parse().ok()?))
        })
        .collect();
    let started_at_ms = parts.next().and_then(|value| value.parse().ok());
    let mut usage = parse_status_usage(line);
    for entry in &mut usage {
        entry.resets_at_ms = resets
            .iter()
            .find(|(label, _)| label.eq_ignore_ascii_case(&entry.label))
            .map(|(_, ms)| *ms);
    }

    AppStatus {
        state: state.to_string(),
        model_parts,
        credits,
        discord,
        plan,
        usage,
        presence,
        started_at_ms,
    }
}

#[tauri::command]
fn load_status() -> Result<AppStatus, String> {
    Ok(parse_status_line(&read_status_line()))
}

#[tauri::command]
fn start_daemon(
    app: tauri::AppHandle,
    state: tauri::State<'_, DaemonState>,
) -> Result<DaemonStatus, String> {
    start_daemon_inner(&app, &state);
    Ok(read_daemon_status(&state))
}

fn acquire_instance_lock(path: &Path) -> std::io::Result<Option<fs::File>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(error),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Keep the OS lock alive until the app exits; crashes release it too.
    let Some(_instance) = acquire_instance_lock(&app_data_dir()?.join("desktop-instance.lock"))?
    else {
        return Ok(());
    };
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .manage(DaemonState::default())
        .manage(TrayAnchor::default())
        .manage(monitor::Shared::default())
        .invoke_handler(tauri::generate_handler![
            load_settings,
            save_settings,
            load_status,
            start_daemon,
            close_settings,
            tray_snapshot,
            fit_tray,
            hide_tray,
            toggle_startup,
            open_settings_from_tray,
            quit_app,
            pause_presence,
            resume_presence,
            set_hide_model,
            reconnect_discord,
            app_info,
            usage_history,
            open_codex,
            open_codex_cli,
            open_release_page,
            open_data_folder,
            open_link
        ])
        .setup(|app| {
            keep_window_in_tray(app);
            hide_tray_popup_on_blur(app);
            let handle = app.handle().clone();
            let state = app.state::<DaemonState>();
            start_daemon_inner(&handle, &state);
            create_tray(app)?;
            monitor::spawn(handle);
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to run Codex RPC tray");
    Ok(())
}

fn keep_window_in_tray(app: &mut tauri::App) {
    if let Some(window) = app.get_webview_window("main") {
        let window_to_hide = window.clone();
        window.on_window_event(move |event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window_to_hide.hide();
            }
        });
    }
}

fn create_tray(app: &mut tauri::App) -> tauri::Result<()> {
    TrayIconBuilder::with_id(monitor::TRAY_ID)
        .tooltip("Codex RPC")
        .icon(app.default_window_icon().unwrap().clone())
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button,
                button_state: MouseButtonState::Up,
                position,
                ..
            } = event
            {
                match button {
                    MouseButton::Left => show_settings(tray.app_handle()),
                    MouseButton::Right => show_tray_popup(tray.app_handle(), position),
                    _ => {}
                }
            }
        })
        .build(app)?;

    Ok(())
}

fn hide_tray_popup_on_blur(app: &mut tauri::App) {
    if let Some(window) = app.get_webview_window("tray") {
        let window_to_hide = window.clone();
        window.on_window_event(move |event| {
            if let WindowEvent::Focused(false) = event {
                let _ = window_to_hide.hide();
            }
        });
    }
}

fn show_tray_popup(app: &tauri::AppHandle, cursor: PhysicalPosition<f64>) {
    *app.state::<TrayAnchor>().0.lock().unwrap() = Some(cursor);
    let Some(window) = app.get_webview_window("tray") else {
        return;
    };

    let size = window.outer_size().unwrap_or(tauri::PhysicalSize {
        width: 300,
        height: 380,
    });
    position_tray(app, cursor, size);
    let _ = window.show();
    let _ = window.set_focus();
}

fn position_tray(
    app: &tauri::AppHandle,
    cursor: PhysicalPosition<f64>,
    size: tauri::PhysicalSize<u32>,
) {
    let Some(window) = app.get_webview_window("tray") else {
        return;
    };
    let mut position = PhysicalPosition::new(
        cursor.x - size.width as f64,
        cursor.y - size.height as f64 - 8.0,
    );
    if let Ok(Some(monitor)) = app.monitor_from_point(cursor.x, cursor.y) {
        position = tray_position(cursor, size, *monitor.position(), *monitor.size());
    }
    let _ = window.set_position(position);
}

fn tray_position(
    cursor: PhysicalPosition<f64>,
    size: tauri::PhysicalSize<u32>,
    origin: PhysicalPosition<i32>,
    area: tauri::PhysicalSize<u32>,
) -> PhysicalPosition<f64> {
    PhysicalPosition::new(
        (cursor.x - size.width as f64).clamp(
            origin.x as f64,
            (origin.x as f64 + area.width as f64 - size.width as f64).max(origin.x as f64),
        ),
        (cursor.y - size.height as f64 - 8.0).clamp(
            origin.y as f64,
            (origin.y as f64 + area.height as f64 - size.height as f64).max(origin.y as f64),
        ),
    )
}

#[tauri::command]
fn fit_tray(app: tauri::AppHandle, height: f64) -> Result<(), String> {
    let window = app
        .get_webview_window("tray")
        .ok_or("Tray window unavailable")?;
    if !height.is_finite() {
        return Err("Invalid height".into());
    }
    let size = tauri::LogicalSize::new(320.0, height.clamp(200.0, 680.0));
    window.set_size(size).map_err(|err| err.to_string())?;
    if let Some(cursor) = *app.state::<TrayAnchor>().0.lock().unwrap() {
        position_tray(
            &app,
            cursor,
            size.to_physical(window.scale_factor().unwrap_or(1.0)),
        );
    }
    Ok(())
}

#[tauri::command]
fn hide_tray(app: tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("tray") {
        let _ = window.hide();
    }
}

#[derive(Debug, Clone, Serialize)]
struct TraySnapshot {
    #[serde(flatten)]
    status: AppStatus,
    startup_label: &'static str,
    startup_enabled: bool,
    paused_until_ms: u64,
    hide_model: bool,
    update: Option<String>,
}

#[tauri::command]
fn tray_snapshot(shared: tauri::State<'_, monitor::Shared>) -> Result<TraySnapshot, String> {
    let settings = read_settings().unwrap_or_default();
    Ok(TraySnapshot {
        status: parse_status_line(&read_status_line()),
        startup_label: startup_menu_label(),
        startup_enabled: startup_enabled(),
        paused_until_ms: settings.paused_until_ms,
        hide_model: settings.hide_model,
        update: shared.latest_version.lock().ok().and_then(|slot| slot.clone()),
    })
}

#[tauri::command]
fn toggle_startup() -> Result<bool, String> {
    set_startup_enabled(!startup_enabled())?;
    Ok(startup_enabled())
}

#[tauri::command]
fn open_settings_from_tray(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("tray") {
        let _ = window.hide();
    }
    show_settings(&app);
    Ok(())
}

#[tauri::command]
fn quit_app(app: tauri::AppHandle, state: tauri::State<'_, DaemonState>) -> Result<(), String> {
    stop_daemon(&state);
    app.exit(0);
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct UsageEntry {
    label: String,
    value: String,
    percent: f64,
    resets_at_ms: Option<u64>,
}

fn parse_status_usage(status_line: &str) -> Vec<UsageEntry> {
    // The daemon normalizes 5h/week windows before writing local status.
    let mut entries = Vec::new();
    let usage_field = match status_line.split('|').nth(2) {
        Some(field) if !field.trim().is_empty() => field.trim().to_string(),
        _ => return entries,
    };
    let body = usage_field
        .strip_prefix("Usage:")
        .map(str::trim)
        .unwrap_or(usage_field.as_str());
    for raw in body.split('/') {
        let part = raw.trim();
        if part.is_empty() || part.to_ascii_lowercase().starts_with("credits") {
            continue;
        }
        let head = part.strip_suffix("left").unwrap_or(part).trim_end();
        let Some((label, value)) = head.rsplit_once(' ') else {
            continue;
        };
        if !matches!(label.trim(), "5h" | "week") {
            continue;
        }
        let Ok(percent) = value.trim_end_matches('%').parse::<f64>() else {
            continue;
        };
        if !percent.is_finite() {
            continue;
        }
        entries.push(UsageEntry {
            label: capitalize(label.trim()),
            value: value.to_string(),
            percent: percent.clamp(0.0, 100.0),
            resets_at_ms: None,
        });
    }
    entries
}

fn capitalize(label: &str) -> String {
    let mut chars = label.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(windows)]
fn startup_menu_label() -> &'static str {
    "Start on Windows"
}

#[cfg(not(windows))]
fn startup_menu_label() -> &'static str {
    "Start at Login"
}

#[cfg(windows)]
fn startup_enabled() -> bool {
    reg_command()
        .args(["query", RUN_REGISTRY_KEY, "/v", RUN_REGISTRY_NAME])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn startup_enabled() -> bool {
    [MACOS_LAUNCH_AGENT_LABEL, MACOS_LEGACY_LAUNCH_AGENT_LABEL]
        .into_iter()
        .any(|label| launch_agent_path(label).map(|path| path.exists()).unwrap_or(false))
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn startup_enabled() -> bool {
    false
}

#[cfg(windows)]
fn set_startup_enabled(enabled: bool) -> Result<(), String> {
    let mut command = reg_command();
    if enabled {
        let exe = std::env::current_exe().map_err(|err| err.to_string())?;
        let startup_command = format!("\"{}\"", exe.to_string_lossy());
        command.args([
            "add",
            RUN_REGISTRY_KEY,
            "/v",
            RUN_REGISTRY_NAME,
            "/t",
            "REG_SZ",
            "/d",
            &startup_command,
            "/f",
        ]);
    } else {
        command.args(["delete", RUN_REGISTRY_KEY, "/v", RUN_REGISTRY_NAME, "/f"]);
    }

    let status = command.status().map_err(|err| err.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("reg.exe exited with {status}"))
    }
}

#[cfg(target_os = "macos")]
fn set_startup_enabled(enabled: bool) -> Result<(), String> {
    remove_launch_agent(&launch_agent_path(MACOS_LEGACY_LAUNCH_AGENT_LABEL)?)?;
    let path = launch_agent_path(MACOS_LAUNCH_AGENT_LABEL)?;
    if enabled {
        let exe = std::env::current_exe().map_err(|err| err.to_string())?;
        let exe = xml_escape(&exe.to_string_lossy());
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{MACOS_LAUNCH_AGENT_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
</dict>
</plist>
"#
        );
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        fs::write(path, plist).map_err(|err| err.to_string())
    } else {
        remove_launch_agent(&path)
    }
}

#[cfg(target_os = "macos")]
fn remove_launch_agent(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.to_string()),
    }
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn set_startup_enabled(_enabled: bool) -> Result<(), String> {
    Ok(())
}

#[cfg(windows)]
fn reg_command() -> std::process::Command {
    use std::ffi::OsString;
    use std::os::windows::process::CommandExt;

    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| OsString::from(r"C:\Windows"));
    let reg_path = Path::new(&system_root).join("System32").join("reg.exe");
    let mut command = std::process::Command::new(reg_path);
    command.creation_flags(0x08000000);
    command
}

#[cfg(target_os = "macos")]
fn launch_agent_path(label: &str) -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or_else(|| "HOME is not set".to_string())?;
    Ok(Path::new(&home)
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{label}.plist")))
}

#[cfg(target_os = "macos")]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn start_daemon_inner(_app: &tauri::AppHandle, state: &DaemonState) {
    let mut running = state.running.lock().expect("daemon state mutex poisoned");
    if *running {
        return;
    }

    state.stop.store(false, Ordering::SeqCst);
    *state.error.lock().expect("daemon error mutex poisoned") = None;
    *running = true;

    let stop = Arc::clone(&state.stop);
    let running_flag = Arc::clone(&state.running);
    let status_path = status_path().ok();
    let settings_path = settings_path().ok();
    if let Some(handle) = state
        .handle
        .lock()
        .expect("daemon handle mutex poisoned")
        .take()
    {
        let _ = handle.join();
    }
    let handle = std::thread::spawn(move || {
        daemon::run(stop, settings_path, status_path);
        if let Ok(mut running) = running_flag.lock() {
            *running = false;
        }
    });
    *state.handle.lock().expect("daemon handle mutex poisoned") = Some(handle);
}

fn stop_daemon(state: &DaemonState) {
    state.stop.store(true, Ordering::SeqCst);
    if let Some(handle) = state
        .handle
        .lock()
        .expect("daemon handle mutex poisoned")
        .take()
    {
        let _ = handle.join();
    }
}

fn read_daemon_status(state: &DaemonState) -> DaemonStatus {
    let running = *state.running.lock().expect("daemon state mutex poisoned");
    let error = state
        .error
        .lock()
        .expect("daemon error mutex poisoned")
        .clone();

    DaemonStatus {
        running,
        pid: if running {
            Some(std::process::id())
        } else {
            None
        },
        error,
    }
}

#[tauri::command]
fn close_settings(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        window.hide().map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn show_settings(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn settings_path() -> Result<PathBuf, String> {
    Ok(app_data_dir()?.join("rpc-buttons.json"))
}

fn status_path() -> Result<PathBuf, String> {
    Ok(app_data_dir()?.join("status.txt"))
}

fn app_data_dir() -> Result<PathBuf, String> {
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        return Ok(Path::new(&local_app_data).join("codex-rich-presence"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        return Ok(Path::new(&home)
            .join("Library")
            .join("Application Support")
            .join("codex-rich-presence"));
    }
    std::env::current_dir()
        .map(|path| path.join("codex-rich-presence"))
        .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_lock_rejects_duplicates_and_releases_on_drop() {
        let path = std::env::temp_dir().join(format!(
            "codex-rpc-instance-test-{}.lock",
            std::process::id()
        ));
        let first = acquire_instance_lock(&path).unwrap().unwrap();
        assert!(acquire_instance_lock(&path).unwrap().is_none());
        drop(first);
        let next = acquire_instance_lock(&path).unwrap().unwrap();
        drop(next);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn tray_stays_anchored_after_resizing_on_scaled_and_negative_monitors() {
        let origin = PhysicalPosition::new(-1920, 0);
        let area = tauri::PhysicalSize::new(1920, 1080);
        let cursor = PhysicalPosition::new(-50.0, 1040.0);
        for height in [300, 400, 500] {
            let size = tauri::LogicalSize::new(320.0, height as f64).to_physical(1.5);
            let position = tray_position(cursor, size, origin, area);
            assert_eq!(position.y + size.height as f64, cursor.y - 8.0);
            assert!(position.x >= origin.x as f64);
            assert!(position.x + size.width as f64 <= 0.0);
        }
        let position = tray_position(
            PhysicalPosition::new(5.0, 5.0),
            tauri::PhysicalSize::new(320, 300),
            PhysicalPosition::new(0, 0),
            area,
        );
        assert_eq!(position, PhysicalPosition::new(0.0, 0.0));
    }

    #[test]
    fn status_usage_yields_one_entry_per_reported_limit() {
        let line = "Codex: Desktop|GPT-5.6-Sol - Max|Usage: week 89% left / Spark week 100% left / credits 0|Discord: Connected (user)|";
        let entries = parse_status_usage(line);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label, "Week");
        assert_eq!(entries[0].value, "89%");
        assert_eq!(entries[0].percent, 89.0);
    }

    #[test]
    fn status_usage_keeps_short_windows_when_the_plan_has_them() {
        let entries = parse_status_usage("Codex: CLI|GPT-5|Usage: 5h 42% left / week 7% left|");
        let labels: Vec<&str> = entries.iter().map(|entry| entry.label.as_str()).collect();
        assert_eq!(labels, ["5h", "Week"]);
    }

    #[test]
    fn status_usage_is_empty_without_a_usage_field() {
        assert!(parse_status_usage("Codex: Off||").is_empty());
        assert!(parse_status_usage("").is_empty());
    }

    #[test]
    fn status_line_splits_every_field() {
        let status = parse_status_line(
            "Codex: Desktop|GPT-6-Astra - Medium - Fast|Usage: week 28% left / credits 12|Discord: Connected (user)|pro",
        );
        assert_eq!(status.state, "Codex: Desktop");
        assert_eq!(status.model_parts, ["GPT-6-Astra", "Medium", "Fast"]);
        assert_eq!(status.credits.as_deref(), Some("credits 12"));
        assert_eq!(status.discord, "Discord: Connected (user)");
        assert_eq!(status.plan, "pro");
        assert_eq!(status.usage.len(), 1);
    }

    #[test]
    fn status_line_without_credits_or_model() {
        let status = parse_status_line("Codex: CLI||Usage: 5h 42% left|Discord: Not connected|plus");
        assert!(status.model_parts.is_empty());
        assert_eq!(status.credits, None);
        assert_eq!(status.plan, "plus");
    }

    #[test]
    fn status_line_extras_attach_resets_presence_and_start() {
        let status = parse_status_line(
            "Codex: CLI|GPT-5|Usage: 5h 42% left / week 7% left|Discord: Not connected|plus|paused|5h=1700,week=9000|1234",
        );
        assert_eq!(status.presence, "paused");
        assert_eq!(status.started_at_ms, Some(1234));
        let resets: Vec<Option<u64>> = status.usage.iter().map(|e| e.resets_at_ms).collect();
        assert_eq!(resets, [Some(1700), Some(9000)]);
        // Lines written before 0.5 have no extra fields.
        let old = parse_status_line("Codex: CLI||Usage: week 7% left|Discord: Not connected|plus");
        assert_eq!(old.presence, "live");
        assert_eq!(old.usage[0].resets_at_ms, None);
    }

    #[test]
    fn empty_status_line_means_codex_is_off() {
        for line in ["", "Codex: Off||||"] {
            let status = parse_status_line(line);
            assert_eq!(status.state, "Codex: Off");
            assert!(status.model_parts.is_empty());
            assert!(status.usage.is_empty());
            assert_eq!(status.credits, None);
        }
    }
}
