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
//!
//! # Two files, because two different people set the rate
//!
//! [`JsonlLog`] is a fixed-size ring: 2 MiB and one rotated backup, oldest evicted first. That is
//! only safe while every writer is paced by someone trustworthy. Twenty-five of the twenty-seven
//! events here are — each needs a session, the password, a live time code, or a parent resolving a
//! queue. **Two are not.** `auth_failure` and `pair_failed` are written for a caller who has
//! presented nothing at all, at whatever rate the login limiter allows, for as long as they care to
//! keep going. Sharing one ring between those two classes means the cheapest event in the system
//! evicts the most valuable one: measured before this split, four hundred wrong passwords removed a
//! recorded `lock_issued` from the parent's only view of the log.
//!
//! So the attempts get their own file ([`ATTEMPTS_FILE`]) and their own rotation budget. Flooding it
//! destroys attempt history and nothing else, and [`AuditLog::recent`] gives each log a separate
//! read budget so neither can crowd the other off the page either.
//!
//! **Partitioned, deliberately not suppressed.** The cheap way to stop a flood is to write fewer
//! lines — coalesce the failures, or log only the lockout the way `auth::pair` does. OWASP ASVS 5.0
//! **16.3.1** forbids it: *all* authentication operations are logged, successful and unsuccessful
//! alike, because failed attempts are the early indicator of credential stuffing and password
//! spraying. **16.4.2** is the other half — logs "cannot be modified" — and an attacker evicting
//! entries is modifying them. The Logging Cheat Sheet names this exact shape: *an attacker uses one
//! log entry to destroy other log entries*. Keeping every attempt and denying it the power to evict
//! satisfies both; dropping attempts trades one requirement for the other.
//!
//! This is the same move `screentime.jsonl` made when it was split out of `usage.jsonl`, for the
//! same reason: so point-in-time events cannot rotate a history out.

use std::path::PathBuf;

use serde_json::Value;

use crate::jsonl::JsonlLog;

/// The sibling of `audit.jsonl` holding events an **unauthenticated** caller can provoke.
///
/// Derived from the action log's path rather than passed in beside it: the two are one store split
/// for rate reasons, and nothing should be able to point them at different directories — a split
/// that silently landed outside the ACL-hardened data dir would be worse than no split.
pub const ATTEMPTS_FILE: &str = "auth_attempts.jsonl";

/// The events any LAN device can cause without a session, the password, or a valid code.
///
/// This list is the whole security property, so it is stated as data in one place rather than
/// decided at twenty-seven call sites. Getting it wrong in the *safe* direction (an event listed
/// here that did not need to be) costs nothing but a smaller rotation budget for that event.
/// Getting it wrong the other way is the bug this module exists to prevent, which is why
/// [`AuditLog::record`] defaults to the protected log and `tests/audit_partition.rs` fails when a
/// twenty-eighth call site appears without a decision being written down.
///
/// `auth_failure` is the fast one — the login limiter permits five a minute per source address, and
/// a home LAN lets one machine hold many addresses. `pair_failed` is already coalesced onto the
/// lockout transition by `auth::pair`, so it is five times slower and still unbounded.
///
/// `pub` so `tests/audit_partition.rs` can compare it against the classification it enforces. A
/// guard holding its own private copy of the list would agree with itself forever.
pub const ATTEMPT_EVENTS: [&str; 2] = ["auth_failure", "pair_failed"];

/// Rows of **parent-action** history `GET /api/audit` returns.
///
/// Named rather than left as a literal in the handler because it is now half of a pair, and the two
/// numbers only make sense read together — see [`ATTEMPT_VIEW`].
pub const AUDIT_VIEW: usize = 200;

/// Rows of **attempt** history `GET /api/audit` returns, on its own budget.
///
/// Smaller than [`AUDIT_VIEW`] on purpose, and the asymmetry is the point rather than thrift. A
/// flood is self-evident from a screenful; what the parent needs from this half is *that it is
/// happening*, and the durable record of every individual attempt is in the file either way. Giving
/// attempts the same budget as actions would let a guessing run fill half the page with identical
/// rows — no history lost, but the shutdown they were looking for pushed below the fold, which is
/// the same harm in a slower form.
pub const ATTEMPT_VIEW: usize = 100;

pub struct AuditLog {
    /// Events paced by a parent, a session, or a secret. The history that must survive.
    actions: JsonlLog,
    /// Events paced by whoever is knocking. Allowed to rotate away without taking anything else.
    attempts: JsonlLog,
}

impl AuditLog {
    /// An audit log writing `audit.jsonl` at `path`, and [`ATTEMPTS_FILE`] beside it.
    pub fn new(path: PathBuf) -> Self {
        let attempts = path.with_file_name(ATTEMPTS_FILE);
        Self {
            actions: JsonlLog::new(path),
            attempts: JsonlLog::new(attempts),
        }
    }

    /// A no-op audit log (tests, or any context without a data dir).
    pub fn disabled() -> Self {
        Self {
            actions: JsonlLog::disabled(),
            attempts: JsonlLog::disabled(),
        }
    }

