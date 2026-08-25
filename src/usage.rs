//! Append-only **usage-history** log: how the machine was used over time — daily screen-time
//! rollups, session start/stop, and the reason behind enforcement actions (curfew/budget locks
//! and shutdowns).
//!
//! Kept in its own file (`usage.jsonl`), separate from the security audit log, so verbose usage
//! rows can't push security events out of the audit log's rotation window and the two have
//! independent retention. Written by the rules enforcer (Phase 4) and surfaced read-only to the
//! parent via `GET /api/usage`.
//!
//! The store mechanics live in [`crate::jsonl`]; this is a distinct newtype so the compiler keeps
//! usage events and security events from being crossed.

use std::path::PathBuf;

use serde_json::Value;

use crate::jsonl::JsonlLog;

pub struct UsageLog(JsonlLog);

impl UsageLog {
    /// A usage log writing `usage.jsonl` at `path`.
    pub fn new(path: PathBuf) -> Self {
        Self(JsonlLog::new(path))
    }

    /// A no-op usage log (tests, or any context without a data dir).
    pub fn disabled() -> Self {
        Self(JsonlLog::disabled())
    }

    /// Record a usage event (e.g. `screentime_daily`, `session_start`, `lock`).
    pub fn record(&self, event: &str, fields: Value) {
        self.0.record(event, fields);
    }

    /// The most recent `limit` events, newest first.
    pub fn recent(&self, limit: usize) -> Vec<Value> {
        self.0.recent(limit)
    }

    /// The most recent `limit` events tagged `event`, newest first, including the rotated backup.
    ///
    /// The screen-time report's route into this file, and the only one that should be used for it:
    /// the rollups it wants are a few dozen lines among every session start, lock and warning ever
    /// written here.
    pub fn recent_matching_including_rotated(&self, event: &str, limit: usize) -> Vec<Value> {
        self.0.recent_matching_including_rotated(event, limit)
    }

    /// The most recent `limit` events, newest first, including the rotated backup.
    pub fn recent_including_rotated(&self, limit: usize) -> Vec<Value> {
        self.0.recent_including_rotated(limit)
    }
}
