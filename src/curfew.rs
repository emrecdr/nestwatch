//! Curfew: a "closed" time window during which the machine must not be on.
//!
//! The window is stored as two `HH:MM` local times and may wrap past midnight
//! (e.g. 22:00 → 07:00). The pure [`is_within`] check is separated from the clock so it
//! can be unit-tested exhaustively; [`Curfew::is_active_now`] applies it to local time.

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use chrono::{Datelike, Local, NaiveTime, Weekday};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::control::SystemControl;

fn default_warn_secs() -> u32 {
    60
}

/// How often the enforcer re-checks the clock.
const CHECK_INTERVAL: Duration = Duration::from_secs(30);

/// Upper bound on the warning countdown (10 min). A too-large value would let the shutdown
/// fire well outside the window (or effectively never), defeating enforcement.
const MAX_WARN_SECS: u32 = 600;

/// Which weekdays a [`Window`] applies to. An all-false selector means **every day** — that's
/// the common case and also what an omitted `days` deserializes to.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Days {
    #[serde(default)]
    pub mon: bool,
    #[serde(default)]
    pub tue: bool,
    #[serde(default)]
    pub wed: bool,
    #[serde(default)]
    pub thu: bool,
    #[serde(default)]
    pub fri: bool,
    #[serde(default)]
    pub sat: bool,
    #[serde(default)]
    pub sun: bool,
}

impl Days {
    fn any(&self) -> bool {
        self.mon || self.tue || self.wed || self.thu || self.fri || self.sat || self.sun
    }

    /// Whether `wd` is selected. An empty selector matches every day.
    fn includes(&self, wd: Weekday) -> bool {
        if !self.any() {
            return true;
        }
        match wd {
            Weekday::Mon => self.mon,
            Weekday::Tue => self.tue,
            Weekday::Wed => self.wed,
            Weekday::Thu => self.thu,
            Weekday::Fri => self.fri,
            Weekday::Sat => self.sat,
            Weekday::Sun => self.sun,
        }
    }
}

/// A single closed window: `[start, end)` local time (may wrap midnight) on the selected days.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    pub start: String,
    pub end: String,
    #[serde(default)]
    pub days: Days,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Curfew {
    pub enabled: bool,
    /// Legacy single-window start, `HH:MM` local time. Used only when `windows` is empty.
    pub start: String,
    /// Legacy single-window end, `HH:MM` local time. Used only when `windows` is empty.
    pub end: String,
    /// Grace period (Windows shows a countdown + message) before power-off.
    #[serde(default = "default_warn_secs")]
    pub warn_secs: u32,
    /// Per-day windows. When non-empty these are authoritative and the legacy `start`/`end`
    /// above are ignored; when empty, the legacy single window is used. Kept as a separate
    /// field (rather than a breaking rename) so existing `config.json` files still load.
    #[serde(default)]
    pub windows: Vec<Window>,
}

impl Default for Curfew {
    fn default() -> Self {
        Self {
            enabled: false,
            start: "22:00".into(),
            end: "07:00".into(),
            warn_secs: default_warn_secs(),
            windows: Vec::new(),
        }
    }
}

impl Curfew {
    /// Is the *current* local time inside the closed window? `false` if disabled or if the
    /// times are unparseable (fail-open, so a bad config never bricks the machine).
    /// Invalid times are logged once at config load, not here (this runs every tick).
    pub fn is_active_now(&self) -> bool {
        if !self.enabled {
            return false;
        }
        let now = Local::now();
        if !self.windows.is_empty() {
            return any_window_active(&self.windows, now.time(), now.weekday());
        }
        match (parse_hm(&self.start), parse_hm(&self.end)) {
            (Some(start), Some(end)) => is_within(now.time(), start, end),
            _ => false,
        }
    }