    /// Record a security event. `fields` must never contain secrets (passwords, cookies, hashes).
    ///
    /// Which file it lands in is decided here, from [`ATTEMPT_EVENTS`], and **not** by the caller.
    /// Twenty-seven call sites each remembering to pick the right log is precisely the arrangement
    /// that rotted the last time: the invariant "every site is bounded by a human action" was true
    /// when it was written down, was checked once by hand, and was wrong by thirteen sites before
    /// anyone looked again. A table one function reads cannot drift that way.
    ///
    /// An unlisted event goes to the protected log, which is the fail-safe direction: a new event
    /// wrongly treated as trustworthy keeps history it did not need to, while the reverse would
    /// quietly make it evictable.
    pub fn record(&self, event: &str, fields: Value) {
        self.log_for(event).record(event, fields);
    }

    /// The log `event` belongs in. Split out so the routing rule is one expression that the tests
    /// can state directly, rather than something inferred from which file grew.
    fn log_for(&self, event: &str) -> &JsonlLog {
        if ATTEMPT_EVENTS.contains(&event) {
            &self.attempts
        } else {
            &self.actions
        }
    }

    /// The most recent events from both logs, newest first, each on its own budget.
    ///
    /// **`actions` and `attempts` are per-log limits, not a share of one total**, and that is the
    /// whole reason this takes two numbers. A single limit split at read time would let whichever
    /// log was noisier take the larger share, which is the behaviour being fixed.
    ///
    /// Merged by the `ts` field as text. That is exact rather than lucky: [`crate::jsonl`] stamps
    /// every line with `to_rfc3339_opts(Millis, true)`, which is fixed-width, always UTC and always
    /// `Z`-suffixed, so lexicographic order *is* chronological order. A row with no readable `ts`
    /// (a hand-edited or truncated file) sorts as oldest rather than being dropped — the audit log
    /// is the wrong place to make evidence disappear because it is malformed.
    ///
    /// The sort is stable and both inputs arrive newest-first, so events sharing a millisecond keep
    /// their within-file order instead of shuffling between reads.
    pub fn recent(&self, actions: usize, attempts: usize) -> Vec<Value> {
        let mut rows = self.actions.recent_including_rotated(actions);
        rows.extend(self.attempts.recent_including_rotated(attempts));
        rows.sort_by(|a, b| {
            let key = |v: &Value| {
                v.get("ts")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            key(b).cmp(&key(a))
        });
        rows
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
            .flat_map(|e| {
                self.actions
                    .recent_matching_including_rotated(e, MAX_VIEW_ROWS)
            })
            .collect();
        count_views(&rows, day, offset)
    }
}

/// A capture a person asked for.
///
/// Named here rather than spelled at the `record` call site, for exactly the reason
/// [`screentime::ROLLUP_EVENT`](crate::screentime::ROLLUP_EVENT) is: it is now **read back as a
/// filter** as well as written. Renaming it at the write site while this string stayed put would
/// leave `views_on` matching nothing, so the child's "how often were you looked at" would read
/// zero forever — a transparency feature failing silently, which is the one way it must not fail.
/// The compiler cannot see a mismatch between two string literals; it can see a missing constant.
pub const SCREENSHOT_EVENT: &str = "screenshot_taken";

/// One coalesced window of timer-driven live-view frames. Same naming rule as
/// [`SCREENSHOT_EVENT`].
pub const LIVE_VIEW_EVENT: &str = "live_view";

/// The events that mean *a parent looked at this screen*, as opposed to acted on the machine.
///
/// `process_kill`, `lock_issued` and `shutdown_issued` are deliberately absent. The child sees
/// those happen — an app closes, the screen locks — so counting them here would inflate a number
/// whose whole claim is "this is how often you were watched", by adding events that were never
/// watching.
const VIEW_EVENTS: [&str; 2] = [SCREENSHOT_EVENT, LIVE_VIEW_EVENT];

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
/// This is the noisiest member of a class, not the only member of one — and the sentence that used
/// to stand here claimed otherwise. It read: *there are fourteen `audit.record` call sites and the
/// other thirteen are each bounded by a discrete human action … the live stream is the only audit
/// event a clock can produce, so it is the only one whose volume is bounded by nothing at all.*
///
/// Both halves aged badly. There are **twenty-seven** call sites, and `auth_failure` was never
/// bounded by a human action: an unauthenticated caller writes one per wrong password, five a
/// minute per source address, for as long as it likes. Measured before the partition, four hundred
/// of them removed a recorded `lock_issued` from the parent's only view of the log. The claim was
/// true when it was written, was checked once by hand, and was wrong by thirteen sites and one
/// whole threat before anyone re-read it — and it had been copied into `docs/SECURITY.md`, where
/// more people read it than read this.
///
/// **So the count is no longer asserted in prose.** `tests/audit_partition.rs` enumerates the call
/// sites out of the source and fails when one appears that nobody has classified. A sentence cannot
/// notice a twenty-eighth site; that test can.
///
/// Coalescing is still the right answer *here* and would be the wrong one for `auth_failure`. Frames
/// are a clock repeating itself, so counting them loses nothing a reader wanted. Failed logins are
/// evidence, and ASVS 5.0 **16.3.1** requires each one to be logged — so that event is partitioned
/// into its own file instead of being thinned. See the module docs.
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

