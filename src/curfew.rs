//! Curfew: a "closed" time window during which the machine must not be on.
//!
//! The window is stored as two `HH:MM` local times and may wrap past midnight
//! (e.g. 22:00 → 07:00). The pure [`is_within`] check is separated from the clock so it
//! can be unit-tested exhaustively; [`Curfew::is_active_now`] applies it to local time.

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Datelike, FixedOffset, NaiveTime, TimeDelta, Weekday};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::control::SystemControl;
use crate::countdown::{Countdown, LOOKAHEAD_MINS};

/// Default warning countdown (seconds). Shared with the rules enforcer's serde default.
pub fn default_warn_secs() -> u32 {
    60
}

/// How often the enforcer re-checks the clock.
const CHECK_INTERVAL: Duration = Duration::from_secs(30);

/// Upper bound on the warning countdown (10 min). A too-large value would let the shutdown
/// fire well outside the window (or effectively never), defeating enforcement. Shared with the
/// rules enforcer, which bounds its budget warning the same way.
pub const MAX_WARN_SECS: u32 = 600;

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
        // Trusted clock, not `Local::now()`: the curfew window is exactly what a child would
        // shift the timezone to escape, and doing so also cancels a pending shutdown.
        self.is_active_at(crate::clock::now())
    }

    /// Is `at` inside a closed window? The clock-free half of [`Curfew::is_active_now`], split
    /// out so [`Curfew::mins_until_active`] can ask about times other than "now" — and so the
    /// window/day-selector logic is directly testable without touching the process clock.
    pub fn is_active_at(&self, at: DateTime<FixedOffset>) -> bool {
        if !self.enabled {
            return false;
        }
        if !self.windows.is_empty() {
            return any_window_active(&self.windows, at.time(), at.weekday());
        }
        match (parse_hm(&self.start), parse_hm(&self.end)) {
            (Some(start), Some(end)) => is_within(at.time(), start, end),
            _ => false,
        }
    }

    /// Minutes until the next closed window opens, or `None` if that's further out than
    /// [`countdown::LOOKAHEAD_MINS`] — or if curfew is off, or the window is already open (where
    /// Windows' own shutdown countdown has taken over and a "bedtime soon" popup would be a lie).
    ///
    /// Deliberately **probes** [`Curfew::is_active_at`] minute by minute instead of deriving the
    /// next start time from `windows` directly. Doing the arithmetic would mean re-deriving day
    /// selectors and the midnight wrap — the two places an off-by-one would hide, and both already
    /// solved and swept by tests here. Fifteen evaluations of a pure function every 30 seconds is
    /// nothing, and it stays correct for free if the window model ever grows.
    pub fn mins_until_active(&self, now: DateTime<FixedOffset>) -> Option<u32> {
        if self.is_active_at(now) {
            return None;
        }
        (1..=LOOKAHEAD_MINS).find(|&m| self.is_active_at(now + TimeDelta::minutes(m.into())))
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
    /// Issue the first, warned shutdown of this curfew episode — the child gets a countdown.
    Shutdown,
    /// Re-issue **immediately**, with no countdown.
    ///
    /// Reaching here means the previous shutdown didn't happen: on a client Windows a standard
    /// user holds `SeShutdownPrivilege`, so `shutdown /a` cancels one. Re-issuing with the same
    /// warning simply handed them another window to cancel, and a five-line loop
    /// (`for /l %i in () do (shutdown /a & timeout /t 5)`) beat the 30-second tick forever.
    /// A zero-delay shutdown has no pending window, so there is nothing left to abort.
    ShutdownNow,
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
                    // Still running well past when the machine should have powered off, so the
                    // shutdown was cancelled or failed. Don't offer another cancellable window.
                    self.deadline = Some(now + warn);
                    Action::ShutdownNow
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
pub async fn run_enforcer(
    control: Arc<dyn SystemControl>,
    config: Arc<RwLock<Config>>,
    usage_log: Arc<crate::usage::UsageLog>,
) {
    let mut enforcer = Enforcer::new();
    let mut countdown = crate::countdown::Countdown::default();
    let mut ticker = tokio::time::interval(CHECK_INTERVAL);
    // See the note in `rules`: without this a resume from sleep replays every missed tick.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        crate::heartbeat::tick(&mut ticker, crate::heartbeat::Enforcer::Curfew).await;

        let (active, warn_secs, warning) = {
            let guard = crate::state::recover_read(&config);
            let curfew = &guard.curfew;
            // One clock reading for both questions, so "is it bedtime?" and "how soon?" can't
            // disagree across a tick boundary.
            let now = crate::clock::now();
            let active = curfew.is_active_at(now);
            let warning = bedtime_warning(&mut countdown, curfew, now);
            (active, curfew.warn_secs, warning)
        };
        let warn = Duration::from_secs(warn_secs as u64);

        // Advance heads-up before the window opens, so the shutdown dialog isn't the first
        // the child hears of bedtime.
        if let Some(mins) = warning
            && crate::control::notify(&control, "Bedtime", &bedtime_message(mins)).await
        {
            // Recorded only on delivery — see the same reasoning in `rules`.
            usage_log.record(
                "curfew_countdown",
                serde_json::json!({ "minutes_remaining": mins }),
            );
        }

        match enforcer.tick(active, Instant::now(), warn, CHECK_INTERVAL) {
            action @ (Action::Shutdown | Action::ShutdownNow) => {
                // The first issue warns; a re-issue means the last one was cancelled, so it goes
                // immediately (see `Action::ShutdownNow`).
                let delay = if action == Action::Shutdown {
                    tracing::warn!("curfew active — scheduling shutdown ({warn_secs}s warning)");
                    warn_secs
                } else {
                    tracing::warn!(
                        "curfew shutdown did not happen (cancelled?) — shutting down now"
                    );
                    0
                };
                let control = control.clone();
                let msg = "Curfew: this computer is shutting down.".to_string();
                match tokio::task::spawn_blocking(move || control.shutdown(delay, Some(msg))).await
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
                match tokio::task::spawn_blocking(move || control.abort_shutdown()).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => tracing::warn!(error = %e, "failed to abort shutdown"),
                    // A panicked worker means the abort did NOT happen and the machine powers off
                    // outside its window. This arm used to be absent, so that outcome was silent.
                    Err(e) => tracing::error!(error = %e, "shutdown abort task panicked"),
                }
            }
            Action::None => {}
        }
    }
}

