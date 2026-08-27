//! "Request more time" queue: the child asks for extra minutes from a small unauthenticated
//! page on the LAN; the parent approves/denies in the dashboard.
//!
//! Storage is **event-sourced JSON-Lines** on top of [`crate::jsonl::JsonlLog`]: every state
//! change appends a line (`requested` / `approved` / `denied`), and [`TimeRequests::pending`]
//! folds by `id` to the latest status. This reuses the proven append-only store and, unlike an
//! in-memory queue, survives the auto-restarting service so a pending request isn't lost on a
//! reboot.

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::json;

use crate::error::AppError;
use crate::jsonl::JsonlLog;

/// Largest single request we accept (a child can't ask for an absurd grant).
pub const MAX_REQUEST_MINUTES: u32 = 240;
/// Cap on outstanding requests, so a spammy child can't flood the parent's queue.
const MAX_PENDING: usize = 5;
/// Reason text is truncated to this many characters.
const MAX_REASON_CHARS: usize = 200;

/// A pending request as surfaced to the parent UI (never leaks internal event history).
#[derive(Debug, Serialize)]
pub struct PendingRequest {
    pub id: String,
    pub ts: String,
    pub minutes: u32,
    pub reason: String,
}

/// The state of the child's newest request, as shown on their own page.
///
/// Deliberately thinner than [`PendingRequest`]: no id (the child has nothing to do with one) and
/// no timestamp or reason (they wrote it, and it is their own page). Just the two facts that answer
/// "what happened to what I asked for".
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LatestRequest {
    /// `pending` · `approved` · `denied`.
    pub state: String,
    /// How many minutes were asked for.
    pub minutes: u32,
}

/// The persisted request queue.
pub struct TimeRequests {
    log: JsonlLog,
    /// Monotonic component of the id, so ids are unique within a run.
    counter: AtomicU64,
    /// Serializes each read → decide → append (`submit`'s cap check, `resolve`'s consume), the
    /// same gate [`crate::timecode::TimeCodes`] uses for the same reason.
    ///
    /// Without it both are check-then-append races. Measured: six concurrent approvals of one
    /// request all returned `Some` — so a parent double-tapping Approve on a phone granted the
    /// minutes twice — and twenty concurrent submits all landed against a cap of five.
    gate: Mutex<()>,
}

impl TimeRequests {
    pub fn new(path: PathBuf) -> Self {
        Self {
            log: JsonlLog::new(path),
            counter: AtomicU64::new(0),
            gate: Mutex::new(()),
        }
    }

    /// A no-op queue (tests): submit returns an id but nothing persists, so `pending` is empty.
    pub fn disabled() -> Self {
        Self {
            log: JsonlLog::disabled(),
            counter: AtomicU64::new(0),
            gate: Mutex::new(()),
        }
    }