    /// Validate the settings (used when accepting them from the UI and at config load). When
    /// `windows` is non-empty each window is checked; otherwise the legacy `start`/`end` are.
    pub fn validate(&self) -> Result<(), String> {
        if self.warn_secs > MAX_WARN_SECS {
            return Err(format!("warning seconds must be <= {MAX_WARN_SECS}"));
        }
        if self.windows.is_empty() {
            if parse_hm(&self.start).is_none() {
                return Err(format!("invalid start time: {}", self.start));
            }
            if parse_hm(&self.end).is_none() {
                return Err(format!("invalid end time: {}", self.end));
            }
        } else {
            for (i, w) in self.windows.iter().enumerate() {
                if parse_hm(&w.start).is_none() {
                    return Err(format!("window {}: invalid start time: {}", i + 1, w.start));
                }
                if parse_hm(&w.end).is_none() {
                    return Err(format!("window {}: invalid end time: {}", i + 1, w.end));
                }
            }
        }
        Ok(())
    }
}

/// What the enforcer decides to do on a given tick.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    /// (Re)issue a warned shutdown.
    Shutdown,
    /// Cancel a pending shutdown.
    Abort,
    /// Do nothing this tick.
    None,
}

/// Deadline-based enforcement state machine, split from the clock/loop so it is fully
/// unit-testable. `deadline` is when the currently-scheduled shutdown *should* have
/// completed; `None` means no shutdown is believed pending.
struct Enforcer {
    deadline: Option<Instant>,
}

impl Enforcer {
    fn new() -> Self {
        Self { deadline: None }
    }

    /// Decide the action for this tick.
    ///
    /// - Entering the window schedules a shutdown.
    /// - If we're still on `slack` past the deadline, the shutdown was cancelled (e.g. the
    ///   child ran `shutdown /a`) or failed, so we re-issue — this is what makes curfew
    ///   robust rather than a one-shot latch.
    /// - Leaving the window aborts any pending shutdown.
    fn tick(&mut self, active: bool, now: Instant, warn: Duration, slack: Duration) -> Action {
        if active {
            match self.deadline {
                None => {
                    self.deadline = Some(now + warn);
                    Action::Shutdown
                }
                Some(deadline) if now >= deadline + slack => {
                    self.deadline = Some(now + warn);
                    Action::Shutdown
                }
                Some(_) => Action::None,
            }
        } else if self.deadline.take().is_some() {
            Action::Abort
        } else {
            Action::None
        }
    }

    /// Clear the armed state so the next active tick re-issues (used when a shutdown call
    /// failed and nothing is actually pending).
    fn disarm(&mut self) {
        self.deadline = None;
    }
}

/// Background loop: every [`CHECK_INTERVAL`], enforce the curfew window. Runs for the life
/// of the server; it never returns (a caller that `spawn`s it should log if it ever does).
pub async fn run_enforcer(control: Arc<dyn SystemControl>, config: Arc<RwLock<Config>>) {
    let mut enforcer = Enforcer::new();
    let mut ticker = tokio::time::interval(CHECK_INTERVAL);
    loop {
        ticker.tick().await;

        let (active, warn_secs) = {
            let guard = crate::state::recover_read(&config);
            (guard.curfew.is_active_now(), guard.curfew.warn_secs)
        };
        let warn = Duration::from_secs(warn_secs as u64);

        match enforcer.tick(active, Instant::now(), warn, CHECK_INTERVAL) {
            Action::Shutdown => {
                tracing::warn!("curfew active — scheduling shutdown ({warn_secs}s warning)");
                let control = control.clone();
                let msg = "Curfew: this computer is shutting down.".to_string();
                match tokio::task::spawn_blocking(move || control.shutdown(warn_secs, Some(msg)))
                    .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        tracing::error!(error = %e, "curfew shutdown failed; will retry");
                        enforcer.disarm();
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "curfew shutdown task panicked; will retry");
                        enforcer.disarm();
                    }
                }
            }
            Action::Abort => {
                tracing::info!("curfew window ended — aborting any pending shutdown");
                let control = control.clone();
                if let Ok(Err(e)) =
                    tokio::task::spawn_blocking(move || control.abort_shutdown()).await
                {
                    tracing::warn!(error = %e, "failed to abort shutdown");
                }
            }
            Action::None => {}
        }
    }
}

/// Parse `"HH:MM"` (24-hour) into a `NaiveTime`.
fn parse_hm(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M").ok()
}

