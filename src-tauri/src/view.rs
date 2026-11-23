//! Text and numbers shown in the settings window and the tray menu, derived
//! from the daemon's status line and the saved settings.

use crate::{daemon::RpcSettings, monitor::humanize_duration, AppStatus, UsageEntry};
use std::collections::BTreeMap;

const DAY_MS: i64 = 86_400_000;

pub(crate) fn is_active(state: &str) -> bool {
    !state.is_empty() && state != "Codex: Off"
}

pub(crate) fn source_label(state: &str) -> &'static str {
    match state {
        "Codex: CLI/Desktop" => "CLI + Desktop",
        "Codex: CLI" => "CLI",
        "Codex: Desktop" => "Desktop",
        _ => "Offline",
    }
}

pub(crate) fn plan_label(plan: &str) -> String {
    let value = plan.to_ascii_lowercase();
    // Pro tiers are named after the 100/200/500 switch on the pricing page.
    let known = match value.as_str() {
        "prolite" => "Pro 100",
        "pro" => "Pro 200",
        "promax" => "Pro 500",
        "plus" => "Plus",
        "go" => "Go",
        "free" => "Free",
        "team" | "business" => "Business",
        "enterprise" => "Enterprise",
        "edu" => "Edu",
        _ if value.starts_with("pro") => "Pro",
        // No plan means an API-key sign-in, or usage not loaded yet.
        "" => "API key / unknown",
        _ => return capitalize(&value),
    };
    known.into()
}

fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

pub(crate) fn is_pro(plan: &str) -> bool {
    plan.to_ascii_lowercase().starts_with("pro")
}

/// A limit the plan exposes; `available` is false while Codex has not reported it yet.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Limit {
    pub(crate) five_hour: bool,
    pub(crate) available: bool,
    pub(crate) percent: f64,
    pub(crate) resets_at: Option<u64>,
}

impl Limit {
    fn title(&self) -> &'static str {
        if self.five_hour {
            "5-hour limit"
        } else {
            "Weekly limit"
        }
    }

    /// 0 fine, 1 low, 2 critical, 3 unavailable.
    pub(crate) fn level(&self) -> i32 {
        match self.percent {
            _ if !self.available => 3,
            percent if percent <= 10.0 => 2,
            percent if percent <= 25.0 => 1,
            _ => 0,
        }
    }

    pub(crate) fn figure(&self) -> String {
        if self.available {
            format!("{}%", self.percent.round())
        } else {
            "—".into()
        }
    }

    pub(crate) fn caption(&self) -> String {
        if self.available {
            format!("of {} left", self.title().to_lowercase())
        } else {
            format!("{} unavailable", self.title())
        }
    }

    pub(crate) fn reset_in(&self, now: u64) -> String {
        self.resets_at
            .filter(|reset| *reset > now)
            .map(|reset| format!("Resets in {}", humanize_duration(reset - now)))
            .unwrap_or_default()
    }

    fn short(&self) -> &'static str {
        if self.five_hour {
            "5h"
        } else {
            "Week"
        }
    }

    pub(crate) fn view(&self, now: u64) -> LimitView {
        LimitView {
            figure: self.figure(),
            caption: self.caption(),
            percent: (self.percent / 100.0) as f32,
            level: self.level(),
            reset_in: self.reset_in(now),
            reset_date: self
                .resets_at
                .map(|reset| local_date_time(reset, local_offset_ms()))
                .unwrap_or_default(),
        }
    }

    /// One line for the tray menu.
    #[cfg_attr(windows, allow(dead_code))]
    fn summary(&self, now: u64) -> String {
        if !self.available {
            return self.caption();
        }
        let reset = self.reset_in(now);
        let separator = if reset.is_empty() { "" } else { " · " };
        format!("{} {}{separator}{reset}", self.figure(), self.caption())
    }
}