/// The bedtime warning to announce this tick, if any — the whole of the enforcer's countdown
/// decision, split out of the loop so an entire evening's approach can be simulated in tests
/// rather than only ever observed on a child's laptop at 21:45.
fn bedtime_warning(
    countdown: &mut Countdown,
    curfew: &Curfew,
    now: DateTime<FixedOffset>,
) -> Option<u32> {
    // Curfew off, or already bedtime — where Windows' own shutdown dialog has taken over and a
    // "bedtime soon" popup would be a lie. Nothing to count down to, so re-prime rather than
    // announce. Deriving this here rather than taking it as a parameter keeps it tied to the same
    // `now` the reading below is measured from.
    if !curfew.enabled || curfew.is_active_at(now) {
        countdown.reset();
        return None;
    }
    // `None` here means "further off than we can see", which is a reading, not an absence — see
    // [`Countdown::observe_upcoming`], which owns the sentinel that distinction needs.
    countdown.observe_upcoming(curfew.mins_until_active(now))
}

/// What the child is told as bedtime approaches. `mins` is one of
/// [`crate::countdown::WARN_AT_MINS`]; mirrors `rules::budget_countdown_message`.
fn bedtime_message(mins: u32) -> String {
    match mins {
        1 => "Bedtime in 1 minute!".to_string(),
        5 => "Bedtime in 5 minutes — good time to save.".to_string(),
        m => format!("Bedtime in {m} minutes."),
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
    fn is_within_matches_modular_oracle_across_the_day() {
        // An independent oracle via modular arithmetic (a different formulation than the
        // branch-on-`cmp` implementation), swept over every minute of the day for same-day,
        // midnight-wrapping, and empty windows.
        fn oracle(m: i32, s: i32, e: i32) -> bool {
            let span = (e - s).rem_euclid(1440); // window length (0 = empty)
            let off = (m - s).rem_euclid(1440); // how far m is past the start
            span != 0 && off < span
        }
        let nt =
            |min: i32| NaiveTime::from_hms_opt((min / 60) as u32, (min % 60) as u32, 0).unwrap();
        for &(s, e) in &[
            (9 * 60, 17 * 60),          // same day
            (22 * 60, 7 * 60),          // wraps midnight
            (0, 0),                     // empty
            (23 * 60 + 30, 30),         // wraps, short
            (6 * 60 + 15, 6 * 60 + 15), // empty, non-midnight
        ] {
            for m in 0..1440 {
                assert_eq!(
                    is_within(nt(m), nt(s), nt(e)),
                    oracle(m, s, e),
                    "m={m} s={s} e={e}"
                );
            }
        }
    }

    /// A local `DateTime` at the given date and time, UTC offset (the offset is irrelevant here —
    /// windows are compared against local wall-clock time, which is what `is_active_at` reads).
    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> chrono::DateTime<chrono::FixedOffset> {
        chrono::NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
            .and_local_timezone(chrono::FixedOffset::east_opt(0).unwrap())
            .unwrap()
    }

    fn nightly(start: &str, end: &str) -> Curfew {
        Curfew {
            enabled: true,
            start: start.into(),
            end: end.into(),
            ..Default::default()
        }
    }

    #[test]
    fn mins_until_active_counts_down_to_the_window() {
        let c = nightly("22:00", "07:00");
        assert_eq!(c.mins_until_active(at(2026, 7, 9, 21, 45)), Some(15));
        assert_eq!(c.mins_until_active(at(2026, 7, 9, 21, 55)), Some(5));
        assert_eq!(c.mins_until_active(at(2026, 7, 9, 21, 59)), Some(1));
    }

    /// Beyond the lookahead there is nothing to say. The loop feeds one past the longest
    /// threshold in this case, so entering the band is still a real crossing.
    #[test]
    fn mins_until_active_is_none_outside_the_lookahead() {
        let c = nightly("22:00", "07:00");
        assert_eq!(c.mins_until_active(at(2026, 7, 9, 21, 44)), None);
        assert_eq!(c.mins_until_active(at(2026, 7, 9, 15, 0)), None);
    }

    /// Once the window is open, Windows' own shutdown countdown is on screen — a "bedtime soon"
    /// popup on top of it would be both redundant and wrong.
    #[test]
    fn mins_until_active_is_none_once_the_window_is_open() {
        let c = nightly("22:00", "07:00");
        assert_eq!(c.mins_until_active(at(2026, 7, 9, 22, 0)), None);
        assert_eq!(c.mins_until_active(at(2026, 7, 10, 3, 0)), None);
    }

    #[test]
    fn mins_until_active_is_none_when_curfew_is_off() {
        let c = Curfew {
            enabled: false,
            ..nightly("22:00", "07:00")
        };
        assert_eq!(c.mins_until_active(at(2026, 7, 9, 21, 50)), None);
    }

    /// The whole reason the lookahead probes `is_active_at` instead of computing the next start
    /// time: a window that opens just after midnight has to be seen from the evening before, and
    /// on a *different weekday* than "now". Deriving that by hand is where an off-by-one hides.
    #[test]
    fn mins_until_active_sees_across_midnight_and_into_the_next_weekday() {
        let sunday = chrono::NaiveDate::from_ymd_opt(2026, 7, 5).unwrap();
        assert_eq!(sunday.weekday(), Weekday::Sun, "test fixture sanity");

        let monday_only = Curfew {
            enabled: true,
            windows: vec![Window {
                start: "00:00".into(),
                end: "07:00".into(),
                days: Days {
                    mon: true,
                    ..Default::default()
                },
            }],
            ..Default::default()
        };
        // Sunday 23:50 — ten minutes out, and the window belongs to Monday.
        assert_eq!(
            monday_only.mins_until_active(at(2026, 7, 5, 23, 50)),
            Some(10)
        );
        // Saturday 23:50 is the same clock time but the wrong day: nothing is coming.
        assert_eq!(monday_only.mins_until_active(at(2026, 7, 4, 23, 50)), None);
    }

    /// Simulate a real evening: tick at the enforcer's cadence from well before bedtime until
    /// after the window opens, collecting what the child hears — the end-to-end behaviour of the
    /// countdown, minus the loop's I/O.
    ///
    /// Takes the `Countdown` so one can span several configs, which is what a parent changing the
    /// settings mid-evening looks like.
    fn evening(
        countdown: &mut Countdown,
        curfew: &Curfew,
        from: DateTime<FixedOffset>,
        ticks: i64,
    ) -> Vec<u32> {
        let tick = TimeDelta::from_std(CHECK_INTERVAL).expect("check interval fits a TimeDelta");
        (0..ticks)
            .filter_map(|i| bedtime_warning(countdown, curfew, from + tick * i as i32))
            .collect()
    }

    /// [`evening`] over a fresh countdown — the common case.
    fn one_evening(curfew: &Curfew, from: DateTime<FixedOffset>, ticks: i64) -> Vec<u32> {
        evening(&mut Countdown::default(), curfew, from, ticks)
    }

    #[test]
    fn bedtime_countdown_announces_each_threshold_once_across_the_evening() {
        let c = nightly("22:00", "07:00");
        // 21:30 → 22:05, so the run starts outside the lookahead and ends inside the window.
        let announced = one_evening(&c, at(2026, 7, 9, 21, 30), 70);
        assert_eq!(announced, crate::countdown::WARN_AT_MINS.to_vec());
    }

    /// The window opening must silence the countdown, not have it keep talking over Windows'
    /// shutdown dialog — and leaving the window must not replay the warnings on the way out.
    #[test]
    fn bedtime_countdown_is_silent_through_and_after_the_window() {
        let c = nightly("22:00", "23:00");
        // 22:00 → 23:30: starts inside the window, runs past its end.
        assert_eq!(
            one_evening(&c, at(2026, 7, 9, 22, 0), 180),
            Vec::<u32>::new()
        );
    }

    /// Switching curfew **on** inside the lookahead must not announce a threshold the child never
    /// crossed: ten minutes before bedtime, "15 minutes" would be a lie. Caught by mutation
    /// testing — dropping the `enabled` guard leaves the countdown parked just outside the band
    /// while curfew is off, so enabling it late reads as a fresh crossing of 15.
    #[test]
    fn enabling_curfew_close_to_bedtime_does_not_announce_a_stale_threshold() {
        let on = nightly("22:00", "07:00");
        let off = Curfew {
            enabled: false,
            ..nightly("22:00", "07:00")
        };
        // One countdown spanning both configs — the parent flipping the switch mid-evening.
        let cd = &mut Countdown::default();
        let mut announced = evening(cd, &off, at(2026, 7, 9, 21, 30), 40); // off through 21:30–21:50
        announced.extend(evening(cd, &on, at(2026, 7, 9, 21, 50), 30)); // on, ten minutes to go

        assert_eq!(
            announced,
            vec![5, 1],
            "bedtime was never 15 minutes away while curfew was actually on"
        );
    }

    #[test]
    fn a_disabled_curfew_never_announces_anything() {
        let c = Curfew {
            enabled: false,
            ..nightly("22:00", "07:00")
        };
        assert_eq!(
            one_evening(&c, at(2026, 7, 9, 21, 30), 70),
            Vec::<u32>::new()
        );
    }

    #[test]
    fn bedtime_messages_read_naturally_at_every_threshold() {
        for &m in &crate::countdown::WARN_AT_MINS {
            let msg = bedtime_message(m);
            assert!(
                msg.contains(&m.to_string()),
                "{msg} should name the minutes"
            );
            assert!(
                !msg.contains("1 minutes"),
                "singular must not be pluralised: {msg}"
            );
        }
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
        // base+90 = deadline(60) + slack(30) → re-issue, and it must be the UNCANCELLABLE kind.
        // Re-issuing another warned countdown was the bug: it handed the child a fresh window to
        // `shutdown /a`, so a loop beat the 30s tick indefinitely.
        assert_eq!(
            e.tick(true, base + Duration::from_secs(90), WARN, SLACK),
            Action::ShutdownNow
        );
        // And it stays uncancellable for as long as they keep cancelling.
        for i in 1..=5 {
            let t = base + Duration::from_secs(90 + 91 * i);
            assert_eq!(
                e.tick(true, t, WARN, SLACK),
                Action::ShutdownNow,
                "cancel attempt {i} must not earn another countdown"
            );
        }
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
