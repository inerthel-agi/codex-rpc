#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod daemon;

use serde::{Deserialize, Serialize};
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

/// Structured view of the `state|model|usage|discord|plan` line written by the daemon.
#[derive(Debug, Clone, PartialEq, Serialize)]
struct AppStatus {
    state: String,
    model_parts: Vec<String>,
    credits: Option<String>,
    discord: String,
    plan: String,
    usage: Vec<UsageEntry>,
}

#[derive(Debug, Clone, Serialize)]
struct DaemonStatus {
    running: bool,
    pid: Option<u32>,
    error: Option<String>,
}

#[tauri::command]
fn load_settings() -> Result<RpcSettings, String> {
    let path = settings_path()?;
    match fs::read_to_string(&path) {
        Ok(raw) => Ok(normalize_settings(
            serde_json::from_str::<RpcSettings>(raw.trim_start_matches('\u{feff}'))
                .unwrap_or_default(),
        )),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(RpcSettings::default()),
        Err(err) => Err(err.to_string()),
    }
}

#[tauri::command]
fn save_settings(_app: tauri::AppHandle, settings: RpcSettings) -> Result<(), String> {
    let settings = normalize_settings(settings);
    write_settings(&settings)?;
    Ok(())
}

fn write_settings(settings: &RpcSettings) -> Result<(), String> {
    let path = settings_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }

    let json = serde_json::to_string_pretty(settings).map_err(|err| err.to_string())?;
    fs::write(path, json).map_err(|err| err.to_string())
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

    AppStatus {
        state: state.to_string(),
        model_parts,
        credits,
        discord,
        plan,
        usage: parse_status_usage(line),
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
        .manage(DaemonState::default())
        .manage(TrayAnchor::default())
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
            quit_app
        ])
        .setup(|app| {
            keep_window_in_tray(app);
            hide_tray_popup_on_blur(app);
            let handle = app.handle().clone();
            let state = app.state::<DaemonState>();
            start_daemon_inner(&handle, &state);
            create_tray(app)?;
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
    TrayIconBuilder::new()
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
    let size = tauri::LogicalSize::new(320.0, height.clamp(200.0, 600.0));
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
}

#[tauri::command]
fn tray_snapshot() -> Result<TraySnapshot, String> {
    Ok(TraySnapshot {
        status: parse_status_line(&read_status_line()),
        startup_label: startup_menu_label(),
        startup_enabled: startup_enabled(),
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

fn normalize_settings(mut settings: RpcSettings) -> RpcSettings {
    if settings.show_usage == Some(false) {
        settings.show_primary_usage = false;
        settings.show_weekly_usage = false;
        settings.show_credits = false;
    }
    settings.show_usage = None;

    settings.mode = match settings.mode.trim().to_ascii_lowercase().as_str() {
        "watching" | "tv" => "watching".into(),
        "listening" | "listen" => "listening".into(),
        "competing" | "compete" => "competing".into(),
        _ => "playing".into(),
    };

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