pub(crate) fn limits(entries: &[UsageEntry], plan: &str) -> Vec<Limit> {
    let find = |name: &str| {
        entries
            .iter()
            .find(|entry| entry.label.eq_ignore_ascii_case(name))
    };
    let names: Vec<&str> = if is_pro(plan) {
        vec!["week"]
    } else if plan.eq_ignore_ascii_case("plus") {
        vec!["5h", "week"]
    } else {
        ["5h", "week"]
            .into_iter()
            .filter(|name| find(name).is_some())
            .collect()
    };
    names
        .into_iter()
        .map(|name| {
            let entry = find(name).filter(|entry| entry.percent.is_finite());
            Limit {
                five_hour: name == "5h",
                available: entry.is_some(),
                percent: entry.map_or(0.0, |entry| entry.percent.clamp(0.0, 100.0)),
                resets_at: entry.and_then(|entry| entry.resets_at_ms),
            }
        })
        .collect()
}

/// Model, effort and speed from the status line, e.g. ["GPT-6-Astra", "Extra High", "Fast"].
pub(crate) fn split_model(parts: &[String]) -> (String, String, String) {
    let is_effort = |part: &str| {
        [
            "minimal",
            "low",
            "medium",
            "high",
            "extra high",
            "max",
            "ultra",
        ]
        .contains(&part.to_ascii_lowercase().as_str())
    };
    let is_speed = |part: &str| ["fast", "standard"].contains(&part.to_ascii_lowercase().as_str());
    let pick = |test: &dyn Fn(&str) -> bool| {
        parts
            .iter()
            .find(|part| test(part))
            .cloned()
            .unwrap_or_default()
    };
    let model = parts
        .first()
        .filter(|part| !is_effort(part) && !is_speed(part))
        .cloned()
        .unwrap_or_default();
    (model, pick(&is_effort), pick(&is_speed))
}

/// "credits 12.5" -> "12.5".
pub(crate) fn credits_number(credits: Option<&str>) -> Option<String> {
    let text = credits?;
    let start = text.find(|ch: char| ch.is_ascii_digit())?;
    Some(
        text[start..]
            .chars()
            .take_while(|ch| ch.is_ascii_digit() || matches!(ch, '.' | ','))
            .collect(),
    )
}

fn drop_dangling_separators(text: &str) -> String {
    let separator = |word: &str| !word.is_empty() && word.chars().all(|ch| "·-|/•—".contains(ch));
    let mut words: Vec<&str> = Vec::new();
    for word in text.split_whitespace() {
        if separator(word) && words.last().is_none_or(|last| separator(last)) {
            continue;
        }
        words.push(word);
    }
    while words.last().is_some_and(|last| separator(last)) {
        words.pop();
    }
    words.join(" ")
}

