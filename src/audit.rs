//! Append-only **security** audit log: who logged in, from where, and the sensitive actions
//! taken (screenshot, kill, shutdown, lock, curfew/password changes).
//!
//! A SYSTEM service has no console, so `tracing`'s stdout output is invisible on the deployed
//! machine — a stranger could log in and leave no trace the parent can inspect. This records the
//! security-relevant events as one JSON object per line in the ACL-hardened data dir, exposed
//! read-only to the authenticated parent via `GET /api/audit`. It is kept in its own file
//! (`audit.jsonl`), separate from the usage-history log, so the security trail stays clean.
//!
//! The store mechanics live in [`crate::jsonl`]; this is a distinct newtype so the compiler keeps
//! security events and usage events from being crossed.

use std::path::PathBuf;

use serde_json::Value;

use crate::jsonl::JsonlLog;

pub struct AuditLog(JsonlLog);

impl AuditLog {
    /// An audit log writing `audit.jsonl` at `path`.
    pub fn new(path: PathBuf) -> Self {
        Self(JsonlLog::new(path))
    }

    /// A no-op audit log (tests, or any context without a data dir).
    pub fn disabled() -> Self {
        Self(JsonlLog::disabled())
    }

    /// Record a security event. `fields` must never contain secrets (passwords, cookies, hashes).
    pub fn record(&self, event: &str, fields: Value) {
        self.0.record(event, fields);
    }

    /// The most recent `limit` events, newest first.
    pub fn recent(&self, limit: usize) -> Vec<Value> {
        self.0.recent(limit)
    }

    /// How many times the child's screen was viewed on `day`, in the trusted local zone.
    ///
    /// Read through the event-filtered reader rather than [`recent`](Self::recent), which parses
    /// every line in the file however small its `limit`. This is called from `GET /status` — the
    /// child's page, unauthenticated, polling once a minute — so it scans for two substrings and
    /// only parses the lines that could match. Rotation-inclusive, because undercounting how often
    /// a parent looked is the one direction this number must not fail in.
    pub fn views_on(&self, day: chrono::NaiveDate, offset: chrono::FixedOffset) -> u32 {
        let rows: Vec<Value> = VIEW_EVENTS
            .iter()
            .flat_map(|e| self.0.recent_matching_including_rotated(e, MAX_VIEW_ROWS))
            .collect();
        count_views(&rows, day, offset)
    }
}

/// The events that mean *a parent looked at this screen*, as opposed to acted on the machine.
///
/// `process_kill`, `lock_issued` and `shutdown_issued` are deliberately absent. The child sees
/// those happen — an app closes, the screen locks — so counting them here would inflate a number
/// whose whole claim is "this is how often you were watched", by adding events that were never
/// watching.
const VIEW_EVENTS: [&str; 2] = ["screenshot_taken", "live_view"];

/// Upper bound on view lines held while counting. Live frames are already coalesced into one
/// `live_view` line per window, and captures are human-driven, so a real day is orders below this;
/// it exists so a corrupted or hostile log cannot make the child's page allocate without limit.
const MAX_VIEW_ROWS: usize = 20_000;

