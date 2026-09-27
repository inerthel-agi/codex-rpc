//! Background work driven by the daemon's status file: tray icon and tooltip,
//! low-usage notifications, the local usage history and the update check.

use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::PathBuf, sync::Mutex, time::Duration};
use tauri::{image::Image, AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;

use crate::{AppStatus, UsageEntry};

pub(crate) const TRAY_ID: &str = "main";
const TICK: Duration = Duration::from_secs(3);
const HOUR_MS: u64 = 3_600_000;
const HISTORY_HOURS: u64 = 8 * 24;
const UPDATE_FIRST_CHECK_MS: u64 = 30_000;
const UPDATE_INTERVAL_MS: u64 = 24 * HOUR_MS;
const RELEASES_API: &str = "https://api.github.com/repos/inerthel-agi/codex-rpc/releases/latest";

/// State the Tauri commands read back from the monitor thread.
#[derive(Default)]
pub(crate) struct Shared {
    pub(crate) latest_version: Mutex<Option<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrayVisual {
    Normal,
    Off,
    Paused,
    Low,
    Critical,
}

pub(crate) fn tray_visual(status: &AppStatus) -> TrayVisual {
    if status.state == "Codex: Off" {
        return TrayVisual::Off;
    }
    if status.presence == "paused" {
        return TrayVisual::Paused;
    }
    let lowest = status
        .usage
        .iter()
        .map(|entry| entry.percent)
        .fold(f64::INFINITY, f64::min);
    if lowest <= 10.0 {
        TrayVisual::Critical
    } else if lowest <= 25.0 {
        TrayVisual::Low
    } else {
        TrayVisual::Normal
    }
}

pub(crate) fn tray_tooltip(status: &AppStatus) -> String {
    let mut parts = vec!["Codex RPC".to_string()];
    if status.state == "Codex: Off" {
        parts.push("Codex is not running".into());
    } else if status.presence == "paused" {
        parts.push("Presence paused".into());
    }
    for entry in &status.usage {
        let name = if entry.label == "5h" { "5h" } else { "Weekly" };
        parts.push(format!("{name} {}% left", entry.percent.round()));
    }
    parts.join(" · ")
}

/// Grey icon when Codex is off, otherwise a coloured dot in the bottom-right corner.
pub(crate) fn badge_icon(rgba: &[u8], width: u32, height: u32, visual: TrayVisual) -> Vec<u8> {
    let mut out = rgba.to_vec();
    // A panic here would silently stop the whole monitor thread.
    if out.len() != (width as usize) * (height as usize) * 4 {
        return out;
    }
    if visual == TrayVisual::Off {
        for px in out.chunks_exact_mut(4) {
            let grey = (px[0] as u32 * 30 + px[1] as u32 * 59 + px[2] as u32 * 11) / 100;
            px[..3].fill(grey as u8);
            px[3] = (px[3] as u32 * 55 / 100) as u8;
        }
        return out;
    }
    let color = match visual {
        TrayVisual::Paused => [0xa3, 0xa3, 0xa3],
        TrayVisual::Low => [0xf5, 0xa5, 0x24],
        TrayVisual::Critical => [0xf8, 0x71, 0x71],
        TrayVisual::Normal | TrayVisual::Off => return out,
    };
    let size = width.min(height) as f64;
    let radius = size * 0.2;
    let ring = (size * 0.06).max(1.0);
    let (cx, cy) = (width as f64 - radius - ring, height as f64 - radius - ring);
    for y in 0..height {
        for x in 0..width {
            let distance = ((x as f64 + 0.5 - cx).powi(2) + (y as f64 + 0.5 - cy).powi(2)).sqrt();
            let fill = if distance <= radius {
                color
            } else if distance <= radius + ring {
                [0x17, 0x17, 0x17]
            } else {
                continue;
            };
            let i = ((y * width + x) * 4) as usize;
            out[i..i + 3].copy_from_slice(&fill);
            out[i + 3] = 255;
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Alert {
    Low,
    Critical,
    Reset,
}

/// Per-limit alert memory. Usage comes from two sources (account API and local
/// rollouts) that can disagree by a few points, so a threshold is re-armed only
/// after the limit climbs clearly above it again.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct AlertState {
    seen: bool,
    /// 0 = fine, 1 = low already announced, 2 = critical already announced.
    level: u8,
    below_full: bool,
}

const LOW: f64 = 25.0;
const CRITICAL: f64 = 10.0;
const REARM_MARGIN: f64 = 5.0;

impl AlertState {
    pub(crate) fn step(&mut self, next: f64) -> Option<Alert> {
        let level = if next <= CRITICAL {
            2
        } else if next <= LOW {
            1
        } else {
            0
        };
        // The first reading sets the baseline: no alert for a limit that was
        // already low when the app started.
        if !self.seen {
            *self = Self {
                seen: true,
                level,
                below_full: next < 90.0,
            };
            return None;
        }
        let mut alert = None;
        if level > self.level {
            alert = Some(if level == 2 {
                Alert::Critical
            } else {
                Alert::Low
            });
            self.level = level;
        } else if next > LOW + REARM_MARGIN {
            self.level = 0;
        } else if next > CRITICAL + REARM_MARGIN {
            self.level = self.level.min(1);
        }
        if next >= 99.5 && self.below_full {
            alert = alert.or(Some(Alert::Reset));
            self.below_full = false;
        } else if next < 90.0 {
            self.below_full = true;
        }
        alert
    }
}

pub(crate) fn humanize_duration(ms: u64) -> String {
    let minutes = ms / 60_000;
    let (days, hours, mins) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{}m", mins.max(1))
    }
}

fn alert_text(alert: Alert, entry: &UsageEntry, now: u64) -> (String, String) {
    let name = if entry.label == "5h" {
        "5-hour limit"
    } else {
        "Weekly limit"
    };
    let reset = entry
        .resets_at_ms
        .filter(|reset| *reset > now)
        .map(|reset| format!(" Resets in {}.", humanize_duration(reset - now)))
        .unwrap_or_default();
    let percent = entry.percent.round();
    match alert {
        Alert::Low => (
            "Codex usage is getting low".into(),
            format!("{name}: {percent}% left.{reset}"),
        ),
        Alert::Critical => (
            "Codex usage almost used up".into(),
            format!("{name}: only {percent}% left.{reset}"),
        ),
        Alert::Reset => (
            "Codex limit reset".into(),
            format!("{name} is back to 100%."),
        ),
    }
}

/// Weekly-limit percentage consumed per UTC hour; the UI groups hours into local days.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct History {
    pub(crate) hours: BTreeMap<u64, f64>,
    pub(crate) last_week: Option<f64>,
}

impl History {
    /// Returns true when the file needs saving.
    pub(crate) fn record(&mut self, now: u64, remaining: f64) -> bool {
        let hour = now / HOUR_MS;
        let mut baseline = remaining;
        if let Some(previous) = self.last_week {
            if remaining < previous {
                *self.hours.entry(hour).or_insert(0.0) += previous - remaining;
            } else if remaining < 99.0 && remaining - previous < 50.0 {
                // Within a week the limit only goes down. A small rise is a stale
                // reading from the other source; keeping the old baseline stops the
                // next fresh reading from being counted twice.
                baseline = previous;
            }
        }
        let changed = self.last_week != Some(baseline);
        self.last_week = Some(baseline);
        let oldest = hour.saturating_sub(HISTORY_HOURS);
        self.hours.retain(|hour, _| *hour >= oldest);
        changed
    }
}

fn history_path() -> Result<PathBuf, String> {
    Ok(crate::app_data_dir()?.join("usage-history.json"))
}

pub(crate) fn load_history() -> History {
    history_path()
        .ok()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_history(history: &History) {
    if let (Ok(path), Ok(json)) = (history_path(), serde_json::to_string(history)) {
        let tmp = path.with_extension("json.tmp");
        if fs::write(&tmp, json).is_ok() {
            let _ = fs::rename(tmp, path);
        }
    }
}

pub(crate) fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |value: &str| -> Vec<u64> {
        value
            .trim_start_matches('v')
            .split('.')
            .map(|part| part.parse().unwrap_or(0))
            .collect()
    };
    parse(latest) > parse(current)
}

fn fetch_latest_version() -> Option<String> {
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt;
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        let mut command =
            std::process::Command::new(PathBuf::from(root).join("System32").join("curl.exe"));
        command.creation_flags(0x08000000);
        command
    };
    #[cfg(not(windows))]
    let mut command = std::process::Command::new("/usr/bin/curl");
    let output = command
        .args([
            "-fsSL",
            "--max-time",
            "10",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: codex-rpc",
            RELEASES_API,
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let tag = body.get("tag_name")?.as_str()?.trim_start_matches('v');
    let valid = !tag.is_empty()
        && tag.len() <= 16
        && tag.chars().all(|ch| ch.is_ascii_digit() || ch == '.');
    valid.then(|| tag.to_string())
}

pub(crate) fn spawn(app: AppHandle) {
    std::thread::spawn(move || run(app));
}

fn run(app: AppHandle) {
    let base_icon = app
        .default_window_icon()
        .map(|icon| (icon.rgba().to_vec(), icon.width(), icon.height()));
    let started = crate::now_ms();
    let mut last_visual = None;
    let mut last_tooltip = String::new();
    let mut alerts: BTreeMap<String, AlertState> = BTreeMap::new();
    let mut history = load_history();
    let mut next_update_check = started + UPDATE_FIRST_CHECK_MS;

    loop {
        let now = crate::now_ms();
        let status = crate::parse_status_line(&crate::read_status_line());
        let settings = crate::read_settings().unwrap_or_default();

        let visual = tray_visual(&status);
        let tooltip = tray_tooltip(&status);
        if let Some(tray) = app.tray_by_id(TRAY_ID) {
            if last_visual != Some(visual) {
                if let Some((rgba, width, height)) = &base_icon {
                    let icon = badge_icon(rgba, *width, *height, visual);
                    let _ = tray.set_icon(Some(Image::new_owned(icon, *width, *height)));
                }
                last_visual = Some(visual);
            }
            if tooltip != last_tooltip {
                let _ = tray.set_tooltip(Some(tooltip.as_str()));
                last_tooltip = tooltip;
            }
        }

        for entry in &status.usage {
            let alert = alerts
                .entry(entry.label.clone())
                .or_default()
                .step(entry.percent);
            let wanted = match alert {
                Some(Alert::Low | Alert::Critical) => settings.notify_low,
                Some(Alert::Reset) => settings.notify_reset,
                None => false,
            };
            if let (true, Some(alert)) = (wanted, alert) {
                let (title, body) = alert_text(alert, entry, now);
                let _ = app.notification().builder().title(title).body(body).show();
            }
            if entry.label == "Week" && history.record(now, entry.percent) {
                save_history(&history);
            }
        }

        if !settings.check_updates {
            if let Ok(mut slot) = app.state::<Shared>().latest_version.lock() {
                *slot = None;
            }
        } else if now >= next_update_check {
            next_update_check = now + UPDATE_INTERVAL_MS;
            // curl can take up to 10 s; keep the tray icon and alerts responsive meanwhile.
            let app = app.clone();
            std::thread::spawn(move || {
                let latest = fetch_latest_version()
                    .filter(|latest| is_newer(latest, env!("CARGO_PKG_VERSION")));
                if let Ok(mut slot) = app.state::<Shared>().latest_version.lock() {
                    *slot = latest;
                }
            });
        }

        std::thread::sleep(TICK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(state: &str, presence: &str, percents: &[(&str, f64)]) -> AppStatus {
        let mut status = crate::parse_status_line(state);
        status.presence = presence.into();
        status.usage = percents
            .iter()
            .map(|(label, percent)| UsageEntry {
                label: (*label).into(),
                value: format!("{percent}%"),
                percent: *percent,
                resets_at_ms: None,
            })
            .collect();
        status
    }

    #[test]
    fn tray_visual_follows_state_pause_and_lowest_limit() {
        assert_eq!(
            tray_visual(&status("Codex: Off", "off", &[])),
            TrayVisual::Off
        );
        assert_eq!(
            tray_visual(&status("Codex: CLI", "paused", &[("Week", 5.0)])),
            TrayVisual::Paused
        );
        assert_eq!(
            tray_visual(&status(
                "Codex: CLI",
                "live",
                &[("5h", 60.0), ("Week", 8.0)]
            )),
            TrayVisual::Critical
        );
        assert_eq!(
            tray_visual(&status("Codex: CLI", "live", &[("Week", 20.0)])),
            TrayVisual::Low
        );
        assert_eq!(
            tray_visual(&status("Codex: CLI", "live", &[("Week", 97.0)])),
            TrayVisual::Normal
        );
    }

    #[test]
    fn alerts_fire_once_on_each_threshold() {
        let steps = |values: &[f64]| {
            let mut state = AlertState::default();
            values
                .iter()
                .map(|value| state.step(*value))
                .collect::<Vec<_>>()
        };
        // The first reading is only a baseline, even when already low.
        assert_eq!(steps(&[5.0, 4.0]), [None, None]);
        assert_eq!(
            steps(&[30.0, 24.0, 23.0, 9.0, 8.0]),
            [None, Some(Alert::Low), None, Some(Alert::Critical), None]
        );
        assert_eq!(steps(&[30.0, 9.0]), [None, Some(Alert::Critical)]);
        assert_eq!(steps(&[40.0, 100.0]), [None, Some(Alert::Reset)]);
        assert_eq!(steps(&[97.0, 100.0]), [None, None]);
    }

    #[test]
    fn alerts_do_not_repeat_when_sources_disagree() {
        let mut state = AlertState::default();
        let fired: Vec<_> = [30.0, 24.0, 27.0, 24.0, 28.0, 24.0]
            .iter()
            .filter_map(|value| state.step(*value))
            .collect();
        assert_eq!(fired, [Alert::Low]);
        // Climbing clearly above the threshold re-arms it.
        assert_eq!(state.step(40.0), None);
        assert_eq!(state.step(24.0), Some(Alert::Low));
    }

    #[test]
    fn history_ignores_stale_readings_and_survives_a_round_trip() {
        let mut history = History::default();
        for value in [50.0, 45.0, 48.0, 45.0, 48.0, 40.0] {
            history.record(0, value);
        }
        assert_eq!(history.hours.get(&0), Some(&10.0));
        let json = serde_json::to_string(&history).unwrap();
        let back: History = serde_json::from_str(&json).unwrap();
        assert_eq!(back.hours.get(&0), Some(&10.0));
        assert_eq!(back.last_week, Some(40.0));
    }

    #[test]
    fn badge_ignores_mismatched_buffers() {
        assert_eq!(badge_icon(&[1, 2, 3], 16, 16, TrayVisual::Low), [1, 2, 3]);
    }

    #[test]
    fn history_counts_only_decreases_and_prunes_old_hours() {
        let mut history = History::default();
        assert!(history.record(0, 100.0));
        assert!(history.record(HOUR_MS / 2, 97.0));
        assert!(history.record(HOUR_MS, 100.0));
        assert!(!history.record(HOUR_MS, 100.0));
        assert!(history.record(HOUR_MS * 2, 90.0));
        assert_eq!(history.hours.get(&0), Some(&3.0));
        assert_eq!(history.hours.get(&1), None);
        assert_eq!(history.hours.get(&2), Some(&10.0));
        history.record(HOUR_MS * (HISTORY_HOURS + 1), 80.0);
        assert!(!history.hours.contains_key(&0));
    }

    #[test]
    fn badge_keeps_size_and_marks_the_corner() {
        let rgba = vec![0u8; 16 * 16 * 4];
        let out = badge_icon(&rgba, 16, 16, TrayVisual::Critical);
        assert_eq!(out.len(), rgba.len());
        let corner = ((12 * 16 + 12) * 4) as usize;
        assert_eq!(&out[corner..corner + 4], &[0xf8, 0x71, 0x71, 255]);
        assert_eq!(badge_icon(&rgba, 16, 16, TrayVisual::Normal), rgba);
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("v0.5.0", "0.4.1"));
        assert!(!is_newer("0.4.1", "0.4.1"));
        assert!(!is_newer("0.4.0", "0.4.1"));
    }

    #[test]
    fn durations_are_short() {
        assert_eq!(humanize_duration(4 * 86_400_000 + 11 * HOUR_MS), "4d 11h");
        assert_eq!(humanize_duration(3 * HOUR_MS + 20 * 60_000), "3h 20m");
        assert_eq!(humanize_duration(10_000), "1m");
    }
}