/// Discord's state line; mirrors daemon.rs `build_state_line` for the live preview.
pub(crate) fn preview_state(status: &AppStatus, settings: &RpcSettings) -> String {
    let pro = is_pro(&status.plan);
    let (model, effort, speed) = split_model(&status.model_parts);
    let shown = if settings.hide_model {
        ["Coding".to_string(), String::new(), String::new()]
    } else {
        [
            model,
            if settings.show_effort {
                effort
            } else {
                String::new()
            },
            if settings.show_fast_mode {
                speed
            } else {
                String::new()
            },
        ]
    };
    let percent = |wanted: bool, name: &str| {
        status
            .usage
            .iter()
            .find(|entry| wanted && entry.label.eq_ignore_ascii_case(name))
            .map(|entry| format!("{}%", entry.percent.round()))
            .unwrap_or_default()
    };
    let h5 = percent(settings.show_primary_usage && !pro, "5h");
    let week = percent(settings.show_weekly_usage, "week");
    let credits = credits_number(status.credits.as_deref())
        .filter(|_| settings.show_credits)
        .unwrap_or_default();
    let plan = if settings.show_plan && !status.plan.is_empty() {
        plan_label(&status.plan)
    } else {
        String::new()
    };

    let template = settings.custom_state.trim();
    if !template.is_empty() {
        // Like {credits}, {plan} stays empty unless its chip is ticked (daemon.rs filter_usage).
        let project = if settings.hide_model { "" } else { "project" };
        let mut text = template.to_string();
        for (token, value) in [
            ("{model}", shown[0].as_str()),
            ("{effort}", shown[1].as_str()),
            ("{speed}", shown[2].as_str()),
            ("{5h}", h5.as_str()),
            ("{week}", week.as_str()),
            ("{credits}", credits.as_str()),
            ("{plan}", plan.as_str()),
            ("{project}", project),
        ] {
            text = text.replace(token, value);
        }
        let text = drop_dangling_separators(&text);
        if text.chars().count() >= 2 {
            return text.chars().take(128).collect();
        }
    }

    let base = shown
        .iter()
        .filter(|part| !part.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" - ");
    let base = if base.is_empty() {
        match status.state.as_str() {
            "Codex: CLI" => "Terminal session active",
            "Codex: CLI/Desktop" => "CLI + Desktop",
            _ => "Desktop session",
        }
        .to_string()
    } else {
        base
    };
    let whole_credits = credits
        .replace(',', ".")
        .parse::<f64>()
        .map_or(0.0, f64::round);
    let label = |prefix: &str, value: &str| {
        if value.is_empty() {
            String::new()
        } else {
            format!("{prefix}{value}")
        }
    };
    [
        base,
        label("5h ", &h5),
        label("week ", &week),
        if whole_credits > 0.0 {
            format!("{whole_credits} credits")
        } else {
            String::new()
        },
        label("ChatGPT ", &plan),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" - ")
}

/// Discord's details line; mirrors daemon.rs `build_details`.
fn preview_details(status: &AppStatus, settings: &RpcSettings) -> String {
    let base = preview_activity(status, &settings.mode);
    let with_project = format!("{base} - {}", status.project);
    if is_active(&status.state)
        && !status.project.is_empty()
        && !settings.hide_model
        && with_project.len() <= 96
    {
        with_project
    } else {
        base.into()
    }
}

fn preview_activity(status: &AppStatus, mode: &str) -> &'static str {
    let watching = mode == "watching";
    match status.state.as_str() {
        state if !is_active(state) => "Codex is not running",
        "Codex: CLI/Desktop" if watching => "Watching Codex (CLI + Desktop)",
        "Codex: CLI/Desktop" => "Coding with Codex (CLI + Desktop)",
        state if state.contains("CLI") && watching => "Watching Codex CLI",
        state if state.contains("CLI") => "Coding with Codex CLI",
        _ if watching => "Watching Codex",
        _ => "Using Codex",
    }
}