    /// Lock the gate, recovering from poison rather than panicking (the critical sections can't
    /// panic, and the release build aborts on panic anyway).
    fn gate(&self) -> std::sync::MutexGuard<'_, ()> {
        self.gate.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Append a new request. Returns its id, or `None` if the pending cap is reached (so the
    /// caller can respond identically either way and never leak queue state to the child).
    pub fn submit(&self, minutes: u32, reason: &str) -> Option<String> {
        // Held across the cap check and the append, so concurrent submits can't all observe the
        // queue as under the cap and all land.
        let _gate = self.gate();
        if self.pending().len() >= MAX_PENDING {
            return None;
        }
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        let id = format!("{:x}-{:x}", chrono::Utc::now().timestamp_millis(), n);
        let reason: String = reason.trim().chars().take(MAX_REASON_CHARS).collect();
        self.log.record(
            "requested",
            json!({ "id": id, "minutes": minutes, "reason": reason }),
        );
        Some(id)
    }

    /// The still-pending requests, newest first. Folds the event log by `id`: since events come
    /// back newest-first, the first one seen for an id is its latest status.
    pub fn pending(&self) -> Vec<PendingRequest> {
        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        for e in self.log.recent(usize::MAX) {
            let Some(id) = e.get("id").and_then(|v| v.as_str()) else {
                continue;
            };
            if !seen.insert(id.to_string()) {
                continue; // already have this id's latest status
            }
            if e.get("event").and_then(|v| v.as_str()) == Some("requested") {
                out.push(PendingRequest {
                    id: id.to_string(),
                    ts: e
                        .get("ts")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    minutes: e.get("minutes").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                    reason: e
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                });
            }
        }
        out
    }

    /// What became of the most recently submitted request — for the child's own page.
    ///
    /// [`Self::pending`] answers the parent's question ("what is waiting for me?") and deliberately
    /// drops anything already decided. That left the child with no answer at all: they pressed
    /// *Send request*, read "waiting for a parent to reply", and the page never said another word. A
    /// denial was indistinguishable from silence, and an approval showed up only as a number that
    /// changed by itself up to a minute later. The likely outcome is the child walking off to ask
    /// out loud — which is the exact interaction `/ask` exists to replace.
    ///
    /// Returns the *latest status* of the *newest submission*, which are two different lookups over
    /// one newest-first pass: the first `requested` event encountered is the newest submission
    /// (nothing submitted later can appear further down a newest-first list), and the first event
    /// carrying that id is its current status.
    pub fn latest(&self) -> Option<LatestRequest> {
        let events = self.log.recent(usize::MAX);
        let newest_submission = events.iter().find(|e| {
            e.get("event").and_then(|v| v.as_str()) == Some("requested")
                && e.get("id").and_then(|v| v.as_str()).is_some()
        })?;
        let id = newest_submission.get("id")?.as_str()?;
        let minutes = newest_submission
            .get("minutes")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;

        let status = events
            .iter()
            .find(|e| e.get("id").and_then(|v| v.as_str()) == Some(id))?;
        let state = match status.get("event").and_then(|v| v.as_str()) {
            Some("approved") => "approved",
            Some("denied") => "denied",
            // Anything else is still waiting. Unknown event names resolve to "pending" rather than
            // to an outcome, because telling a child their request was refused when it was not is
            // the one wrong answer here.
            _ => "pending",
        };
        Some(LatestRequest {
            state: state.to_string(),
            minutes,
        })
    }

    /// Approve (`true`) or deny (`false`) a pending request. Returns the resolved request (so the
    /// caller knows how many minutes to grant), or `None` if the id isn't currently pending.
    pub fn resolve(&self, id: &str, approve: bool) -> Option<PendingRequest> {
        // Held across find → append so one request resolves exactly once. Otherwise every
        // concurrent approval sees it as pending and the caller grants the minutes each time.
        let _gate = self.gate();
        let req = self.pending().into_iter().find(|r| r.id == id)?;
        self.log.record(
            if approve { "approved" } else { "denied" },
            json!({ "id": id }),
        );
        Some(req)
    }
}

/// A per-IP submission throttle that counts **every** call (unlike [`crate::auth::LoginLimiter`],
/// which only counts failures) — so a child can't spam the request endpoint.
pub struct SubmitLimiter {
    inner: Mutex<std::collections::HashMap<IpAddr, Vec<Instant>>>,
    max_per_window: usize,
    window: Duration,
}

impl SubmitLimiter {
    pub fn new(max_per_window: usize, window: Duration) -> Self {
        Self {
            inner: Mutex::new(std::collections::HashMap::new()),
            max_per_window,
            window,
        }
    }

    /// Record a call from `ip` and return `Err(TooManyAttempts)` if it exceeds the window quota.
    /// `now` is injected (callers pass `Instant::now()`) so the window logic is unit-testable.
    pub fn count_and_check(&self, ip: IpAddr, now: Instant) -> Result<(), AppError> {
        let mut map = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        // Drop timestamps outside the window, everywhere, so the map stays bounded.
        map.retain(|_, times| {
            times.retain(|t| now.duration_since(*t) < self.window);
            !times.is_empty()
        });
        let times = map.entry(ip).or_default();
        if times.len() >= self.max_per_window {
            return Err(AppError::TooManyAttempts);
        }
        times.push(now);
        Ok(())
    }
}

impl Default for SubmitLimiter {
    fn default() -> Self {
        // 5 requests per minute per IP.
        Self::new(5, Duration::from_secs(60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> (TimeRequests, crate::testutil::ScratchDir) {
        let dir = crate::testutil::ScratchDir::new(&format!("timereq-{tag}"));
        (TimeRequests::new(dir.join("time_requests.jsonl")), dir)
    }

    /// Regression: a parent double-tapping Approve must grant the minutes once.
    ///
    /// Before the gate this failed every round — all six concurrent approvals returned `Some`,
    /// and `api::approve_time_request` adds the minutes for each. Repeated rounds because the
    /// race is timing-dependent.
    #[test]
    fn concurrent_approvals_resolve_a_request_only_once() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (q, _dir) = scratch("approve");
        let q = Arc::new(q);
        for round in 0..30 {
            let id = q.submit(30, "homework").unwrap();
            let wins = Arc::new(AtomicUsize::new(0));
            let start = Arc::new(std::sync::Barrier::new(6));
            std::thread::scope(|s| {
                for _ in 0..6 {
                    let (q, id) = (q.clone(), id.clone());
                    let (w, b) = (wins.clone(), start.clone());
                    s.spawn(move || {
                        b.wait(); // maximize overlap
                        if q.resolve(&id, true).is_some() {
                            w.fetch_add(1, Ordering::SeqCst);
                        }
                    });
                }
            });
            assert_eq!(
                wins.load(Ordering::SeqCst),
                1,
                "round {round}: one request granted minutes more than once"
            );
        }
    }

    /// Regression: the pending cap must hold under concurrent submits (it admitted 20 of 20).
    #[test]
    fn concurrent_submits_respect_the_pending_cap() {
        use std::sync::Arc;

        let (q, _dir) = scratch("cap");
        let q = Arc::new(q);
        let start = Arc::new(std::sync::Barrier::new(20));
        std::thread::scope(|s| {
            for _ in 0..20 {
                let (q, b) = (q.clone(), start.clone());
                s.spawn(move || {
                    b.wait();
                    let _ = q.submit(10, "x");
                });
            }
        });
        assert_eq!(q.pending().len(), MAX_PENDING, "pending cap bypassed");
    }

    #[test]
    fn submit_pending_resolve_roundtrip() {
        let dir = crate::testutil::ScratchDir::new("timereq-roundtrip");
        let q = TimeRequests::new(dir.join("time_requests.jsonl"));

        let id = q.submit(30, "homework done").unwrap();
        let pending = q.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].minutes, 30);
        assert_eq!(pending[0].id, id);

        let resolved = q.resolve(&id, true).unwrap();
        assert_eq!(resolved.minutes, 30);
        assert!(q.pending().is_empty(), "approved request no longer pending");

        assert!(q.resolve(&id, true).is_none(), "already resolved");
    }