/// The pure half of [`AuditLog::views_on`]: how many view events in `rows` fall on `day`.
///
/// Takes the trusted offset rather than reading a clock, for the same reason `clock::decide` and
/// `screentime::build_report` take theirs — so the day-boundary arithmetic is testable without a
/// machine set to the right zone.
///
/// **The conversion is the whole function.** `jsonl::record` stamps every line in UTC, while the
/// day this is counting is the trusted *local* day. Comparing the two as text — matching the
/// `YYYY-MM-DD` prefix of the timestamp against the local date — is the obvious implementation and
/// is wrong everywhere except UTC: in Amsterdam at 00:30 local it is still yesterday in UTC, so
/// every view in the first hour or two of the evening's tail would be counted on the wrong day.
///
/// Re-checks the event tag rather than trusting the caller to have filtered, so the function is
/// correct standalone and can be tested against a mixed log.
pub fn count_views(rows: &[Value], day: chrono::NaiveDate, offset: chrono::FixedOffset) -> u32 {
    let n = rows
        .iter()
        .filter(|v| {
            v.get("event")
                .and_then(Value::as_str)
                .is_some_and(|e| VIEW_EVENTS.contains(&e))
        })
        .filter(|v| {
            v.get("ts")
                .and_then(Value::as_str)
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .is_some_and(|t| t.with_timezone(&offset).date_naive() == day)
        })
        .count();
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// How long a live-view session runs before it writes a second audit line.
///
/// `pub(crate)`, not `pub`: this is [`LiveViewAudit`]'s default and nothing outside the crate needs
/// to name it. It was public for exactly one reason — so `api::screenshot` could hand it back, on
/// every call, to the type that defines it. That put the production window in the caller's gift,
/// which is the wrong place for it: a second call site could quietly coalesce over five seconds.
pub(crate) const LIVE_AUDIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(300);

/// Collapses the live view's timer-driven stream of captures into at most one audit line per
/// `LIVE_AUDIT_WINDOW`.
///
/// # Why this exists
///
/// `screenshot_taken` used to be written once per frame, live frames included. At the fixed 3s
/// cadence the live view shipped with, that is 1,200 lines an hour, 61 bytes each. `jsonl.rs` caps `audit.jsonl` at 2 MiB and
/// keeps exactly one rotated backup, so **about 57 hours of live viewing evicts the entire security
/// history** — every login, every kill, every password change, gone to make room for a timer.
///
/// This is not simply the noisiest of a known class. There are fourteen `audit.record` call sites
/// and the other thirteen are each bounded by a discrete human action, which is precisely why
/// `OPEN-FINDINGS.md` investigated the `/time-request` case and correctly refuted it. The live
/// stream is the only audit event a *clock* can produce, so it is the only one whose volume is
/// bounded by nothing at all.
///
/// # Why coalescing rather than a start/stop pair
///
/// A pair would need the browser to announce both ends, and the browser is exactly what disappears
/// when a laptop lid closes. Counting frames and reporting periodically needs no cooperation, no
/// extra route, and degrades correctly when the tab dies: the line already written stands, and no
/// further ones appear.
///
/// Full-tier captures are **not** routed through here. Those are few, deliberate, and the sensitive
/// ones — a parent reading something on the screen rather than glancing at it — so they keep a line
/// each. That distinction is the point: the log now says "five detailed looks, plus ambient view for
/// forty minutes" instead of 1,200 identical rows.
pub struct LiveViewAudit {
    /// Held rather than passed in, so the window is a property of the coalescer and not a decision
    /// each caller re-makes. `timereq::SubmitLimiter` is the same shape for the same reason: the
    /// window is a field, and only `now` is injected.
    window: std::time::Duration,
    state: std::sync::Mutex<LiveState>,
}

impl Default for LiveViewAudit {
    fn default() -> Self {
        Self::new(LIVE_AUDIT_WINDOW)
    }
}

#[derive(Default)]
struct LiveState {
    /// Frames seen since the last line was written, this one included.
    frames: u64,
    /// When the last line was written. `None` before the first ever frame.
    last_emit: Option<std::time::Instant>,
}

impl LiveViewAudit {
    /// Coalescing over `window`. `Default` is the shipped `LIVE_AUDIT_WINDOW`; tests name their own.
    pub fn new(window: std::time::Duration) -> Self {
        Self {
            window,
            state: std::sync::Mutex::new(LiveState::default()),
        }
    }

    /// Count one timer-driven frame. Returns `Some(n)` when a line is due, `n` being the frames
    /// since the previous line.
    ///
    /// **`n` counts frames DELIVERED, not frames captured**, and the two genuinely differ. A click
    /// that supersedes a timer frame drops the handler future at the `await` in `api::blocking` —
    /// before `screenshot` reaches this method — while `spawn_blocking` runs to completion
    /// regardless, so the child's machine captured a frame that is never counted here. Delivered is
    /// the right measure for the question this log answers (*was the screen watched, and for how
    /// long*), but the name does not say which it is, so this does.
    ///
    /// The first frame after a quiet spell always reports, so the log records that watching
    /// *started* promptly rather than five minutes late — and a parent who opens the live view for
    /// ten seconds still leaves a trace.
    pub fn observe(&self, now: std::time::Instant) -> Option<u64> {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        st.frames += 1;
        let due = match st.last_emit {
            None => true,
            Some(last) => now.duration_since(last) >= self.window,
        };
        if !due {
            return None;
        }
        st.last_emit = Some(now);
        Some(std::mem::take(&mut st.frames))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    mod views {
        use super::super::count_views;
        use chrono::{FixedOffset, NaiveDate};
        use serde_json::json;

        fn day() -> NaiveDate {
            NaiveDate::from_ymd_opt(2026, 8, 29).unwrap()
        }
        /// Amsterdam in summer. Chosen because it is where the difference shows.
        fn cest() -> FixedOffset {
            FixedOffset::east_opt(2 * 3600).unwrap()
        }
        fn row(event: &str, ts: &str) -> serde_json::Value {
            json!({ "ts": ts, "event": event })
        }

        /// The reason this function takes an offset instead of comparing text.
        ///
        /// `jsonl::record` stamps UTC; the day being counted is the trusted *local* day. At 00:30
        /// on the 30th in Amsterdam it is still 22:30 on the 29th in UTC, and at 23:30 on the 29th
        /// local it is 21:30 UTC on the 29th. A prefix match on the timestamp puts the first of
        /// those on the 29th and is wrong; both belong to the local day they happened in.
        /// The fixture is deliberately lopsided, and the first version of it was not — which is
        /// the more useful half of this comment. It began with one row on each side of both
        /// boundaries plus one unambiguous row, and counted 2 whether the offset was applied or
        /// dropped: the row wrongly included and the row wrongly excluded cancelled exactly. It
        /// passed, and it proved nothing. Only mutating the conversion away and watching the test
        /// stay green showed it up.
        ///
        /// So: two views that belong to the local day and fall on the UTC day *before* it, and one
        /// that falls on the UTC day but belongs to tomorrow locally. Correct is 2, counting by
        /// UTC is 1, and no arrangement of the two errors reaches the same number.
        #[test]
        fn a_view_is_counted_on_the_local_day_not_the_utc_one() {
            let rows = vec![
                // 01:00 local on the 29th -> 23:00 UTC on the 28th. Today.
                row("live_view", "2026-08-28T23:00:00.000Z"),
                // 00:30 local on the 29th -> 22:30 UTC on the 28th. Today.
                row("screenshot_taken", "2026-08-28T22:30:00.000Z"),
                // 00:30 local on the 30th -> 22:30 UTC on the 29th. Tomorrow, not today.
                row("screenshot_taken", "2026-08-29T22:30:00.000Z"),
            ];
            assert_eq!(count_views(&rows, day(), cest()), 2);
        }

        /// Acting on the machine is not looking at it. The child watches an app close or the
        /// screen lock; counting those would inflate a number whose claim is about being watched.
        #[test]
        fn actions_are_not_views() {
            let rows = vec![
                row("screenshot_taken", "2026-08-29T10:00:00.000Z"),
                row("process_kill", "2026-08-29T10:01:00.000Z"),
                row("lock_issued", "2026-08-29T10:02:00.000Z"),
                row("shutdown_issued", "2026-08-29T10:03:00.000Z"),
                row("auth_success", "2026-08-29T10:04:00.000Z"),
            ];
            assert_eq!(count_views(&rows, day(), cest()), 1);
        }

        /// A line with no usable timestamp is skipped rather than counted on an arbitrary day.
        #[test]
        fn an_unparseable_row_is_skipped_not_guessed() {
            let rows = vec![
                row("screenshot_taken", "not-a-timestamp"),
                json!({ "event": "screenshot_taken" }),
                row("screenshot_taken", "2026-08-29T10:00:00.000Z"),
            ];
            assert_eq!(count_views(&rows, day(), cest()), 1);
        }
    }

    // The window under test IS the shipped window. Spelling `300` again here would have let
    // `LIVE_AUDIT_WINDOW` be changed to sixty seconds with all four tests still passing, still
    // proving things about five minutes.
    const W: Duration = LIVE_AUDIT_WINDOW;

    #[test]
    fn the_first_frame_always_reports() {
        let a = LiveViewAudit::default();
        assert_eq!(
            a.observe(Instant::now()),
            Some(1),
            "opening the live view must leave a trace immediately, not after the first window"
        );
    }

    #[test]
    fn frames_inside_one_window_collapse_into_the_next_line() {
        let a = LiveViewAudit::default();
        let t0 = Instant::now();
        assert_eq!(a.observe(t0), Some(1));
        // A window's worth of live frames, three seconds apart. The interval is arbitrary — it
        // only has to fit ninety-nine more frames inside the window without reaching its end. The
        // cadences a parent can actually choose are 2s, 5s and 15s; 3s was the old fixed default
        // and no longer exists.
        for i in 1..100 {
            assert_eq!(
                a.observe(t0 + Duration::from_secs(i * 3)),
                None,
                "no line may be written mid-window"
            );
        }
        assert_eq!(
            a.observe(t0 + W),
            Some(100),
            "the line that closes the window must carry every frame it stood for"
        );
    }

    /// The count is what makes one line worth as much as the thousand it replaced. Losing it — by
    /// resetting on a skipped emit, say — would leave a log that records *that* the screen was
    /// watched but never *how much*.
    #[test]
    fn the_count_resets_only_when_a_line_is_actually_written() {
        let a = LiveViewAudit::default();
        let t0 = Instant::now();
        a.observe(t0);
        a.observe(t0 + Duration::from_secs(1));
        a.observe(t0 + Duration::from_secs(2));
        assert_eq!(a.observe(t0 + W), Some(3));
        a.observe(t0 + W + Duration::from_secs(1));
        assert_eq!(
            a.observe(t0 + W + W),
            Some(2),
            "the second window counts only its own frames"
        );
    }

    /// A parent who watches, stops for an hour, then watches again must produce two visible
    /// episodes rather than one line an hour late.
    #[test]
    fn a_gap_longer_than_the_window_reports_again_at_once() {
        let a = LiveViewAudit::default();
        let t0 = Instant::now();
        assert_eq!(a.observe(t0), Some(1));
        assert_eq!(
            a.observe(t0 + Duration::from_secs(3600)),
            Some(1),
            "a fresh viewing session must report on its first frame"
        );
    }
}
