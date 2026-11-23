#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod daemon;
mod monitor;
#[cfg(windows)]
mod panel;
mod view;

slint::include_modules!();

use daemon::{RpcButton, RpcSettings, PAUSE_FOREVER};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{
    cell::RefCell,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

/// Launch-agent label on macOS and notification identity on Windows.
const APP_ID: &str = "io.github.inerthel-agi.codex-rich-presence";
#[cfg(windows)]
const RUN_REGISTRY_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const RUN_REGISTRY_NAME: &str = "CodexRichPresence";
/// Label used before 0.4.1; removed on the next startup toggle.
#[cfg(target_os = "macos")]
const MACOS_LEGACY_LAUNCH_AGENT_LABEL: &str = "eu.stealthylabs.codex-rich-presence";

/// Structured view of the `state|model|usage|discord|plan|presence|resets|since`
/// line written by the daemon.
#[derive(Debug, Clone, PartialEq)]
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
    /// Folder name of the Codex thread; Discord shows it unless the model is hidden.
    project: String,
}

#[derive(Debug, Clone, PartialEq)]
struct UsageEntry {
    label: String,
    value: String,
    percent: f64,
    resets_at_ms: Option<u64>,
}

// ---------- Settings file ----------

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

fn save_settings(settings: RpcSettings) -> Result<(), String> {
    let mut settings = daemon::normalize_settings(settings);
    let _guard = settings_guard();
    // The pause belongs to the tray; a settings window opened earlier must not undo it.
    settings.paused_until_ms = read_settings()?.paused_until_ms;
    write_settings(&settings)
}

