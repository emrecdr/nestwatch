//! Curfew: a "closed" time window during which the machine must not be on.
//!
//! The window is stored as two `HH:MM` local times and may wrap past midnight
//! (e.g. 22:00 → 07:00). The pure [`is_within`] check is separated from the clock so it
//! can be unit-tested exhaustively; [`Curfew::is_active_now`] applies it to local time.

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Datelike, FixedOffset, NaiveTime, TimeDelta, Weekday};
use serde::{Deserialize, Serialize};

use crate::config::{Config, Language};
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
    /// Bedtime is suppressed until this instant — a parent-granted "half an hour more tonight".
    ///
    /// **An absolute instant, deliberately, and not the `date + minutes` shape `DailyGrant` uses
    /// for screen time.** Bedtime crosses midnight. A grant made at 23:50 and keyed on "today"
    /// would expire when the date rolled over and slam the window shut ten minutes later — the
    /// parent having been told they had thirty. An instant has no such edge, and it self-cleans:
    /// one in the past is simply inert, so there is no reset to forget.
    ///
    /// Lives on `Curfew` rather than beside `Config::extra` so that [`Curfew::is_active_at`] can
    /// honour it, which makes **every** reader correct by construction — the enforcer, the
    /// budget-shutdown abort coordination in `rules`, the bedtime countdown, and
    /// [`Curfew::cuts_grant_short_in`]. Suppressing it at one call site and not the others is
    /// precisely the class of bug this feature exists to fix: the machine would stay up and the
    /// *budget* enforcer would shut it down instead.
    ///
    /// Not sent by the dashboard's curfew form; `api::set_curfew` carries the stored value across
    /// a save, the way `port` and `password_hash` are never written by a handler.
    #[serde(default)]
    pub extra_until: Option<DateTime<FixedOffset>>,
}