    /// The audit log's one job is to survive the incident it is recording. These pin that it
    /// cannot be emptied by the only caller who needs no credential to write to it.
    ///
    /// The numbers are the shipped ones: `GET /api/audit` shows [`AUDIT_VIEW`] rows, and an
    /// unauthenticated caller can produce `auth_failure` at the login limiter's five a minute per
    /// source address, for as long as it likes.
    mod flooding {
        use super::super::{ATTEMPT_VIEW, AUDIT_VIEW, AuditLog};
        use serde_json::json;

        fn attempt(log: &AuditLog) {
            log.record(
                "auth_failure",
                json!({ "src_ip": "192.168.1.66", "reason": "bad_password", "locked_out": false }),
            );
        }

        /// Wrong passwords must not push a real action out of the parent's only view of the log.
        #[test]
        fn a_flood_of_attempts_cannot_evict_a_parent_action_from_the_view() {
            let dir = crate::testutil::ScratchDir::new("audit-evict");
            let log = AuditLog::new(dir.join("audit.jsonl"));

            log.record("lock_issued", json!({}));
            for _ in 0..400 {
                attempt(&log);
            }

            let view = log.recent(AUDIT_VIEW, ATTEMPT_VIEW);
            assert!(
                view.iter().any(|v| v["event"] == "lock_issued"),
                "400 wrong passwords erased a lock from the parent's view of the audit log"
            );
        }

        /// The flood must not reach the file the actions live in at all — the view surviving is
        /// not enough, because rotation destroys history that no view can bring back.
        #[test]
        fn a_flood_of_attempts_does_not_grow_the_action_log() {
            let dir = crate::testutil::ScratchDir::new("audit-bytes");
            let path = dir.join("audit.jsonl");
            let log = AuditLog::new(path.clone());

            log.record("lock_issued", json!({}));
            let before = std::fs::metadata(&path).unwrap().len();
            for _ in 0..400 {
                attempt(&log);
            }
            let after = std::fs::metadata(&path).unwrap().len();

            assert_eq!(
                before, after,
                "attempts are being appended to the action log, so enough of them rotate it away"
            );
        }

        /// ASVS 5.0 16.3.1 requires every unsuccessful authentication attempt to be logged, so the
        /// fix for the two above must not be to write fewer of them. Pinned in the same module as
        /// the eviction tests precisely because the cheap way to pass those is to violate this.
        #[test]
        fn every_attempt_is_still_recorded_and_still_reachable() {
            let dir = crate::testutil::ScratchDir::new("audit-keep");
            let log = AuditLog::new(dir.join("audit.jsonl"));

            for _ in 0..30 {
                attempt(&log);
            }

            let seen = log
                .recent(usize::MAX, usize::MAX)
                .iter()
                .filter(|v| v["event"] == "auth_failure")
                .count();
            assert_eq!(
                seen, 30,
                "an attempt was suppressed rather than partitioned"
            );
        }

        /// The attempts land in the ACL-hardened data dir, beside the action log.
        ///
        /// Pinned because the split is derived rather than passed in: `install` hardens the data
        /// *directory* and relies on `(OI)(CI)` inheritance to cover files created later, so a file
        /// that landed one level up would be world-readable and nothing would say so. A path bug
        /// here is silent in exactly the way an ACL bug is.
        #[test]
        fn the_attempt_log_is_a_sibling_of_the_action_log() {
            let dir = crate::testutil::ScratchDir::new("audit-sibling");
            let path = dir.join("audit.jsonl");
            let log = AuditLog::new(path.clone());

            attempt(&log);

            let expected = path.with_file_name(super::super::ATTEMPTS_FILE);
            assert!(
                expected.exists(),
                "the attempt log is not at {}",
                expected.display()
            );
            assert_eq!(
                expected.parent(),
                path.parent(),
                "the attempt log left the hardened data dir"
            );
        }

        /// The view spans both logs, and a reader cannot tell they are two files.
        #[test]
        fn the_view_interleaves_both_logs_newest_first() {
            let dir = crate::testutil::ScratchDir::new("audit-order");
            let log = AuditLog::new(dir.join("audit.jsonl"));

            // Spaced, because the merge key has millisecond resolution and three records in a
            // tight loop land in the same one — the assertion below would then be pinning the
            // concatenation order of the two reads rather than the sort.
            let tick = || std::thread::sleep(std::time::Duration::from_millis(3));
            log.record("lock_issued", json!({}));
            tick();
            attempt(&log);
            tick();
            log.record("shutdown_issued", json!({}));

            let view = log.recent(AUDIT_VIEW, ATTEMPT_VIEW);
            let order: Vec<&str> = view.iter().filter_map(|v| v["event"].as_str()).collect();
            assert_eq!(
                order,
                ["shutdown_issued", "auth_failure", "lock_issued"],
                "the merged view must be newest-first across both logs"
            );
        }
    }

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