fn pause_presence(minutes: u64) -> Result<RpcSettings, String> {
    let until = if minutes == 0 {
        PAUSE_FOREVER
    } else {
        now_ms().saturating_add(minutes.min(24 * 60) * 60_000)
    };
    update_settings(|settings| settings.paused_until_ms = until)
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
    SETTINGS_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `dark`, `system` or `light`; kept apart from the settings the daemon watches.
fn read_theme() -> String {
    let stored = app_data_dir()
        .ok()
        .and_then(|dir| fs::read_to_string(dir.join("theme.txt")).ok())
        .unwrap_or_default();
    match stored.trim() {
        mode @ ("oled" | "system" | "light") => mode.into(),
        _ => "dark".into(),
    }
}

fn write_theme(mode: &str) {
    if let Ok(dir) = app_data_dir() {
        let _ = fs::write(dir.join("theme.txt"), mode);
    }
}

// ---------- Actions ----------

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
            .map(|dir| {
                Path::new(&dir)
                    .join("Microsoft")
                    .join("WindowsApps")
                    .join("wt.exe")
            })
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

/// Opens one of the app's fixed links; the interface only passes a key.
fn open_link(kind: &str) -> Result<(), String> {
    let url = match kind {
        "repo" => "https://github.com/inerthel-agi/codex-rpc",
        "profile" => "https://github.com/inerthel-agi",
        "issues" => "https://github.com/inerthel-agi/codex-rpc/issues",
        "release" => "https://github.com/inerthel-agi/codex-rpc/releases/latest",
        "slint" => "https://slint.dev",
        _ => return Err("Unknown link".into()),
    };
    open_with_shell(url)
}

fn open_data_folder() -> Result<(), String> {
    let dir = app_data_dir()?;
    fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
    open_with_shell(&dir.to_string_lossy())
}

/// Hands a fixed URL, folder or shell link to the OS; never called with user input.
fn open_with_shell(target: &str) -> Result<(), String> {
    #[cfg(windows)]
    let mut command = {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        std::process::Command::new(Path::new(&root).join("explorer.exe"))
    };
    #[cfg(not(windows))]
    let mut command = std::process::Command::new("/usr/bin/open");
    command
        .arg(target)
        .spawn()
        .map(|_| ())
        .map_err(|err| err.to_string())
}

/// Returns whether the system accepted the notification.
pub(crate) fn notify(title: &str, body: &str) -> bool {
    let mut notification = notify_rust::Notification::new();
    notification.summary(title).body(body);
    // Windows only attributes a toast to the app once an installed shortcut
    // carries its id; a build run from cargo's target folder goes without.
    #[cfg(windows)]
    if !std::env::current_exe()
        .is_ok_and(|exe| exe.components().any(|part| part.as_os_str() == "target"))
    {
        notification.app_id(APP_ID);
    }
    notification.show().is_ok()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ---------- Status line ----------

fn read_status_line() -> String {
    status_path()
        .ok()
        .and_then(|path| fs::read_to_string(path).ok())
        .map(|raw| raw.trim().to_string())
        .unwrap_or_default()
}

fn parse_status_line(line: &str) -> AppStatus {
    let mut parts = line.split('|').map(str::trim);
    let state = parts
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or("Codex: Off");
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
    let project = parts.next().unwrap_or("").to_string();
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
        project,
    }
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

// ---------- Start with the system ----------

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
    [APP_ID, MACOS_LEGACY_LAUNCH_AGENT_LABEL]
        .into_iter()
        .any(|label| {
            launch_agent_path(label)
                .map(|path| path.exists())
                .unwrap_or(false)
        })
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
    let path = launch_agent_path(APP_ID)?;
    if enabled {
        let exe = std::env::current_exe().map_err(|err| err.to_string())?;
        let exe = xml_escape(&exe.to_string_lossy());
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{APP_ID}</string>
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

// ---------- Tray ----------
// Windows draws its own panel (panel.rs); other platforms use Slint's tray icon with a native menu.

thread_local! {
    /// These live on the event-loop thread; other threads reach them through
    /// `slint::invoke_from_event_loop`.
    #[cfg(not(windows))]
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
    static SETTINGS: RefCell<Option<SettingsUi>> = const { RefCell::new(None) };
}

/// Event-loop thread only, like the two functions below.
pub(crate) fn set_tray_icon(rgba: Vec<u8>, width: u32, height: u32) {
    #[cfg(windows)]
    panel::set_icon(&rgba, width, height);
    #[cfg(not(windows))]
    with_tray(|tray| {
        tray.set_image(slint::Image::from_rgba8(
            slint::SharedPixelBuffer::clone_from_slice(&rgba, width, height),
        ))
    });
}

pub(crate) fn set_tray_tip(tip: String) {
    #[cfg(windows)]
    panel::set_tip(&tip);
    #[cfg(not(windows))]
    with_tray(|tray| tray.set_tip(tip.into()));
}

/// The open panel refreshes itself; only the native menu needs this.
#[cfg(not(windows))]
pub(crate) fn set_tray_menu(model: view::TrayModel) {
    with_tray(|tray| apply_tray(tray, &model));
}

/// Shows a tray action's effect right away instead of at the monitor's next tick.
fn tray_changed(settings: &RpcSettings) {
    #[cfg(windows)]
    {
        let _ = settings;
        panel::refresh();
    }
    #[cfg(not(windows))]
    refresh_tray(settings);
}

// Actions shared by the native menu and the Windows panel.
fn pause(minutes: u64) {
    if let Ok(settings) = pause_presence(minutes) {
        tray_changed(&settings);
    }
}

fn resume() {
    if let Ok(settings) = update_settings(|settings| settings.paused_until_ms = 0) {
        tray_changed(&settings);
    }
}

fn set_hide_model(enabled: bool) {
    if let Ok(settings) = update_settings(|settings| settings.hide_model = enabled) {
        tray_changed(&settings);
    }
    // An open settings window shows the same switch.
    with_settings(|ui| {
        ui.settings.hide_model = enabled;
        ui.window.global::<App>().set_hide_model(enabled);
        ui.refresh();
    });
}

fn quit() {
    let _ = slint::quit_event_loop();
}

#[cfg(not(windows))]
pub(crate) fn with_tray(update: impl FnOnce(&Tray)) {
    TRAY.with(|slot| {
        if let Some(tray) = slot.borrow().as_ref() {
            update(tray);
        }
    });
}

#[cfg(not(windows))]
pub(crate) fn apply_tray(tray: &Tray, model: &view::TrayModel) {
    tray.set_headline(model.headline.as_str().into());
    tray.set_facts(strings(&model.facts));
    tray.set_paused(model.paused);
    tray.set_pause_label(model.pause_label.as_str().into());
    tray.set_hide_model(model.hide_model);
    tray.set_update(model.update.as_str().into());
    tray.set_discord(model.discord.as_str().into());
    tray.set_can_reconnect(model.can_reconnect);
}

#[cfg(not(windows))]
fn refresh_tray(settings: &RpcSettings) {
    let model = view::tray_model(
        &parse_status_line(&read_status_line()),
        settings,
        monitor::latest_version(),
        now_ms(),
    );
    with_tray(|tray| apply_tray(tray, &model));
}

fn strings(values: &[String]) -> ModelRc<slint::SharedString> {
    ModelRc::new(VecModel::from(
        values
            .iter()
            .map(|value| value.as_str().into())
            .collect::<Vec<slint::SharedString>>(),
    ))
}

#[cfg(not(windows))]
fn wire_tray(tray: &Tray) {
    tray.on_open_settings(open_settings);
    tray.on_pause(|minutes| pause(minutes.max(0) as u64));
    tray.on_resume(resume);
    tray.on_toggle_hide(|| {
        let mut enabled = false;
        with_tray(|tray| enabled = tray.get_hide_model());
        set_hide_model(enabled);
    });
    tray.on_open_codex(|| {
        let _ = open_codex();
    });
    tray.on_open_cli(|| {
        let _ = open_codex_cli();
    });
    tray.on_open_update(|| {
        let _ = open_link("release");
    });
    tray.on_reconnect(daemon::request_reconnect);
    tray.on_quit(quit);
}

/// Colours, motion and fonts for a newly created window.
fn apply_theme(theme: &Theme) {
    theme.set_mode(read_theme().into());
    theme.set_motion(animations_enabled());
    theme.set_mono(mono_font().into());
}

/// Gives the window's title bar and border the theme's colours (title bar colour
/// needs Windows 11; Windows 10 only switches it between light and dark), so
/// the frame matches the app instead of the Windows theme.
/// Returns the colours applied, to repaint only when the theme changes.
#[cfg(windows)]
fn paint_frame(window: &slint::Window, theme: &Theme) -> [slint::Color; 3] {
    use slint::winit_030::winit::{platform::windows::WindowExtWindows, window::Theme as Frame};
    use slint::winit_030::{winit::platform::windows::Color, WinitWindowAccessor};
    let colors = [theme.get_panel(), theme.get_text(), theme.get_frame()];
    let rgb = |color: slint::Color| Color::from_rgb(color.red(), color.green(), color.blue());
    let dark = theme.get_dark();
    window.with_winit_window(|frame| {
        // Light or dark caption buttons, which keep their own colours.
        frame.set_theme(Some(if dark { Frame::Dark } else { Frame::Light }));
        frame.set_title_background_color(Some(rgb(colors[0])));
        frame.set_title_text_color(rgb(colors[1]));
        frame.set_border_color(Some(rgb(colors[2])));
        hide_caption(frame);
    });
    colors
}

/// Leaves the title bar empty: the sidebar already shows the name and the logo.
/// The window keeps both for the taskbar and Alt+Tab.
#[cfg(windows)]
fn hide_caption(frame: &slint::winit_030::winit::window::Window) {
    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::Controls::{
        SetWindowThemeAttribute, WTA_NONCLIENT, WTA_OPTIONS, WTNCA_NODRAWCAPTION, WTNCA_NODRAWICON,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        SendMessageW, ICON_SMALL, WM_GETICON, WM_SETICON,
    };
    let Ok(RawWindowHandle::Win32(handle)) = frame.window_handle().map(|handle| handle.as_raw())
    else {
        return;
    };
    let window = HWND(handle.hwnd.get() as _);
    let flags = WTNCA_NODRAWCAPTION | WTNCA_NODRAWICON;
    let options = WTA_OPTIONS {
        dwFlags: flags,
        dwMask: flags,
    };
    let small = Some(WPARAM(ICON_SMALL as usize));
    unsafe {
        let _ = SetWindowThemeAttribute(
            window,
            WTA_NONCLIENT,
            (&raw const options).cast(),
            size_of::<WTA_OPTIONS>() as u32,
        );
        // Windows 11 keeps drawing an icon set before the call until that icon
        // changes: take it off and put it straight back.
        let icon = SendMessageW(window, WM_GETICON, small, None);
        SendMessageW(window, WM_SETICON, small, None);
        SendMessageW(window, WM_SETICON, small, Some(LPARAM(icon.0)));
    }
}

#[cfg(not(windows))]
fn paint_frame(_window: &slint::Window, theme: &Theme) -> [slint::Color; 3] {
    [theme.get_panel(), theme.get_text(), theme.get_frame()]
}

// ---------- Settings window ----------

/// The settings window exists only while it is open; closing it frees its memory.
struct SettingsUi {
    window: SettingsWindow,
    /// What the form currently holds; fields without a control keep their saved value.
    settings: RpcSettings,
    dirty: bool,
    shown: Option<view::StatusView>,
    ticks: u32,
    /// Colours last given to the title bar; "System" can change them while the window is open.
    frame: Option<[slint::Color; 3]>,
    save: slint::Timer,
    _tick: slint::Timer,
}

fn fill_form(app: &App, settings: &RpcSettings) {
    let button = |index: usize| {
        settings
            .buttons
            .get(index)
            .map(|button| (button.label.clone(), button.url.clone()))
            .unwrap_or_default()
    };
    app.set_mode(settings.mode.as_str().into());
    app.set_show_5h(settings.show_primary_usage);
    app.set_show_week(settings.show_weekly_usage);
    app.set_show_effort(settings.show_effort);
    app.set_show_fast(settings.show_fast_mode);
    app.set_show_credits(settings.show_credits);
    app.set_show_plan(settings.show_plan);
    app.set_hide_model(settings.hide_model);
    app.set_show_elapsed(settings.show_elapsed);
    app.set_idle_minutes(settings.idle_clear_minutes as i32);
    app.set_template(settings.custom_state.as_str().into());
    app.set_label0(button(0).0.into());
    app.set_url0(button(0).1.into());
    app.set_label1(button(1).0.into());
    app.set_url1(button(1).1.into());
    app.set_notify_low(settings.notify_low);
    app.set_notify_reset(settings.notify_reset);
    app.set_check_updates(settings.check_updates);
}

/// Copies the form into `settings`; fields without a control keep their value.
fn read_form(app: &App, settings: &mut RpcSettings) {
    settings.mode = app.get_mode().into();
    settings.show_primary_usage = app.get_show_5h();
    settings.show_weekly_usage = app.get_show_week();
    settings.show_effort = app.get_show_effort();
    settings.show_fast_mode = app.get_show_fast();
    settings.show_credits = app.get_show_credits();
    settings.show_plan = app.get_show_plan();
    settings.hide_model = app.get_hide_model();
    settings.show_elapsed = app.get_show_elapsed();
    settings.idle_clear_minutes = app.get_idle_minutes().max(0) as u64;
    settings.custom_state = app.get_template().into();
    settings.notify_low = app.get_notify_low();
    settings.notify_reset = app.get_notify_reset();
    settings.check_updates = app.get_check_updates();
    settings.buttons = [
        (app.get_label0(), app.get_url0()),
        (app.get_label1(), app.get_url1()),
    ]
    .into_iter()
    .map(|(label, url)| RpcButton {
        label: label.trim().into(),
        url: url.trim().into(),
    })
    .filter(|button| !button.label.is_empty() && !button.url.is_empty())
    .collect();
}

fn with_settings(update: impl FnOnce(&mut SettingsUi)) {
    SETTINGS.with(|slot| {
        if let Some(ui) = slot.borrow_mut().as_mut() {
            update(ui);
        }
    });
}

impl SettingsUi {
    fn message(&self, text: &str, saved: bool) {
        let app = self.window.global::<App>();
        app.set_message(text.into());
        app.set_saved(saved);
    }

    /// Status, limits and the Discord preview.
    fn refresh(&mut self) {
        let now = now_ms();
        let status = parse_status_line(&read_status_line());
        let app = self.window.global::<App>();
        let view = view::StatusView::new(&status, &self.settings, now);
        if self.shown.as_ref() != Some(&view) {
            app.set_active(view.active);
            app.set_source_label(view.source_label.into());
            app.set_plan_label(view.plan_label.as_str().into());
            app.set_has_plan(view.has_plan);
            app.set_pro(view.pro);
            app.set_discord_status(view.discord_status.as_str().into());
            app.set_limits(ModelRc::new(VecModel::from(
                view.limits
                    .iter()
                    .map(|limit| Limit {
                        figure: limit.figure.as_str().into(),
                        caption: limit.caption.as_str().into(),
                        percent: limit.percent,
                        level: limit.level,
                        reset_in: limit.reset_in.as_str().into(),
                        reset_date: limit.reset_date.as_str().into(),
                    })
                    .collect::<Vec<_>>(),
            )));
            app.set_credits(view.credits.as_str().into());
            app.set_dc_activity(view.dc_activity.into());
            app.set_dc_details(view.dc_details.as_str().into());
            app.set_dc_state(view.dc_state.as_str().into());
            app.set_dc_note(view.dc_note.into());
            app.set_dc_buttons(strings(&view.dc_buttons));
            self.shown = Some(view);
        }
        app.set_dc_time(view::elapsed(&status, &self.settings, now).into());
        app.set_update_version(monitor::latest_version().unwrap_or_default().into());
    }

    fn refresh_history(&self) {
        let app = self.window.global::<App>();
        let history = monitor::load_history();
        let days = view::week_days(&history.hours, now_ms(), view::local_offset_ms());
        let busiest = days.iter().map(|(_, used)| *used).fold(1.0, f64::max);
        let percent = |used: f64| {
            if history.hours.is_empty() {
                "—".to_string()
            } else {
                format!("{}%", used.round())
            }
        };
        app.set_has_history(!history.hours.is_empty());
        app.set_used_today(percent(days.last().map_or(0.0, |(_, used)| *used)).into());
        app.set_used_week(percent(days.iter().map(|(_, used)| *used).sum()).into());
        app.set_days(ModelRc::new(VecModel::from(
            days.iter()
                .enumerate()
                .map(|(index, (name, used))| Day {
                    name: (*name).into(),
                    share: (used / busiest) as f32,
                    today: index == days.len() - 1,
                })
                .collect::<Vec<_>>(),
        )));
    }

    fn refresh_startup(&self) {
        let app = self.window.global::<App>();
        app.set_startup_label(startup_menu_label().into());
        app.set_startup_enabled(startup_enabled());
    }

    fn save_now(&mut self) {
        self.save.stop();
        if !self.dirty {
            return;
        }
        self.dirty = false;
        match save_settings(self.settings.clone()) {
            Ok(()) => self.message(
                &format!(
                    "Saved · {}",
                    view::local_clock(now_ms(), view::local_offset_ms())
                ),
                true,
            ),
            Err(error) => self.message(&format!("Could not save: {error}"), false),
        }
    }

    fn repaint_frame(&mut self) {
        let theme = self.window.global::<Theme>();
        let wanted = [theme.get_panel(), theme.get_text(), theme.get_frame()];
        if self.frame != Some(wanted) {
            self.frame = Some(paint_frame(self.window.window(), &theme));
        }
    }

    fn tick(&mut self) {
        self.ticks += 1;
        self.repaint_frame();
        // The tray can flip this switch while the window stays open.
        if !self.dirty && self.ticks.is_multiple_of(2) {
            if let Ok(fresh) = read_settings() {
                if fresh.hide_model != self.settings.hide_model {
                    self.settings.hide_model = fresh.hide_model;
                    self.window.global::<App>().set_hide_model(fresh.hide_model);
                }
            }
        }
        self.refresh();
        if self.ticks.is_multiple_of(60) {
            self.refresh_history();
        }
    }
}

fn open_settings() {
    let open = SETTINGS.with(|slot| slot.borrow().as_ref().map(|ui| ui.window.clone_strong()));
    if let Some(window) = open {
        // Showing it again is what brings an already open window to the front.
        let _ = window.hide();
        let _ = window.show();
        return;
    }
    let Ok(window) = SettingsWindow::new() else {
        return;
    };
    apply_theme(&window.global::<Theme>());
    let app = window.global::<App>();
    app.set_version(env!("CARGO_PKG_VERSION").into());
    app.on_changed(|| {
        with_settings(|ui| {
            read_form(&ui.window.global::<App>(), &mut ui.settings);
            ui.dirty = true;
            ui.message("Saving…", false);
            ui.save.start(
                slint::TimerMode::SingleShot,
                Duration::from_millis(300),
                || with_settings(SettingsUi::save_now),
            );
            ui.refresh();
        });
    });
    app.on_set_theme(|mode| {
        write_theme(&mode);
        with_settings(|ui| {
            ui.window.global::<Theme>().set_mode(mode);
            ui.repaint_frame();
        });
    });
    app.on_toggle_startup(|| {
        let result = set_startup_enabled(!startup_enabled());
        with_settings(|ui| {
            if let Err(error) = &result {
                ui.message(error, false);
            }
            ui.refresh_startup();
        });
    });
    app.on_reconnect(|| {
        daemon::request_reconnect();
        with_settings(|ui| ui.message("Reconnecting to Discord…", true));
    });
    app.on_open_data(|| report(open_data_folder()));
    app.on_open_link(|kind| report(open_link(&kind)));
    app.on_close(close_settings);
    window.window().on_close_requested(|| {
        close_settings();
        slint::CloseRequestResponse::HideWindow
    });

    let tick = slint::Timer::default();
    tick.start(slint::TimerMode::Repeated, Duration::from_secs(1), || {
        with_settings(SettingsUi::tick)
    });
    let mut ui = SettingsUi {
        window,
        settings: read_settings().unwrap_or_default(),
        dirty: false,
        shown: None,
        ticks: 0,
        frame: None,
        save: slint::Timer::default(),
        _tick: tick,
    };
    fill_form(&ui.window.global::<App>(), &ui.settings);
    ui.refresh();
    ui.refresh_history();
    ui.refresh_startup();
    let _ = ui.window.show();
    // Slint creates the real window when this event-loop turn ends: painting the
    // title bar before that does nothing and leaves it in the Windows colours.
    let weak = ui.window.as_weak();
    let _ = slint::spawn_local(async move {
        use slint::winit_030::WinitWindowAccessor;
        let Some(window) = weak.upgrade() else {
            return;
        };
        if window.window().winit_window().await.is_ok() {
            with_settings(SettingsUi::repaint_frame);
        }
    });
    SETTINGS.with(|slot| *slot.borrow_mut() = Some(ui));
}

/// Cascadia Mono ships with Windows 11; older Windows has Consolas.
fn mono_font() -> &'static str {
    if !cfg!(windows) {
        return "Menlo";
    }
    let fonts = std::env::var_os("SystemRoot").map(|root| Path::new(&root).join("Fonts"));
    if fonts.is_some_and(|dir| dir.join("CascadiaMono.ttf").is_file()) {
        "Cascadia Mono"
    } else {
        "Consolas"
    }
}

/// Follows Windows' "Animation effects" and macOS' "Reduce motion" settings.
#[cfg(windows)]
fn animations_enabled() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{
        SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    };
    let mut enabled: i32 = 1;
    let read = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(&mut enabled as *mut i32 as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    read.is_err() || enabled != 0
}

#[cfg(not(windows))]
fn animations_enabled() -> bool {
    !std::process::Command::new("/usr/bin/defaults")
        .args(["read", "com.apple.universalaccess", "reduceMotion"])
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).trim() == "1")
}