impl Default for Curfew {
    fn default() -> Self {
        Self {
            enabled: false,
            start: "22:00".into(),
            end: "07:00".into(),
            warn_secs: default_warn_secs(),
            windows: Vec::new(),
            extra_until: None,
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
        // A parent-granted extension suppresses the window up to its instant. Checked here, in
        // the one function every caller goes through, so the enforcer, the abort coordination in
        // `rules`, the bedtime countdown and `cuts_grant_short_in` cannot disagree about whether
        // the machine is supposed to be off. Probing a time past the instant correctly reports
        // the window as active again, so an extension delays bedtime rather than cancelling it.
        if self.extra_until.is_some_and(|until| at < until) {
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
    /// [`crate::countdown::LOOKAHEAD_MINS`] — or if curfew is off, or the window is already open (where
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

    /// How soon a grant of `minutes` made at `now` runs into a closed window — `None` when curfew
    /// will not interrupt it at all, and `Some(0)` when the window is **already open**.
    ///
    /// Screen time and bedtime are independent limits: nothing in this module reads
    /// `Config::extra`, so granting minutes cannot move a curfew, and
    /// `rules::should_abort_budget_shutdown` deliberately declines to cancel a shutdown while a
    /// window is open — curfew stays the sole authority over the one OS shutdown slot. Both are
    /// correct and both are invisible, which is the problem this exists to fix: a parent approves
    /// a request at 22:05, the machine powers off anyway, and the tool has told them nothing. The
    /// two places that make the promise can now check whether it can be kept.
    ///
    /// Probes minute by minute rather than deriving the next start, for the reason
    /// [`mins_until_active`](Self::mins_until_active) gives at length — the midnight wrap and the
    /// day selectors are where an off-by-one hides, and `is_active_at` has already solved both.
    /// Bounded by `minutes`, which both callers cap at `timereq::MAX_REQUEST_MINUTES` (240), so
    /// this is at most 240 evaluations of a pure function on a button a parent pressed.
    ///
    /// Deliberately **not** [`mins_until_active`](Self::mins_until_active): that one stops at
    /// `LOOKAHEAD_MINS` (15) because it feeds a "bedtime soon" popup, and a 30-minute grant made
    /// at 21:40 has to see a 22:00 window that is twenty minutes out.
    pub fn cuts_grant_short_in(&self, now: DateTime<FixedOffset>, minutes: u32) -> Option<u32> {
        // `is_active_at` returns false when curfew is disabled, so `enabled` needs no separate
        // check here.
        if self.is_active_at(now) {
            return Some(0);
        }
        (1..=minutes).find(|&m| self.is_active_at(now + TimeDelta::minutes(m.into())))
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
    /// The heads-up state machine, owned here rather than by the loop.
    ///
    /// It used to be a local in `run_enforcer`, which meant no single function could answer "what
    /// should curfew do this tick" — the shutdown machine and the warning machine were joined only
    /// by the loop body. Any rule coupling them had nowhere to live and no way to be tested:
    /// *don't warn while a shutdown is pending*, *re-arm when one is aborted*. Each would have
    /// landed in the loop as an ad-hoc `if`, invisible to the tests. The rules enforcer can pin
    /// that class of interaction (`countdown_is_silent_once_the_budget_is_spent`) because both
    /// outcomes come out of one call; this one structurally could not.
    countdown: Countdown,
}

/// What the loop learned about the **next** curfew window this tick.
///
/// Passed in rather than derived here, so the enforcer stays free of both the config and the clock
/// — the property that makes every case below a unit test rather than something discovered on a
/// child's PC at bedtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Upcoming {
    /// Nothing to count down to: curfew is off, or it is already bedtime and Windows' own shutdown
    /// dialog has taken over, where a "bedtime soon" popup would be a lie.
    ///
    /// Distinct from `In(None)`, and the difference is load-bearing. This **re-primes** the
    /// countdown, so the first reading afterwards announces nothing. `In(None)` records a real
    /// observation of "further off than we can see", from which the next reading *can* warn.
    Nothing,
    /// The next window is this many minutes away — `None` meaning further off than
    /// [`LOOKAHEAD_MINS`], which is a reading rather than an absence.
    In(Option<u32>),
}

impl Enforcer {
    fn new() -> Self {
        Self {
            deadline: None,
            countdown: Countdown::default(),
        }
    }

    /// Decide the action for this tick.
    ///
    /// - Entering the window schedules a shutdown.
    /// - If we're still on `slack` past the deadline, the shutdown was cancelled (e.g. the
    ///   child ran `shutdown /a`) or failed, so we re-issue — this is what makes curfew
    ///   robust rather than a one-shot latch.
    /// - Leaving the window aborts any pending shutdown.
    fn tick(
        &mut self,
        active: bool,
        upcoming: Upcoming,
        now: Instant,
        warn: Duration,
        slack: Duration,
    ) -> (Action, Option<u32>) {
        let action = self.decide(active, now, warn, slack);

        let warning = match upcoming {
            Upcoming::Nothing => {
                self.countdown.reset();
                None
            }
            Upcoming::In(mins) => self.countdown.observe_upcoming(mins),
        };

        // Two states where "bedtime in 15 minutes" is wrong whatever the caller observed, and the
        // reason this refactor was worth doing: before it, these rules had nowhere to live except
        // as ad-hoc `if`s in the loop, invisible to every test.
        //
        // * A shutdown is pending — bedtime has *arrived*, and Windows' own countdown dialog is on
        //   screen. A popup promising it is still coming is a plain contradiction.
        // * One was just aborted — the window ended this tick, so the next is a whole day off and
        //   the countdown is re-priming. Announcing from a reading taken before the abort would
        //   count down to a bedtime that already happened.
        //
        // The loop happens to pass `Nothing` in both cases today, so this changes no behaviour. It
        // moves the guarantee from "the caller is careful" to "the enforcer cannot do otherwise",
        // which is the difference between a convention and a rule.
        if self.deadline.is_some() || action == Action::Abort {
            if action == Action::Abort {
                // Re-arm, so the first reading after the window closes primes rather than warns.
                self.countdown.reset();
            }
            return (action, None);
        }

        (action, warning)
    }

    /// The shutdown half, unchanged. Split out so [`tick`](Self::tick) reads as "warn, then decide"
    /// rather than interleaving two machines.
    fn decide(&mut self, active: bool, now: Instant, warn: Duration, slack: Duration) -> Action {
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
    mut wake: crate::heartbeat::Wake,
) {
    let mut enforcer = Enforcer::new();
    let mut ticker = tokio::time::interval(CHECK_INTERVAL);
    // See the note in `rules`: without this a resume from sleep replays every missed tick.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        crate::heartbeat::tick(&mut ticker, crate::heartbeat::Enforcer::Curfew, &mut wake).await;

        let (active, warn_secs, upcoming, lang) = {
            let guard = crate::state::recover_read(&config);
            let curfew = &guard.curfew;
            // One clock reading for both questions, so "is it bedtime?" and "how soon?" can't
            // disagree across a tick boundary.
            let now = crate::clock::now();
            let active = curfew.is_active_at(now);
            // The only place the config and the clock are consulted. Everything the enforcer needs
            // to decide is now an argument, which is what lets the tests below drive the real one.
            let upcoming = if !curfew.enabled || active {
                Upcoming::Nothing
            } else {
                Upcoming::In(curfew.mins_until_active(now))
            };
            (active, curfew.warn_secs, upcoming, guard.language)
        };
        let warn = Duration::from_secs(warn_secs as u64);

        // One call, both answers. They used to come from two machines the loop had to join by
        // hand — see `Enforcer::countdown`.
        let (action, warning) =
            enforcer.tick(active, upcoming, Instant::now(), warn, CHECK_INTERVAL);

        // Advance heads-up before the window opens, so the shutdown dialog isn't the first
        // the child hears of bedtime.
        if let Some(mins) = warning
            && crate::control::notify(&control, bedtime_title(lang), &bedtime_message(mins, lang))
                .await
        {
            // Recorded only on delivery — see the same reasoning in `rules`.
            usage_log.record(
                "curfew_countdown",
                serde_json::json!({ "minutes_remaining": mins }),
            );
        }

        match action {
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

/// What the child is told as bedtime approaches. `mins` is one of
/// [`crate::countdown::WARN_AT_MINS`]; mirrors `rules::budget_countdown_message`.
fn bedtime_message(mins: u32, lang: Language) -> String {
    match (lang, mins) {
        (Language::En, 1) => "Bedtime in 1 minute!".to_string(),
        (Language::En, 5) => "Bedtime in 5 minutes — good time to save.".to_string(),
        (Language::En, m) => format!("Bedtime in {m} minutes."),
        // Dutch splits singular/plural at the same place English does (minuut/minuten), so the
        // shape of the match carries over. That is luck, not a rule — a language that pluralises
        // differently needs its own arms rather than a translated catch-all.
        (Language::Nl, 1) => "Over 1 minuut is het bedtijd!".to_string(),
        (Language::Nl, 5) => "Over 5 minuten is het bedtijd — sla je werk even op.".to_string(),
        (Language::Nl, m) => format!("Over {m} minuten is het bedtijd."),
    }
}

/// The notification title beside [`bedtime_message`].
fn bedtime_title(lang: Language) -> &'static str {
    match lang {
        Language::En => "Bedtime",
        Language::Nl => "Bedtijd",
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

    /// The window predicate stated a second way: modular arithmetic rather than a branch on
    /// `cmp`. Shared by the two tests below, which sweep it along different axes — keeping one
    /// definition, so a wrong oracle cannot agree with a wrong implementation in one test and
    /// disagree in the other.
    fn window_oracle(now: i32, start: i32, end: i32) -> bool {
        let span = (end - start).rem_euclid(1440); // 0 = empty window
        let past_start = (now - start).rem_euclid(1440);
        span != 0 && past_start < span
    }

    /// Minute-of-day to a `NaiveTime`, wrapping so a probe either side of a boundary is legal.
    fn at_minute(m: i32) -> NaiveTime {
        let m = m.rem_euclid(1440);
        NaiveTime::from_hms_opt((m / 60) as u32, (m % 60) as u32, 0).expect("valid minute")
    }

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
        // Sweeps every minute of the day against a handful of representative window shapes.
        // `is_within_matches_an_independent_definition_for_every_window_in_the_day` covers the
        // other axis — every window, probed at its boundaries — and both share `window_oracle`.
        let nt = at_minute;
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
                    window_oracle(m, s, e),
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

    /// An extension delays bedtime; it does not cancel it. Past the instant the window is active
    /// again, which is what makes "+30 tonight" mean thirty minutes rather than the rest of the
    /// night.
    #[test]
    fn an_extension_delays_bedtime_and_then_gives_it_back() {
        let c = Curfew {
            extra_until: Some(at(2026, 8, 29, 22, 30)),
            ..nightly("22:00", "07:00")
        };
        assert!(
            !c.is_active_at(at(2026, 8, 29, 22, 5)),
            "inside the extension"
        );
        assert!(
            !c.is_active_at(at(2026, 8, 29, 22, 29)),
            "up to the instant"
        );
        assert!(
            c.is_active_at(at(2026, 8, 29, 22, 30)),
            "at the instant, bedtime is back"
        );
        assert!(c.is_active_at(at(2026, 8, 29, 23, 0)), "and after it");
    }

    /// **The reason this is an absolute instant and not `date + minutes`.**
    ///
    /// Bedtime crosses midnight. A grant stored the way `DailyGrant` stores screen time — keyed on
    /// the local day — would be granted at 23:50, expire the moment the date rolled over, and slam
    /// the window shut at 00:00 with the parent having been told they had thirty minutes. An
    /// instant simply does not have that edge, and this test is what would fail if someone
    /// "simplified" it into the shape used one file over.
    #[test]
    fn an_extension_granted_before_midnight_survives_the_date_change() {
        let c = Curfew {
            extra_until: Some(at(2026, 8, 30, 0, 20)), // 23:50 + 30 min
            ..nightly("22:00", "07:00")
        };
        assert!(!c.is_active_at(at(2026, 8, 29, 23, 55)), "before midnight");
        assert!(
            !c.is_active_at(at(2026, 8, 30, 0, 10)),
            "after midnight and still inside the thirty minutes the parent granted"
        );
        assert!(c.is_active_at(at(2026, 8, 30, 0, 25)), "and it does end");
    }

    /// An extension that has run out is inert, so nothing has to reset it. That self-cleaning
    /// property is half the argument for storing an instant.
    #[test]
    fn a_spent_extension_needs_no_clearing() {
        let c = Curfew {
            extra_until: Some(at(2026, 8, 28, 23, 0)), // last night
            ..nightly("22:00", "07:00")
        };
        assert!(
            c.is_active_at(at(2026, 8, 29, 22, 30)),
            "tonight is unaffected"
        );
    }

    /// An extension must not switch curfew *on*. `enabled` is checked first, so a leftover
    /// instant on a disabled curfew cannot make the machine start shutting down.
    #[test]
    fn an_extension_cannot_resurrect_a_disabled_curfew() {
        let c = Curfew {
            enabled: false,
            extra_until: Some(at(2026, 8, 29, 22, 30)),
            ..nightly("22:00", "07:00")
        };
        assert!(!c.is_active_at(at(2026, 8, 29, 23, 0)));
    }

    /// Every reader goes through `is_active_at`, so the warning a parent gets when granting screen
    /// time has to see the extension too — otherwise it would tell them bedtime is in force while
    /// the enforcer has stood down, which is the same disagreement in the opposite direction.
    #[test]
    fn the_grant_warning_agrees_with_the_enforcer_about_an_extension() {
        let c = Curfew {
            extra_until: Some(at(2026, 8, 29, 22, 30)),
            ..nightly("22:00", "07:00")
        };
        assert_eq!(
            c.cuts_grant_short_in(at(2026, 8, 29, 22, 5), 60),
            Some(25),
            "screen time is usable until bedtime resumes at 22:30, not blocked outright"
        );
    }

    /// The reported case, reproduced. A parent approved a request on a Saturday just after 22:00
    /// and the PC shut down anyway — correctly, because the grant moved the budget and bedtime is
    /// a separate limit. What was missing was anyone saying so.
    #[test]
    fn a_grant_made_inside_the_window_is_already_dead() {
        let c = nightly("22:00", "07:00");
        assert_eq!(
            c.cuts_grant_short_in(at(2026, 8, 29, 22, 5), 30),
            Some(0),
            "granting minutes during bedtime buys nothing, and the caller must be able to say so"
        );
    }

    /// The narrower and easier-to-miss half: the window is not open yet, so every "is curfew on?"
    /// check says no, and the grant still dies partway through.
    #[test]
    fn a_grant_that_outlives_the_evening_is_cut_short() {
        let c = nightly("22:00", "07:00");
        assert!(!c.is_active_at(at(2026, 8, 29, 21, 40)), "fixture sanity");
        assert_eq!(
            c.cuts_grant_short_in(at(2026, 8, 29, 21, 40), 30),
            Some(20),
            "30 minutes granted at 21:40 runs out of evening at 22:00"
        );
        assert_eq!(
            c.cuts_grant_short_in(at(2026, 8, 29, 21, 40), 15),
            None,
            "15 minutes fits before the window, so there is nothing to warn about"
        );
    }

    /// This must not fire when curfew cannot interfere, or the warning becomes noise a parent
    /// learns to click past — which would cost more than it buys.
    #[test]
    fn nothing_is_claimed_when_curfew_is_off_or_far_away() {
        let off = Curfew {
            enabled: false,
            ..nightly("22:00", "07:00")
        };
        assert_eq!(off.cuts_grant_short_in(at(2026, 8, 29, 22, 5), 240), None);

        let c = nightly("22:00", "07:00");
        assert_eq!(
            c.cuts_grant_short_in(at(2026, 8, 29, 9, 0), 60),
            None,
            "an hour granted at nine in the morning is nowhere near bedtime"
        );
    }

    /// The probe has to cross midnight and land on the *next* weekday, the case
    /// `mins_until_active`'s own tests call out as where an off-by-one hides. A Sunday-only
    /// window is invisible from Saturday evening to anything that reasons about "today".
    #[test]
    fn the_probe_sees_a_window_that_opens_on_the_following_day() {
        let sunday_small_hours = Curfew {
            enabled: true,
            windows: vec![Window {
                start: "01:00".into(),
                end: "07:00".into(),
                days: Days {
                    sun: true,
                    ..Default::default()
                },
            }],
            ..nightly("22:00", "07:00")
        };
        // Saturday 23:30, so the window opens 90 minutes later on Sunday.
        let sat = at(2026, 8, 29, 23, 30);
        assert!(!sunday_small_hours.is_active_at(sat), "fixture sanity");
        assert_eq!(sunday_small_hours.cuts_grant_short_in(sat, 240), Some(90));
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
        enforcer: &mut Enforcer,
        curfew: &Curfew,
        from: DateTime<FixedOffset>,
        ticks: i64,
    ) -> Vec<u32> {
        let tick = TimeDelta::from_std(CHECK_INTERVAL).expect("check interval fits a TimeDelta");
        let base = Instant::now();
        (0..ticks)
            .filter_map(|i| {
                let now = from + tick * i as i32;
                // Exactly what `run_enforcer` computes, and the only place the config and the clock
                // are read — see the loop. Driving the real enforcer rather than a free function is
                // what this refactor bought: these evenings now exercise the same code path that
                // decides when the PC shuts down, including the interaction between the two.
                let active = curfew.is_active_at(now);
                let upcoming = if !curfew.enabled || active {
                    Upcoming::Nothing
                } else {
                    Upcoming::In(curfew.mins_until_active(now))
                };
                enforcer
                    .tick(
                        active,
                        upcoming,
                        base + tick.to_std().unwrap() * i as u32,
                        WARN,
                        SLACK,
                    )
                    .1
            })
            .collect()
    }

    /// [`evening`] over a fresh enforcer — the common case.
    fn one_evening(curfew: &Curfew, from: DateTime<FixedOffset>, ticks: i64) -> Vec<u32> {
        evening(&mut Enforcer::new(), curfew, from, ticks)
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
        // One enforcer spanning both configs — the parent flipping the switch mid-evening. It has
        // to be the same one, because the state that could carry a stale threshold across the
        // change now lives inside it.
        let e = &mut Enforcer::new();
        let mut announced = evening(e, &off, at(2026, 7, 9, 21, 30), 40); // off through 21:30–21:50
        announced.extend(evening(e, &on, at(2026, 7, 9, 21, 50), 30)); // on, ten minutes to go

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

    /// `is_within` against an independent definition, over **every** start/end pair of the day.
    ///
    /// # Why exhaustive rather than a property crate
    ///
    /// This is the shape random sampling is worst at and enumeration is best at: the whole risk
    /// lives on three boundaries (`start`, `end`, and midnight) out of 1,440 minutes, so a
    /// generator spends almost all its budget in the interior where nothing can go wrong. The pair
    /// space is only 2,073,600 — small enough to walk completely — and probing each pair at the
    /// boundaries and either side of them covers exactly the off-by-ones that a wrap-around
    /// predicate gets wrong. It also needs no dependency, in a crate whose manifest argues about
    /// every one it has.
    ///
    /// The reference is the standard circular half-open test — `(now - start) mod day` is inside
    /// when it is less than `(end - start) mod day` — derived from the interval definition rather
    /// than from the code under test, so agreement means something. `start == end` is the one case
    /// where the two definitions genuinely differ: the reference calls it a zero-length window and
    /// so does `is_within`, which is what "empty" must mean here — a window from 22:00 to 22:00
    /// closing the machine forever would be the worst possible reading.
    #[test]
    fn is_within_matches_an_independent_definition_for_every_window_in_the_day() {
        const DAY: i32 = 24 * 60;
        let at = at_minute;

        let mut checked: u64 = 0;
        for start in 0..DAY {
            for end in 0..DAY {
                // Every boundary and its neighbours, plus midnight and the interior midpoint —
                // the places a wrap-around comparison is wrong when it is wrong at all.
                let probes = [
                    start - 1,
                    start,
                    start + 1,
                    end - 1,
                    end,
                    end + 1,
                    0,
                    DAY - 1,
                    (start + end) / 2,
                ];
                for now in probes {
                    let want = window_oracle(now, start, end);
                    let got = is_within(at(now), at(start), at(end));
                    assert_eq!(
                        got,
                        want,
                        "is_within({:02}:{:02}, {:02}:{:02}, {:02}:{:02}) = {got}, expected {want}",
                        now.rem_euclid(DAY) / 60,
                        now.rem_euclid(DAY) % 60,
                        start / 60,
                        start % 60,
                        end / 60,
                        end % 60
                    );
                    checked += 1;
                }
            }
        }
        assert!(
            checked > 18_000_000,
            "only {checked} comparisons ran — the sweep has been narrowed and no longer proves this"
        );
    }

    /// The notification *title* beside the bedtime message.
    ///
    /// Its only caller is `run_enforcer`, which no test drives, so the first coverage run this
    /// project has had showed both arms — including the Dutch "Bedtijd" — as never executed. The
    /// message beside it was pinned from the start; the title was not, purely because it is one
    /// line long. A title is the larger text in a Windows toast.
    #[test]
    fn every_language_has_its_own_bedtime_title() {
        let titles: Vec<&str> = Language::ALL.iter().map(|&l| bedtime_title(l)).collect();
        for (lang, title) in Language::ALL.iter().zip(&titles) {
            assert!(!title.trim().is_empty(), "{lang:?} has no bedtime title");
        }
        for (i, a) in titles.iter().enumerate() {
            for b in &titles[i + 1..] {
                assert_ne!(
                    a, b,
                    "two languages share a bedtime title — one was never translated"
                );
            }
        }
    }

    #[test]
    fn bedtime_messages_read_naturally_at_every_threshold() {
        for &m in &crate::countdown::WARN_AT_MINS {
            // Both languages, because a translation that pluralises "1 minuten" is exactly the
            // trap the singular arms exist to avoid, and English passing says nothing about Dutch.
            // Derived from `Language::ALL` so a third language cannot be added past this test.
            for lang in Language::ALL {
                let msg = bedtime_message(m, lang);
                assert!(
                    msg.contains(&m.to_string()),
                    "{msg} should name the minutes"
                );
                assert!(
                    !msg.contains("1 minutes") && !msg.contains("1 minuten"),
                    "singular must not be pluralised ({lang:?}): {msg}"
                );
                assert!(!msg.is_empty(), "{lang:?} must have a string for {m}");
            }
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
            extra_until: None,
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
        assert_eq!(act(&mut e, true, base, WARN, SLACK), Action::Shutdown);
        // Subsequent ticks before the deadline do nothing (countdown in progress).
        assert_eq!(
            act(&mut e, true, base + Duration::from_secs(30), WARN, SLACK),
            Action::None
        );
        assert_eq!(
            act(&mut e, true, base + Duration::from_secs(60), WARN, SLACK),
            Action::None
        );
    }

    // The interaction the old shape could not express, let alone test. `curfew::Enforcer` owned the
    // deadline and a loose local in `run_enforcer` owned the countdown, so no single function
    // answered "what should curfew do this tick". The four cases below each need *both* answers
    // from *one* call.

    /// A pending shutdown silences the heads-up, whatever the caller observed.
    #[test]
    fn no_bedtime_warning_while_a_shutdown_is_already_pending() {
        let mut e = Enforcer::new();
        let base = Instant::now();

        // Prime the countdown just short of a threshold, so it *would* announce on the next
        // reading if nothing suppressed it.
        let (_, w) = e.tick(false, Upcoming::In(Some(16)), base, WARN, SLACK);
        assert_eq!(w, None, "the first reading primes rather than announces");

        // Bedtime arrives. A shutdown is scheduled, and a "bedtime in 15 minutes" popup would now
        // contradict the dialog Windows is already showing.
        let (action, warning) = e.tick(true, Upcoming::In(Some(15)), base, WARN, SLACK);
        assert_eq!(action, Action::Shutdown);
        assert_eq!(
            warning, None,
            "a pending shutdown must silence the countdown"
        );
    }

    /// Leaving the window aborts the shutdown and says nothing on the way out.
    #[test]
    fn an_abort_tick_is_silent_and_re_arms_the_countdown() {
        let mut e = Enforcer::new();
        let base = Instant::now();

        assert_eq!(
            e.tick(true, Upcoming::Nothing, base, WARN, SLACK).0,
            Action::Shutdown
        );

        // The window ends. The next one is a day away, so a reading taken now would count down to
        // a bedtime that has already been and gone.
        let (action, warning) = e.tick(false, Upcoming::In(Some(15)), base, WARN, SLACK);
        assert_eq!(action, Action::Abort);
        assert_eq!(warning, None, "an abort tick must not announce");

        // Re-armed: the first reading afterwards primes rather than firing on a stale comparison.
        let (_, warning) = e.tick(false, Upcoming::In(Some(15)), base, WARN, SLACK);
        assert_eq!(warning, None, "the reading after an abort primes");
    }

    /// The ordinary case still works: approaching bedtime does announce.
    #[test]
    fn a_warning_fires_when_a_threshold_is_crossed_with_nothing_pending() {
        let mut e = Enforcer::new();
        let base = Instant::now();

        let (_, w) = e.tick(false, Upcoming::In(Some(16)), base, WARN, SLACK);
        assert_eq!(w, None, "primes");

        let (action, warning) = e.tick(false, Upcoming::In(Some(15)), base, WARN, SLACK);
        assert_eq!(action, Action::None, "not bedtime yet");
        assert_eq!(warning, Some(15), "and the child gets the heads-up");
    }

    /// `Nothing` and `In(None)` are not the same, and collapsing them would misfire.
    ///
    /// `Nothing` re-primes: nothing to count down to, so the next reading announces nothing.
    /// `In(None)` is a real observation of "further off than we can see", from which the next
    /// reading *can* cross a threshold. A refactor that treated curfew-disabled as "16 minutes
    /// away" would announce bedtime to a household that had switched curfew off.
    #[test]
    fn nothing_to_count_down_to_is_not_the_same_as_a_distant_window() {
        let base = Instant::now();

        let mut disabled = Enforcer::new();
        disabled.tick(false, Upcoming::Nothing, base, WARN, SLACK);
        let (_, after_nothing) = disabled.tick(false, Upcoming::In(Some(15)), base, WARN, SLACK);
        assert_eq!(
            after_nothing, None,
            "curfew off then on primes, it does not announce"
        );

        let mut distant = Enforcer::new();
        distant.tick(false, Upcoming::In(None), base, WARN, SLACK);
        let (_, after_distant) = distant.tick(false, Upcoming::In(Some(15)), base, WARN, SLACK);
        assert_eq!(
            after_distant,
            Some(15),
            "a distant window is a reading, and this one counts"
        );
    }

    /// The shutdown half only, for the cases that predate the countdown moving into the enforcer.
    ///
    /// `Upcoming::Nothing` is the honest stand-in: every one of them is either inside a window or
    /// leaving one, and there is nothing to count down to in either. The coupling between the two
    /// halves is exercised by the tests further down, which call `tick` directly.
    fn act(
        e: &mut Enforcer,
        active: bool,
        now: Instant,
        warn: Duration,
        slack: Duration,
    ) -> Action {
        e.tick(active, Upcoming::Nothing, now, warn, slack).0
    }

    #[test]
    fn enforcer_reissues_if_still_on_past_deadline() {
        // Simulates the child running `shutdown /a`: still active well past when the machine
        // should have powered off → re-issue.
        let base = Instant::now();
        let mut e = Enforcer::new();
        assert_eq!(act(&mut e, true, base, WARN, SLACK), Action::Shutdown); // deadline = base+60
        // base+90 = deadline(60) + slack(30) → re-issue, and it must be the UNCANCELLABLE kind.
        // Re-issuing another warned countdown was the bug: it handed the child a fresh window to
        // `shutdown /a`, so a loop beat the 30s tick indefinitely.
        assert_eq!(
            act(&mut e, true, base + Duration::from_secs(90), WARN, SLACK),
            Action::ShutdownNow
        );
        // And it stays uncancellable for as long as they keep cancelling.
        for i in 1..=5 {
            let t = base + Duration::from_secs(90 + 91 * i);
            assert_eq!(
                act(&mut e, true, t, WARN, SLACK),
                Action::ShutdownNow,
                "cancel attempt {i} must not earn another countdown"
            );
        }
    }

    #[test]
    fn enforcer_aborts_when_window_ends_while_armed() {
        let base = Instant::now();
        let mut e = Enforcer::new();
        assert_eq!(act(&mut e, true, base, WARN, SLACK), Action::Shutdown);
        // Window ends (curfew disabled or time passed) → cancel the pending shutdown.
        assert_eq!(
            act(&mut e, false, base + Duration::from_secs(10), WARN, SLACK),
            Action::Abort
        );
        // Nothing pending anymore.
        assert_eq!(
            act(&mut e, false, base + Duration::from_secs(20), WARN, SLACK),
            Action::None
        );
    }

    #[test]
    fn enforcer_disarm_forces_reissue_next_active_tick() {
        let base = Instant::now();
        let mut e = Enforcer::new();
        assert_eq!(act(&mut e, true, base, WARN, SLACK), Action::Shutdown);
        e.disarm(); // simulate a failed shutdown call
        assert_eq!(
            act(&mut e, true, base + Duration::from_secs(5), WARN, SLACK),
            Action::Shutdown
        );
    }
}