/// "00:12:07 elapsed", or empty when the timer is hidden.
pub(crate) fn elapsed(status: &AppStatus, settings: &RpcSettings, now: u64) -> String {
    match status.started_at_ms {
        Some(started) if settings.show_elapsed && is_active(&status.state) => {
            let seconds = now.saturating_sub(started) / 1000;
            format!(
                "{:02}:{:02}:{:02} elapsed",
                seconds / 3600,
                seconds / 60 % 60,
                seconds % 60
            )
        }
        _ => String::new(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LimitView {
    pub(crate) figure: String,
    pub(crate) caption: String,
    pub(crate) percent: f32,
    pub(crate) level: i32,
    pub(crate) reset_in: String,
    pub(crate) reset_date: String,
}

/// Everything in the settings window that follows the status line, except the
/// elapsed timer, so the window is only repainted when something changed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StatusView {
    pub(crate) active: bool,
    pub(crate) source_label: &'static str,
    pub(crate) plan_label: String,
    pub(crate) has_plan: bool,
    pub(crate) pro: bool,
    pub(crate) discord_status: String,
    pub(crate) limits: Vec<LimitView>,
    pub(crate) credits: String,
    pub(crate) dc_activity: &'static str,
    pub(crate) dc_details: String,
    pub(crate) dc_state: String,
    pub(crate) dc_note: &'static str,
    pub(crate) dc_buttons: Vec<String>,
}

impl StatusView {
    pub(crate) fn new(status: &AppStatus, settings: &RpcSettings, now: u64) -> Self {
        let active = is_active(&status.state);
        let watching = settings.mode == "watching";
        Self {
            active,
            source_label: source_label(&status.state),
            plan_label: plan_label(&status.plan),
            has_plan: !status.plan.is_empty(),
            pro: is_pro(&status.plan),
            discord_status: if status.discord.is_empty() {
                "Not connected".into()
            } else {
                status.discord.clone()
            },
            limits: limits(&status.usage, &status.plan)
                .iter()
                .map(|limit| limit.view(now))
                .collect(),
            credits: credits_number(status.credits.as_deref()).unwrap_or_else(|| "—".into()),
            dc_activity: match settings.mode.as_str() {
                "watching" => "WATCHING",
                "listening" => "LISTENING TO",
                "competing" => "COMPETING IN",
                _ => "PLAYING",
            },
            dc_details: preview_details(status, settings),
            dc_state: if active {
                preview_state(status, settings)
            } else {
                "Nothing is shown on Discord right now".into()
            },
            dc_note: match status.presence.as_str() {
                _ if !active => "",
                "paused" => "Paused from the tray menu: Discord shows nothing until you resume.",
                "idle" => "Codex is idle, so the presence is cleared for now.",
                _ => "",
            },
            dc_buttons: settings
                .buttons
                .iter()
                .filter(|_| watching)
                .map(|button| button.label.clone())
                .collect(),
        }
    }
}

/// What the tray panel shows (Windows).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct PanelView {
    /// "GPT-6.2-Astra · Ultra", or what Codex is doing when no model is known.
    pub(crate) model_line: String,
    pub(crate) pill: &'static str,
    /// 0 off, 1 live, 2 paused or idle.
    pub(crate) pill_kind: i32,
    /// The most constrained limit leads; the others go to `facts`.
    pub(crate) hero: Option<LimitView>,
    pub(crate) plan: String,
    pub(crate) banner: String,
    pub(crate) banner_level: i32,
    pub(crate) facts: Vec<(String, String)>,
    pub(crate) paused: bool,
    pub(crate) pause_label: String,
    pub(crate) hide_model: bool,
    pub(crate) update: String,
    /// Discord line as text before the user name, the name in bold, and the rest.
    pub(crate) discord: [String; 3],
    /// "", "reconnect" or "resume".
    pub(crate) discord_action: &'static str,
}

pub(crate) fn panel_view(
    status: &AppStatus,
    settings: &RpcSettings,
    update: Option<String>,
    now: u64,
) -> PanelView {
    let active = is_active(&status.state);
    let paused = settings.paused_until_ms > now;
    let resumes = if settings.paused_until_ms >= crate::daemon::PAUSE_FOREVER {
        String::new()
    } else {
        format!(
            "resumes in {}",
            humanize_duration(settings.paused_until_ms.saturating_sub(now))
        )
    };
    let (pill, pill_kind) = match status.presence.as_str() {
        _ if !active => ("Offline", 0),
        _ if paused => ("Paused", 2),
        "idle" => ("Idle", 2),
        _ => (source_label(&status.state), 1),
    };
    let model_line = if !status.model_parts.is_empty() {
        status.model_parts.join(" · ")
    } else {
        match status.state.as_str() {
            "Codex: CLI/Desktop" => "Connected to Codex CLI and Desktop",
            "Codex: CLI" => "Connected to Codex CLI",
            "Codex: Desktop" => "Connected to Codex Desktop",
            _ => "Codex is not running",
        }
        .into()
    };

    let limits = limits(&status.usage, &status.plan);
    let hero = limits
        .iter()
        .filter(|limit| limit.available)
        .min_by(|a, b| a.percent.total_cmp(&b.percent))
        .or(limits.first());
    let banner = match hero {
        Some(limit) if limit.level() == 2 => format!(
            "Only {}% of your {} left",
            limit.percent.round(),
            limit.title().to_lowercase()
        ),
        Some(limit) if limit.level() == 1 => format!("{} is getting low", limit.title()),
        _ => String::new(),
    };
    let mut facts: Vec<(String, String)> = limits
        .iter()
        .filter(|limit| Some(*limit) != hero)
        .map(|limit| {
            let value = if limit.available {
                format!("{}%", limit.percent.round())
            } else {
                "—".into()
            };
            (limit.short().into(), value)
        })
        .collect();
    if let Some(credits) = credits_number(status.credits.as_deref()) {
        facts.push(("Credits".into(), credits));
    }
    let (_, _, speed) = split_model(&status.model_parts);
    if !speed.is_empty() {
        facts.push(("Mode".into(), speed));
    }

    let user = status
        .discord
        .strip_suffix(')')
        .and_then(|text| text.split_once("Connected ("))
        .map(|(_, user)| user.to_string());
    let after = |text: String| {
        if text.is_empty() {
            text
        } else {
            format!(" · {text}")
        }
    };
    let (discord, discord_action) = match user {
        None => (
            ["Discord not connected".into(), String::new(), String::new()],
            "reconnect",
        ),
        Some(user) if !active => (
            ["Connected as ".into(), user, " · waiting for Codex".into()],
            "",
        ),
        Some(user) if paused => (
            ["Hidden from ".into(), user, after(resumes.clone())],
            "resume",
        ),
        Some(user) if status.presence == "idle" => (
            ["Cleared from ".into(), user, " · Codex is idle".into()],
            "",
        ),
        Some(user) => {
            let since = status
                .started_at_ms
                .map(|started| humanize_duration(now.saturating_sub(started)))
                .unwrap_or_default();
            (["Showing on ".into(), user, after(since)], "")
        }
    };

    PanelView {
        model_line,
        pill,
        pill_kind,
        hero: hero.map(|limit| limit.view(now)),
        plan: plan_label(&status.plan),
        banner_level: hero.map_or(0, |limit| limit.level().min(2)),
        banner,
        facts,
        paused,
        pause_label: if paused {
            format!("Presence paused{}", after(resumes))
        } else {
            "Pause Discord presence".into()
        },
        hide_model: settings.hide_model,
        update: update.unwrap_or_default(),
        discord,
        discord_action,
    }
}

/// What the tray menu shows (macOS and other platforms).
#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(windows, allow(dead_code))]
pub(crate) struct TrayModel {
    pub(crate) headline: String,
    pub(crate) facts: Vec<String>,
    pub(crate) paused: bool,
    pub(crate) pause_label: String,
    pub(crate) hide_model: bool,
    pub(crate) update: String,
    pub(crate) discord: String,
    pub(crate) can_reconnect: bool,
}

#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn tray_model(
    status: &AppStatus,
    settings: &RpcSettings,
    update: Option<String>,
    now: u64,
) -> TrayModel {
    let active = is_active(&status.state);
    let paused = settings.paused_until_ms > now;
    let resumes = if settings.paused_until_ms >= crate::daemon::PAUSE_FOREVER {
        String::new()
    } else {
        format!(
            " · resumes in {}",
            humanize_duration(settings.paused_until_ms.saturating_sub(now))
        )
    };
    let (_, _, speed) = split_model(&status.model_parts);

    let headline = if !active {
        "Codex is not running".to_string()
    } else {
        let presence = match status.presence.as_str() {
            _ if paused => "Paused",
            "idle" => "Idle",
            _ => source_label(&status.state),
        };
        if status.model_parts.is_empty() {
            format!("Codex {presence}")
        } else {
            format!("{} · {presence}", status.model_parts.join(" · "))
        }
    };

    let mut facts: Vec<String> = limits(&status.usage, &status.plan)
        .iter()
        .map(|limit| limit.summary(now))
        .collect();
    if facts.is_empty() {
        facts.push("Waiting for Codex usage…".into());
    }
    if let Some(credits) = credits_number(status.credits.as_deref()) {
        facts.push(format!("Credits {credits}"));
    }
    if !speed.is_empty() {
        facts.push(format!("Mode {speed}"));
    }

    let user = status
        .discord
        .strip_suffix(')')
        .and_then(|text| text.split_once("Connected ("))
        .map(|(_, user)| user);
    let discord = match user {
        None => "Discord not connected".to_string(),
        Some(user) if !active => format!("Connected as {user} · waiting for Codex"),
        Some(user) if paused => format!("Hidden from {user}{resumes}"),
        Some(user) if status.presence == "idle" => format!("Cleared from {user} · Codex is idle"),
        Some(user) => match status.started_at_ms {
            Some(started) => format!(
                "Showing on {user} · {}",
                humanize_duration(now.saturating_sub(started))
            ),
            None => format!("Showing on {user}"),
        },
    };

    TrayModel {
        headline,
        facts,
        paused,
        pause_label: format!("Resume presence{resumes}"),
        hide_model: settings.hide_model,
        update: update.unwrap_or_default(),
        discord,
        can_reconnect: user.is_none(),
    }
}