fn report(result: Result<(), String>) {
    if let Err(error) = result {
        with_settings(|ui| ui.message(&error, false));
    }
}

fn close_settings() {
    with_settings(SettingsUi::save_now);
    // Dropped from a fresh event-loop turn: this runs inside one of the window's own callbacks.
    slint::Timer::single_shot(Duration::ZERO, || {
        if let Some(ui) = SETTINGS.with(|slot| slot.borrow_mut().take()) {
            let _ = ui.window.hide();
        }
    });
}

// ---------- Startup ----------

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
    // A second launch (Start menu, double-click) asks the running app for its settings window.
    let open_request = app_data_dir()?.join("open-settings.request");
    // Keep the OS lock alive until the app exits; crashes release it too.
    let Some(_instance) = acquire_instance_lock(&app_data_dir()?.join("desktop-instance.lock"))?
    else {
        fs::write(&open_request, b"")?;
        return Ok(());
    };
    let _ = fs::remove_file(&open_request);

    let stop = Arc::new(AtomicBool::new(false));
    let daemon = std::thread::spawn({
        let stop = Arc::clone(&stop);
        let (settings, status) = (settings_path().ok(), status_path().ok());
        move || daemon::run(stop, settings, status)
    });

    #[cfg(windows)]
    let base_icon = {
        panel::select_backend()?;
        let icon = panel::tray_rgba().ok_or("the tray picture could not be decoded")?;
        panel::add_icon(&icon.0, icon.1, icon.2, "Codex RPC")?;
        Some(icon)
    };
    #[cfg(not(windows))]
    let base_icon = {
        let tray = Tray::new()?;
        wire_tray(&tray);
        let icon = tray
            .get_image()
            .to_rgba8()
            .map(|pixels| (pixels.as_bytes().to_vec(), pixels.width(), pixels.height()));
        tray.show()?;
        TRAY.with(|slot| *slot.borrow_mut() = Some(tray));
        icon
    };
    monitor::spawn(base_icon);
    // Normally the app starts quietly in the tray.
    if std::env::args().any(|arg| arg == "--settings") {
        open_settings();
    }
    let open_requests = slint::Timer::default();
    open_requests.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(500),
        move || {
            if fs::remove_file(&open_request).is_ok() {
                open_settings();
            }
        },
    );

    slint::run_event_loop_until_quit()?;

    // "Quit" can come while a change still waits for its delayed save.
    with_settings(SettingsUi::save_now);
    SETTINGS.with(|slot| slot.borrow_mut().take());
    #[cfg(windows)]
    panel::shutdown();
    #[cfg(not(windows))]
    TRAY.with(|slot| slot.borrow_mut().take());
    stop.store(true, Ordering::SeqCst);
    let _ = daemon.join();
    Ok(())
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

    /// Two form fields wired to each other's setting would save the wrong option.
    #[test]
    fn settings_form_round_trips_every_field() {
        i_slint_backend_testing::init_no_event_loop();
        let window = SettingsWindow::new().unwrap();
        let app = window.global::<App>();
        let base = serde_json::to_value(RpcSettings::default()).unwrap();
        let flags: Vec<String> = base
            .as_object()
            .unwrap()
            .iter()
            .filter(|(_, value)| value.is_boolean())
            .map(|(key, _)| key.clone())
            .collect();
        assert!(flags.len() >= 10);
        // One flag on at a time: a swap or a missing field changes the result.
        for on in &flags {
            let mut value = base.clone();
            for flag in &flags {
                value[flag] = serde_json::json!(flag == on);
            }
            value["mode"] = "competing".into();
            value["idle_clear_minutes"] = 30.into();
            value["custom_state"] = "{model} · {week}".into();
            value["buttons"] = serde_json::json!([
                {"label": "A", "url": "https://a.example"},
                {"label": "B", "url": "https://b.example"},
            ]);
            let settings: RpcSettings = serde_json::from_value(value).unwrap();
            fill_form(&app, &settings);
            let mut read = RpcSettings::default();
            read_form(&app, &mut read);
            assert_eq!(
                serde_json::to_value(&read).unwrap(),
                serde_json::to_value(&settings).unwrap(),
                "with only {on} on"
            );
        }
    }

    #[test]
    #[ignore = "Shows a desktop notification"]
    fn live_notification() {
        assert!(notify("Codex RPC", "Notification test"));
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
        let status =
            parse_status_line("Codex: CLI||Usage: 5h 42% left|Discord: Not connected|plus");
        assert!(status.model_parts.is_empty());
        assert_eq!(status.credits, None);
        assert_eq!(status.plan, "plus");
    }

    #[test]
    fn status_line_extras_attach_resets_presence_and_start() {
        let status = parse_status_line(
            "Codex: CLI|GPT-5|Usage: 5h 42% left / week 7% left|Discord: Not connected|plus|paused|5h=1700,week=9000|1234|codex-rpc",
        );
        assert_eq!(status.presence, "paused");
        assert_eq!(status.started_at_ms, Some(1234));
        assert_eq!(status.project, "codex-rpc");
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
