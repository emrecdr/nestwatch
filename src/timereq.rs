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

/// The persisted request queue.
pub struct TimeRequests {
    log: JsonlLog,
    /// Monotonic component of the id, so ids are unique within a run.
    counter: AtomicU64,
}

impl TimeRequests {
    pub fn new(path: PathBuf) -> Self {
        Self {
            log: JsonlLog::new(path),
            counter: AtomicU64::new(0),
        }
    }

    /// A no-op queue (tests): submit returns an id but nothing persists, so `pending` is empty.
    pub fn disabled() -> Self {
        Self {
            log: JsonlLog::disabled(),
            counter: AtomicU64::new(0),
        }
    }

    /// Append a new request. Returns its id, or `None` if the pending cap is reached (so the
    /// caller can respond identically either way and never leak queue state to the child).
    pub fn submit(&self, minutes: u32, reason: &str) -> Option<String> {
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

    /// Approve (`true`) or deny (`false`) a pending request. Returns the resolved request (so the
    /// caller knows how many minutes to grant), or `None` if the id isn't currently pending.
    pub fn resolve(&self, id: &str, approve: bool) -> Option<PendingRequest> {
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
    pub fn count_and_check(&self, ip: IpAddr) -> Result<(), AppError> {
        let now = Instant::now();
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

    #[test]
    fn submit_pending_resolve_roundtrip() {
        let dir = std::env::temp_dir().join(format!("nw-timereq-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let q = TimeRequests::new(dir.join("timereq.jsonl"));

        let id = q.submit(30, "homework done").unwrap();
        let pending = q.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].minutes, 30);
        assert_eq!(pending[0].id, id);

        let resolved = q.resolve(&id, true).unwrap();
        assert_eq!(resolved.minutes, 30);
        assert!(q.pending().is_empty(), "approved request no longer pending");
        assert!(q.resolve(&id, true).is_none(), "already resolved");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pending_cap_is_enforced() {
        let dir = std::env::temp_dir().join(format!("nw-timereq-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let q = TimeRequests::new(dir.join("timereq.jsonl"));
        for _ in 0..MAX_PENDING {
            assert!(q.submit(10, "").is_some());
        }
        assert!(q.submit(10, "").is_none(), "over the pending cap");
        let _ = std::fs::remove_dir_all(&dir);
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
        let lim = SubmitLimiter::new(2, Duration::from_secs(60));
        assert!(lim.count_and_check(ip).is_ok());
        assert!(lim.count_and_check(ip).is_ok());
        assert!(
            lim.count_and_check(ip).is_err(),
            "3rd exceeds the quota of 2"
        );
        // A different IP is unaffected.
        let other: IpAddr = "192.168.1.6".parse().unwrap();
        assert!(lim.count_and_check(other).is_ok());
    }
}
