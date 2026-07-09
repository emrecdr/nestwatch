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
