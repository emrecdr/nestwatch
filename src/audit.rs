//! Append-only security audit log.
//!
//! A SYSTEM service has no console, so `tracing`'s stdout output is invisible on the deployed
//! machine — a stranger could log in and leave no trace the parent can inspect. This records
//! the security-relevant events (who logged in, from where, and the sensitive actions taken)
//! as one JSON object per line in the ACL-hardened data dir, and exposes them read-only to the
//! authenticated parent via `GET /api/audit`.
//!
//! Writes are best-effort: a failure is logged and dropped, never propagated to a handler —
//! auditing must not be able to break the control path. The log lives inside the data dir that
//! `install` locks to SYSTEM + Administrators, so the standard-user child can't read or delete
//! it (the `(OI)(CI)` inheritance flags cover files created later).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Map, Value};

/// Rotate once the log passes this size, keeping a single `.1` backup. Events are a few per
/// session, so this is a slow-moving cap that just bounds unbounded growth.
const MAX_BYTES: u64 = 2 * 1024 * 1024;

/// The audit sink. `None` path = disabled (used in tests so they never touch disk).
pub struct AuditLog {
    path: Option<PathBuf>,
    /// Serializes concurrent appends so lines never interleave.
    write_lock: Mutex<()>,
}

impl AuditLog {
    /// An audit log writing newline-delimited JSON to `path`.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path: Some(path),
            write_lock: Mutex::new(()),
        }
    }

    /// A no-op audit log (tests, or any context without a data dir).
    pub fn disabled() -> Self {
        Self {
            path: None,
            write_lock: Mutex::new(()),
        }
    }

    /// Append one event. `event` is a short type tag; `fields` is a JSON object of extra
    /// attributes (never secrets — no passwords, cookies, or hashes). Best-effort.
    pub fn record(&self, event: &str, fields: Value) {
        let Some(path) = &self.path else { return };

        let mut obj = Map::new();
        obj.insert("ts".into(), Value::String(timestamp()));
        obj.insert("event".into(), Value::String(event.to_string()));
        if let Value::Object(extra) = fields {
            obj.extend(extra);
        }
        let line = Value::Object(obj).to_string();

        let _guard = self.write_lock.lock().unwrap_or_else(|p| p.into_inner());
        if let Err(e) = append_line(path, &line) {
            tracing::warn!(error = %e, "audit log write failed");
        }
    }

    /// The most recent `limit` events, newest first. Malformed lines are skipped; a missing
    /// file (nothing logged yet) yields an empty list.
    pub fn recent(&self, limit: usize) -> Vec<Value> {
        let Some(path) = &self.path else {
            return Vec::new();
        };
        let Ok(content) = std::fs::read_to_string(path) else {
            return Vec::new();
        };
        let mut events: Vec<Value> = content
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let start = events.len().saturating_sub(limit);
        let mut recent = events.split_off(start);
        recent.reverse();
        recent
    }
}

/// RFC3339 UTC timestamp with millisecond precision.
fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Append a line, rotating first if the file has grown past [`MAX_BYTES`]. Does not create the
/// parent directory — the data dir is created and ACL-hardened by `install`; if it's absent
/// (e.g. running uninstalled) the open simply fails and the event is dropped.
fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    if let Ok(meta) = std::fs::metadata(path)
        && meta.len() > MAX_BYTES
    {
        let _ = std::fs::rename(path, path.with_extension("jsonl.1"));
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{line}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn records_and_reads_back_newest_first() {
        let dir = std::env::temp_dir().join(format!("nw-audit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = AuditLog::new(dir.join("audit.jsonl"));

        log.record("auth_failure", json!({ "src_ip": "192.168.1.9" }));
        log.record("auth_success", json!({ "src_ip": "192.168.1.20" }));

        let recent = log.recent(10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0]["event"], "auth_success", "newest first");
        assert_eq!(recent[1]["event"], "auth_failure");
        assert!(recent[0]["ts"].is_string(), "timestamp present");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn disabled_is_a_noop() {
        let log = AuditLog::disabled();
        log.record("auth_success", json!({}));
        assert!(log.recent(10).is_empty());
    }
}