/// Whether any window covers `now` on `today` — the multi-window evaluator. Pure/testable:
/// a window matches when its `days` selector includes `today` and `now` is within its
/// `[start, end)` range. Unparseable times in a window are treated as non-matching (fail-open).
fn any_window_active(windows: &[Window], now: NaiveTime, today: Weekday) -> bool {
    windows.iter().any(|w| {
        w.days.includes(today)
            && matches!(
                (parse_hm(&w.start), parse_hm(&w.end)),
                (Some(s), Some(e)) if is_within(now, s, e)
            )
    })
}

/// Whether `now` falls in `[start, end)`, treating `start > end` as a window that wraps
/// midnight. An empty window (`start == end`) is never active.
fn is_within(now: NaiveTime, start: NaiveTime, end: NaiveTime) -> bool {
    use std::cmp::Ordering;
    match start.cmp(&end) {
        Ordering::Less => now >= start && now < end, // same day, e.g. 09:00–17:00
        Ordering::Greater => now >= start || now < end, // wraps midnight, e.g. 22:00–07:00
        Ordering::Equal => false,                    // empty window
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    #[test]
    fn same_day_window() {
        let (s, e) = (t(9, 0), t(17, 0));
        assert!(!is_within(t(8, 59), s, e));
        assert!(is_within(t(9, 0), s, e)); // inclusive start
        assert!(is_within(t(12, 0), s, e));
        assert!(!is_within(t(17, 0), s, e)); // exclusive end
        assert!(!is_within(t(23, 0), s, e));
    }

    #[test]
    fn window_wraps_midnight() {
        let (s, e) = (t(22, 0), t(7, 0));
        assert!(is_within(t(22, 0), s, e)); // inclusive start
        assert!(is_within(t(23, 59), s, e));
        assert!(is_within(t(0, 0), s, e));
        assert!(is_within(t(6, 59), s, e));
        assert!(!is_within(t(7, 0), s, e)); // exclusive end
        assert!(!is_within(t(12, 0), s, e));
    }

    #[test]
    fn empty_window_is_never_active() {
        let x = t(10, 0);
        assert!(!is_within(x, x, x));
    }

    #[test]
    fn parses_and_rejects_times() {
        assert!(parse_hm("07:30").is_some());
        assert!(parse_hm("23:59").is_some());
        assert!(parse_hm("24:00").is_none());
        assert!(parse_hm("7:5").is_some()); // %H:%M accepts single digits
        assert!(parse_hm("nope").is_none());
    }

    #[test]
    fn validate_rejects_bad_times_and_huge_warn() {
        let ok = Curfew {
            enabled: true,
            start: "22:00".into(),
            end: "07:00".into(),
            warn_secs: 60,
            windows: Vec::new(),
        };
        assert!(ok.validate().is_ok());
        let bad_time = Curfew {
            start: "25:00".into(),
            ..ok.clone()
        };
        assert!(bad_time.validate().is_err());
        let huge_warn = Curfew {
            warn_secs: MAX_WARN_SECS + 1,
            ..ok.clone()
        };
        assert!(huge_warn.validate().is_err());
    }

    // ---- Multi-window + per-day-of-week ----

    fn win(start: &str, end: &str, days: Days) -> Window {
        Window {
            start: start.into(),
            end: end.into(),
            days,
        }
    }

    fn only(day: Weekday) -> Days {
        let mut d = Days::default();
        match day {
            Weekday::Mon => d.mon = true,
            Weekday::Tue => d.tue = true,
            Weekday::Wed => d.wed = true,
            Weekday::Thu => d.thu = true,
            Weekday::Fri => d.fri = true,
            Weekday::Sat => d.sat = true,
            Weekday::Sun => d.sun = true,
        }
        d
    }

    #[test]
    fn empty_days_selector_matches_every_day() {
        let ws = vec![win("22:00", "07:00", Days::default())];
        assert!(any_window_active(&ws, t(23, 0), Weekday::Mon));
        assert!(any_window_active(&ws, t(23, 0), Weekday::Sun));
        assert!(!any_window_active(&ws, t(12, 0), Weekday::Mon)); // outside the time range
    }

    #[test]
    fn window_respects_weekday_selection() {
        let ws = vec![win("22:00", "23:59", only(Weekday::Fri))];
        assert!(any_window_active(&ws, t(22, 30), Weekday::Fri));
        assert!(!any_window_active(&ws, t(22, 30), Weekday::Sat)); // wrong day
    }

    #[test]
    fn any_of_several_windows_can_match() {
        let ws = vec![
            win("09:00", "12:00", only(Weekday::Mon)),
            win("20:00", "22:00", only(Weekday::Wed)),
        ];
        assert!(any_window_active(&ws, t(21, 0), Weekday::Wed));
        assert!(any_window_active(&ws, t(10, 0), Weekday::Mon));
        assert!(!any_window_active(&ws, t(21, 0), Weekday::Mon)); // Mon window is 09–12
    }

    #[test]
    fn windows_authoritative_and_legacy_json_still_loads() {
        // A legacy config with no `windows` key deserializes with an empty vec (legacy path).
        let legacy = r#"{"enabled":true,"start":"22:00","end":"07:00","warn_secs":45}"#;
        let c: Curfew = serde_json::from_str(legacy).unwrap();
        assert!(c.windows.is_empty());
        assert_eq!(c.warn_secs, 45);
        assert!(c.validate().is_ok());

        // A windowed config validates per-window and rejects a bad one.
        let windowed = Curfew {
            windows: vec![win("21:00", "06:00", only(Weekday::Fri))],
            ..Curfew::default()
        };
        assert!(windowed.validate().is_ok());
        let bad = Curfew {
            windows: vec![win("99:99", "06:00", Days::default())],
            ..Curfew::default()
        };
        assert!(bad.validate().is_err());
    }

    // ---- Enforcer state machine ----

    const WARN: Duration = Duration::from_secs(60);
    const SLACK: Duration = Duration::from_secs(30);

    #[test]
    fn enforcer_arms_once_on_entry_then_stays_quiet() {
        let base = Instant::now();
        let mut e = Enforcer::new();
        // Enter the window → schedule a shutdown.
        assert_eq!(e.tick(true, base, WARN, SLACK), Action::Shutdown);
        // Subsequent ticks before the deadline do nothing (countdown in progress).
        assert_eq!(
            e.tick(true, base + Duration::from_secs(30), WARN, SLACK),
            Action::None
        );
        assert_eq!(
            e.tick(true, base + Duration::from_secs(60), WARN, SLACK),
            Action::None
        );
    }

    #[test]
    fn enforcer_reissues_if_still_on_past_deadline() {
        // Simulates the child running `shutdown /a`: still active well past when the machine
        // should have powered off → re-issue.
        let base = Instant::now();
        let mut e = Enforcer::new();
        assert_eq!(e.tick(true, base, WARN, SLACK), Action::Shutdown); // deadline = base+60
        // base+90 = deadline(60) + slack(30) → re-issue.
        assert_eq!(
            e.tick(true, base + Duration::from_secs(90), WARN, SLACK),
            Action::Shutdown
        );
    }

    #[test]
    fn enforcer_aborts_when_window_ends_while_armed() {
        let base = Instant::now();
        let mut e = Enforcer::new();
        assert_eq!(e.tick(true, base, WARN, SLACK), Action::Shutdown);
        // Window ends (curfew disabled or time passed) → cancel the pending shutdown.
        assert_eq!(
            e.tick(false, base + Duration::from_secs(10), WARN, SLACK),
            Action::Abort
        );
        // Nothing pending anymore.
        assert_eq!(
            e.tick(false, base + Duration::from_secs(20), WARN, SLACK),
            Action::None
        );
    }

    #[test]
    fn enforcer_disarm_forces_reissue_next_active_tick() {
        let base = Instant::now();
        let mut e = Enforcer::new();
        assert_eq!(e.tick(true, base, WARN, SLACK), Action::Shutdown);
        e.disarm(); // simulate a failed shutdown call
        assert_eq!(
            e.tick(true, base + Duration::from_secs(5), WARN, SLACK),
            Action::Shutdown
        );
    }
}
