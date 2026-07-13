//! A tiny append-only JSON-Lines log: one JSON object per line, best-effort writes, size-based
//! rotation. Shared by the security audit log ([`crate::audit`]) and the usage-history log
//! ([`crate::usage`]) so the store logic lives in exactly one place.
//!
//! Writes are best-effort: a failure is logged and dropped, never propagated — logging must not
//! be able to break the control path. The file lives inside the data dir that `install` locks to
//! SYSTEM + Administrators, so a standard-user child can't read or delete it (the `(OI)(CI)`
//! inheritance flags cover files created later).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Map, Value};

/// Rotate once the log passes this size, keeping a single `.1` backup. Events are a few per
/// session, so this is a slow-moving cap that just bounds unbounded growth.
const MAX_BYTES: u64 = 2 * 1024 * 1024;

/// An append-only JSONL sink. `None` path = disabled (used in tests so they never touch disk).
pub struct JsonlLog {
    path: Option<PathBuf>,
    /// Serializes concurrent appends so lines never interleave.
    write_lock: Mutex<()>,
}

impl JsonlLog {
    /// A log writing newline-delimited JSON to `path`.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path: Some(path),
            write_lock: Mutex::new(()),
        }
    }

    /// A no-op log (tests, or any context without a data dir).
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
            tracing::warn!(error = %e, "jsonl log write failed");
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
        let dir = std::env::temp_dir().join(format!("nw-jsonl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = JsonlLog::new(dir.join("log.jsonl"));

        log.record("first", json!({ "n": 1 }));
        log.record("second", json!({ "n": 2 }));

        let recent = log.recent(10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0]["event"], "second", "newest first");
        assert_eq!(recent[1]["event"], "first");
        assert!(recent[0]["ts"].is_string(), "timestamp present");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn disabled_is_a_noop() {
        let log = JsonlLog::disabled();
        log.record("x", json!({}));
        assert!(log.recent(10).is_empty());
    }

    #[test]
    fn rotates_when_over_size() {
        let dir = std::env::temp_dir().join(format!("nw-jsonl-rot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("log.jsonl");

        // Pre-fill past the rotation threshold, then a single record triggers rotation.
        std::fs::write(&path, vec![b'x'; MAX_BYTES as usize + 1]).unwrap();
        let log = JsonlLog::new(path.clone());
        log.record("after_rotate", json!({}));

        // The oversized file was moved aside to `.jsonl.1`…
        assert!(
            path.with_extension("jsonl.1").exists(),
            "rotated backup exists"
        );
        // …and the live file now holds only the fresh line.
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.lines().count(), 1);
        assert!(content.contains("after_rotate"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
