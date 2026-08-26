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
/// live stream is the only audit event a *clock* can produce, so it is the only one whose volume
/// is bounded by nothing at all.
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

    /// Count one timer-driven frame. Returns `Some(n)` when a line is due, `n` being the frames since
    /// the previous line.
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