/// Local time minus UTC. Read once: a DST change is picked up at the next start.
pub(crate) fn local_offset_ms() -> i64 {
    static OFFSET: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *OFFSET.get_or_init(query_offset_minutes) * 60_000
}

#[cfg(windows)]
fn query_offset_minutes() -> i64 {
    use windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
    let (local, utc) = unsafe { (GetLocalTime(), GetSystemTime()) };
    let minutes = |hour: u16, minute: u16| hour as i64 * 60 + minute as i64;
    let day_shift =
        match (local.wYear, local.wMonth, local.wDay).cmp(&(utc.wYear, utc.wMonth, utc.wDay)) {
            std::cmp::Ordering::Greater => 1440,
            std::cmp::Ordering::Less => -1440,
            std::cmp::Ordering::Equal => 0,
        };
    let offset = minutes(local.wHour, local.wMinute) - minutes(utc.wHour, utc.wMinute) + day_shift;
    // The two clocks are read a moment apart; real offsets are multiples of 15 minutes.
    (offset as f64 / 15.0).round() as i64 * 15
}

#[cfg(not(windows))]
fn query_offset_minutes() -> i64 {
    // `date +%z` prints e.g. "+0100".
    let output = std::process::Command::new("/bin/date")
        .arg("+%z")
        .output()
        .ok();
    let text = output.map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());
    text.and_then(|text| {
        let value: i64 = text.get(1..)?.parse().ok()?;
        let minutes = value / 100 * 60 + value % 100;
        Some(if text.starts_with('-') {
            -minutes
        } else {
            minutes
        })
    })
    .unwrap_or(0)
}