    /// The child's page answers "what happened to what I asked for", so every state it can show
    /// is pinned — including the one that used to be unreachable from that page at all.
    #[test]
    fn latest_reports_each_outcome_to_the_child() {
        let (q, _dir) = scratch("latest-outcomes");
        assert_eq!(q.latest(), None, "nothing asked for yet");

        let id = q.submit(20, "homework done").expect("submit");
        assert_eq!(
            q.latest(),
            Some(LatestRequest {
                state: "pending".into(),
                minutes: 20
            })
        );

        q.resolve(&id, false).expect("deny");
        assert_eq!(
            q.latest(),
            Some(LatestRequest {
                state: "denied".into(),
                minutes: 20
            }),
            "a denial must be visible; before this it reached the child through no channel at all"
        );

        let id2 = q.submit(15, "").expect("submit again");
        assert_eq!(
            q.latest(),
            Some(LatestRequest {
                state: "pending".into(),
                minutes: 15
            }),
            "a new request supersedes the old outcome"
        );

        q.resolve(&id2, true).expect("approve");
        assert_eq!(
            q.latest(),
            Some(LatestRequest {
                state: "approved".into(),
                minutes: 15
            })
        );
    }

    /// `latest` reports the newest *submission*, not the newest *event*. Deciding an older request
    /// after a newer one arrived must not make the page describe the older one.
    #[test]
    fn latest_follows_the_newest_submission_not_the_newest_decision() {
        let (q, _dir) = scratch("latest-ordering");
        let older = q.submit(10, "first").expect("submit");
        let _newer = q.submit(45, "second").expect("submit");

        q.resolve(&older, true).expect("approve the older one");

        assert_eq!(
            q.latest(),
            Some(LatestRequest {
                state: "pending".into(),
                minutes: 45
            }),
            "the child is still waiting on their newest request, whatever happened to the older"
        );
    }

    #[test]
    fn pending_cap_is_enforced() {
        let dir = crate::testutil::ScratchDir::new("timereq-cap-enforced");
        let q = TimeRequests::new(dir.join("time_requests.jsonl"));
        for _ in 0..MAX_PENDING {
            assert!(q.submit(10, "").is_some());
        }
        assert!(q.submit(10, "").is_none(), "over the pending cap");
    }

    #[test]
    fn disabled_submit_is_noop() {
        let q = TimeRequests::disabled();
        assert!(q.submit(10, "x").is_some(), "returns an id");
        assert!(q.pending().is_empty(), "but nothing persisted");
    }

    #[test]
    fn submit_limiter_trips_after_quota() {
        let ip: IpAddr = "192.168.1.5".parse().unwrap();
        let now = Instant::now();
        let lim = SubmitLimiter::new(2, Duration::from_secs(60));
        assert!(lim.count_and_check(ip, now).is_ok());
        assert!(lim.count_and_check(ip, now).is_ok());
        assert!(
            lim.count_and_check(ip, now).is_err(),
            "3rd exceeds the quota of 2"
        );
        // A different IP is unaffected.
        let other: IpAddr = "192.168.1.6".parse().unwrap();
        assert!(lim.count_and_check(other, now).is_ok());
    }

    #[test]
    fn submit_limiter_window_expires() {
        let ip: IpAddr = "192.168.1.5".parse().unwrap();
        let base = Instant::now();
        let window = Duration::from_secs(60);
        let lim = SubmitLimiter::new(2, window);
        // Fill the quota at t=0.
        assert!(lim.count_and_check(ip, base).is_ok());
        assert!(lim.count_and_check(ip, base).is_ok());
        assert!(lim.count_and_check(ip, base).is_err());
        // Still inside the window a moment later → still blocked.
        assert!(
            lim.count_and_check(ip, base + Duration::from_secs(59))
                .is_err()
        );
        // Past the window the old timestamps age out → allowed again.
        assert!(
            lim.count_and_check(ip, base + window + Duration::from_secs(1))
                .is_ok()
        );
    }
}