/// Days since 1970-01-01 to (month, day); Howard Hinnant's civil-from-days.
fn month_day(days: i64) -> (usize, i64) {
    let z = days + 719_468;
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (month as usize, doy - (153 * mp + 2) / 5 + 1)
}

fn weekday(days: i64) -> &'static str {
    ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][days.rem_euclid(7) as usize]
}

/// "21:14" in local time.
pub(crate) fn local_clock(ms: u64, offset_ms: i64) -> String {
    let minutes = (ms as i64 + offset_ms).div_euclid(60_000);
    format!(
        "{:02}:{:02}",
        minutes.div_euclid(60).rem_euclid(24),
        minutes.rem_euclid(60)
    )
}

/// "Sun 29 Nov, 21:11" in local time.
pub(crate) fn local_date_time(ms: u64, offset_ms: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = (ms as i64 + offset_ms).div_euclid(DAY_MS);
    let (month, day) = month_day(days);
    format!(
        "{} {day} {}, {}",
        weekday(days),
        MONTHS[month - 1],
        local_clock(ms, offset_ms)
    )
}

/// Weekly-limit percentage used on each of the last seven local days, today last.
pub(crate) fn week_days(
    hours: &BTreeMap<u64, f64>,
    now: u64,
    offset_ms: i64,
) -> Vec<(&'static str, f64)> {
    let today = (now as i64 + offset_ms).div_euclid(DAY_MS);
    (today - 6..=today)
        .map(|day| {
            let used = hours
                .iter()
                .filter(|(hour, _)| {
                    (**hour as i64 * 3_600_000 + offset_ms).div_euclid(DAY_MS) == day
                })
                .map(|(_, used)| *used)
                .sum();
            (weekday(day), used)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(line: &str) -> AppStatus {
        crate::parse_status_line(line)
    }

    #[test]
    fn pro_plans_only_expose_the_weekly_limit() {
        let status =
            status("Codex: CLI|GPT-5|Usage: 5h 42% left / week 7% left|Discord: x|prolite");
        let shown = limits(&status.usage, &status.plan);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].figure(), "7%");
        assert_eq!(shown[0].caption(), "of weekly limit left");
        assert_eq!(shown[0].level(), 2);
        // Plus always lists both, even before Codex reports them.
        let pending = limits(&[], "plus");
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].caption(), "5-hour limit unavailable");
        assert_eq!(pending[0].level(), 3);
        assert_eq!(plan_label("prolite"), "Pro 100");
        assert_eq!(plan_label(""), "API key / unknown");
        assert_eq!(plan_label("custom"), "Custom");
    }

    #[test]
    fn preview_state_follows_the_profile_options() {
        let status = status(
            "Codex: Desktop|GPT-6.2-Astra - Max - Fast|Usage: week 89% left / credits 12|Discord: Connected (user)|prolite",
        );
        let mut settings = RpcSettings::default();
        assert_eq!(
            preview_state(&status, &settings),
            "GPT-6.2-Astra - Max - Fast - week 89% - 12 credits"
        );
        settings.show_effort = false;
        settings.show_credits = false;
        settings.show_plan = true;
        assert_eq!(
            preview_state(&status, &settings),
            "GPT-6.2-Astra - Fast - week 89% - ChatGPT Pro 100"
        );
        settings.custom_state = "{model} · {effort} · {5h} · {week} left".into();
        assert_eq!(
            preview_state(&status, &settings),
            "GPT-6.2-Astra · 89% left"
        );
        settings.hide_model = true;
        settings.custom_state.clear();
        assert_eq!(
            preview_state(&status, &settings),
            "Coding - week 89% - ChatGPT Pro 100"
        );
    }

    #[test]
    fn preview_details_show_the_project_unless_the_model_is_hidden() {
        let status = status(
            "Codex: Desktop|GPT-6.2-Astra - Max|Usage: week 89% left|Discord: x|prolite|live||0|codex-rpc",
        );
        let mut settings = RpcSettings::default();
        assert_eq!(
            preview_details(&status, &settings),
            "Using Codex - codex-rpc"
        );
        settings.hide_model = true;
        assert_eq!(preview_details(&status, &settings), "Using Codex");
    }

    #[test]
    fn panel_leads_with_the_lowest_limit_and_explains_discord() {
        let status = status(
            "Codex: CLI|GPT-5 - High - Fast|Usage: 5h 42% left / week 8% left / credits 3|Discord: Connected (user)|plus|live|week=7200000|0",
        );
        let mut settings = RpcSettings::default();
        let panel = panel_view(&status, &settings, Some("0.6.0".into()), 3_600_000);
        assert_eq!(panel.model_line, "GPT-5 · High · Fast");
        assert_eq!((panel.pill, panel.pill_kind), ("CLI", 1));
        assert_eq!(panel.hero.as_ref().unwrap().figure, "8%");
        assert_eq!(panel.banner, "Only 8% of your weekly limit left");
        assert_eq!(panel.banner_level, 2);
        let facts: Vec<(&str, &str)> = panel
            .facts
            .iter()
            .map(|(label, value)| (label.as_str(), value.as_str()))
            .collect();
        assert_eq!(facts, [("5h", "42%"), ("Credits", "3"), ("Mode", "Fast")]);
        assert_eq!(panel.discord, ["Showing on ", "user", " · 1h 0m"]);
        assert_eq!(panel.discord_action, "");
        assert_eq!(panel.update, "0.6.0");

        settings.paused_until_ms = crate::daemon::PAUSE_FOREVER;
        let paused = panel_view(&status, &settings, None, 3_600_000);
        assert_eq!((paused.pill, paused.pill_kind), ("Paused", 2));
        assert_eq!(paused.pause_label, "Presence paused");
        assert_eq!(paused.discord, ["Hidden from ", "user", ""]);
        assert_eq!(paused.discord_action, "resume");

        let off = panel_view(&crate::parse_status_line(""), &settings, None, 0);
        assert_eq!(
            (off.pill, off.model_line.as_str()),
            ("Offline", "Codex is not running")
        );
        assert!(off.hero.is_none() && off.banner.is_empty());
        assert_eq!(off.discord_action, "reconnect");
    }

    #[test]
    fn tray_menu_reports_limits_pause_and_discord() {
        let status = status(
            "Codex: Desktop|GPT-6.2-Astra - Max|Usage: week 89% left / credits 0|Discord: Connected (user)|prolite|live|week=7200000|0",
        );
        let mut settings = RpcSettings::default();
        let model = tray_model(&status, &settings, None, 3_600_000);
        assert_eq!(model.headline, "GPT-6.2-Astra · Max · Desktop");
        assert_eq!(
            model.facts,
            ["89% of weekly limit left · Resets in 1h 0m", "Credits 0"]
        );
        assert_eq!(model.discord, "Showing on user · 1h 0m");
        assert!(!model.paused && !model.can_reconnect);

        settings.paused_until_ms = 3_600_000 + 25 * 60_000;
        let model = tray_model(&status, &settings, Some("0.6.0".into()), 3_600_000);
        assert!(model.paused);
        assert_eq!(model.pause_label, "Resume presence · resumes in 25m");
        assert_eq!(model.discord, "Hidden from user · resumes in 25m");
        assert_eq!(model.update, "0.6.0");

        let off = tray_model(
            &crate::parse_status_line(""),
            &RpcSettings::default(),
            None,
            0,
        );
        assert_eq!(off.headline, "Codex is not running");
        assert_eq!(off.facts, ["Waiting for Codex usage…"]);
        assert!(off.can_reconnect);
    }

    #[test]
    fn local_dates_and_week_days_use_the_offset() {
        // 2026-11-22 22:10:00 UTC is a Sunday; +01:00 keeps the day, +02:00 moves to Monday.
        let ms = 1_795_385_400_000;
        assert_eq!(local_date_time(ms, 3_600_000), "Sun 22 Nov, 23:10");
        assert_eq!(local_date_time(ms, 2 * 3_600_000), "Mon 23 Nov, 00:10");
        assert_eq!(local_clock(ms, -5 * 3_600_000), "17:10");

        let hour = ms / 3_600_000;
        let hours = BTreeMap::from([(hour, 4.0), (hour - 1, 2.0), (hour - 30, 1.0)]);
        let days = week_days(&hours, ms, 3_600_000);
        assert_eq!(days.len(), 7);
        assert_eq!(days[6], ("Sun", 6.0));
        assert_eq!(days[5], ("Sat", 1.0));
        assert_eq!(days[0].0, "Mon");
    }

    #[test]
    fn elapsed_and_credits_are_formatted() {
        let mut status =
            status("Codex: CLI|GPT-5|Usage: week 7% left / credits 1,234.5|Discord: x|plus");
        status.started_at_ms = Some(1_000);
        assert_eq!(
            elapsed(&status, &RpcSettings::default(), 1_000 + 3_727_000),
            "01:02:07 elapsed"
        );
        assert_eq!(
            credits_number(status.credits.as_deref()).as_deref(),
            Some("1,234.5")
        );
        assert_eq!(credits_number(Some("credits none")), None);
    }
}
