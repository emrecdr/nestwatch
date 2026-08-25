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

    /// Parse every well-formed line of `path`, oldest first. A missing file yields an empty vec.
    fn read_events(path: &Path) -> Vec<Value> {
        let Ok(content) = std::fs::read_to_string(path) else {
            return Vec::new();
        };
        content
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// Keep the newest `limit` of `events` (which arrive oldest-first) and reverse them, so the
    /// caller gets newest-first. The one place this windowing rule lives, so the plain and
    /// rotation-inclusive readers can't drift apart.
    fn newest_first(mut events: Vec<Value>, limit: usize) -> Vec<Value> {
        let start = events.len().saturating_sub(limit);
        let mut recent = events.split_off(start);
        recent.reverse();
        recent
    }

    /// The most recent `limit` events, newest first. Malformed lines are skipped; a missing
    /// file (nothing logged yet) yields an empty list.
    pub fn recent(&self, limit: usize) -> Vec<Value> {
        let Some(path) = &self.path else {
            return Vec::new();
        };
        Self::newest_first(Self::read_events(path), limit)
    }

    /// Like [`recent`], but also reads the single rotated `.1` backup.
    ///
    /// `recent` deliberately does not: after a rotation up to 2 MiB of history is still on disk
    /// but unreachable, which is fine for the audit table (which wants the latest events) and not
    /// fine for a report whose whole purpose is looking backwards.
    pub fn recent_including_rotated(&self, limit: usize) -> Vec<Value> {
        let Some(path) = &self.path else {
            return Vec::new();
        };
        // Backup first: rotation renames the live file to `.1`, so its events are the older ones.
        let mut events = Self::read_events(&path.with_extension("jsonl.1"));
        events.extend(Self::read_events(path));
        Self::newest_first(events, limit)
    }

    /// Parse only the lines whose `event` tag is `event`, oldest first.
    ///
    /// The `contains` is a **reject filter, never the decision**. A line that survives it is still
    /// parsed and its real `event` field compared, so the substring turning up inside some other
    /// field — a reason string, a routine name — cannot smuggle a row in. What it buys is skipping
    /// `serde_json::from_str` on the lines that cannot possibly match, which is where the cost is:
    /// parsing builds a `Value` tree per line, rejecting one is a memchr scan.
    ///
    /// The one way this can be wrong is a writer that `\u`-escapes ASCII letters in the event
    /// name. [`record`](Self::record) never does, and the consequence would be a dropped row —
    /// which surfaces as a *not measured* day in the report rather than as a wrong number.
    fn read_events_matching(path: &Path, event: &str) -> Vec<Value> {
        let Ok(content) = std::fs::read_to_string(path) else {
            return Vec::new();
        };
        content
            .lines()
            .filter(|line| line.contains(event))
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|v| v.get("event").and_then(Value::as_str) == Some(event))
            .collect()
    }

    /// Like [`recent_including_rotated`](Self::recent_including_rotated), but keeps only events
    /// tagged `event`.
    ///
    /// Exists because the screen-time report wants roughly thirty daily rollup rows out of a log
    /// whose other traffic — session starts and stops, locks, countdown warnings, grants — shares
    /// the file and outnumbers them by orders of magnitude. Reading it unfiltered meant building a
    /// `Value` for every line ever written, on every dashboard load, to keep a few dozen. The cost
    /// scaled with how long the tool had been installed rather than with the window asked for.
    pub fn recent_matching_including_rotated(&self, event: &str, limit: usize) -> Vec<Value> {
        let Some(path) = &self.path else {
            return Vec::new();
        };
        let mut events = Self::read_events_matching(&path.with_extension("jsonl.1"), event);
        events.extend(Self::read_events_matching(path, event));
        Self::newest_first(events, limit)
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

    /// The filter keeps what it should and drops what it shouldn't.
    #[test]
    fn a_filtered_read_returns_only_the_named_event() {
        let dir = std::env::temp_dir().join(format!("nw-jsonl-filter-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mixed.jsonl");
        let _ = std::fs::remove_file(&path);
        let log = JsonlLog::new(path);

        log.record("session_start", json!({}));
        log.record("wanted", json!({ "n": 1 }));
        log.record("lock", json!({ "reason": "budget" }));
        log.record("wanted", json!({ "n": 2 }));

        let hits = log.recent_matching_including_rotated("wanted", 10);
        assert_eq!(hits.len(), 2, "only the two `wanted` rows: {hits:?}");
        assert_eq!(hits[0]["n"], 2, "newest first");
        assert_eq!(hits[1]["n"], 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The pre-filter is a reject step, not the verdict.
    ///
    /// This is the property the optimisation could quietly break: a line mentioning the event name
    /// inside some *other* field passes the cheap `contains` and must then be rejected on its real
    /// `event` tag. Deleting the post-parse check leaves the fast path working and this test
    /// failing, which is the point — a report that silently absorbed a child's routine name as a
    /// screen-time rollup would be very hard to notice.
    #[test]
    fn a_line_merely_mentioning_the_event_name_is_not_matched() {
        let dir = std::env::temp_dir().join(format!("nw-jsonl-decoy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("decoy.jsonl");
        let _ = std::fs::remove_file(&path);
        let log = JsonlLog::new(path);

        // The child controls this string: `/time-request` takes a free-text reason.
        log.record("time_request", json!({ "reason": "wanted" }));
        log.record("wanted", json!({ "real": true }));

        let hits = log.recent_matching_including_rotated("wanted", 10);
        assert_eq!(hits.len(), 1, "the decoy must not match: {hits:?}");
        assert_eq!(hits[0]["real"], true);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A filtered read reaches into the rotated backup, like its unfiltered sibling.
    #[test]
    fn a_filtered_read_reaches_past_the_backup_boundary() {
        let dir = std::env::temp_dir().join(format!("nw-jsonl-frot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("frot.jsonl");
        let backup = path.with_extension("jsonl.1");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&backup);

        std::fs::write(
            &backup,
            "{\"event\":\"keep\",\"n\":1}\n{\"event\":\"drop\"}\n",
        )
        .unwrap();
        std::fs::write(&path, "{\"event\":\"keep\",\"n\":2}\n").unwrap();

        let log = JsonlLog::new(path);
        let hits = log.recent_matching_including_rotated("keep", 10);
        assert_eq!(hits.len(), 2, "both sides of the rotation: {hits:?}");
        assert_eq!(hits[0]["n"], 2, "live file is the newer");
        assert_eq!(hits[1]["n"], 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A truncated or corrupt line is skipped, not fatal — the same tolerance the unfiltered
    /// reader has, since the filtered one is now the report's only route to this file.
    #[test]
    fn a_corrupt_line_does_not_break_a_filtered_read() {
        let dir = std::env::temp_dir().join(format!("nw-jsonl-corrupt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("corrupt.jsonl");
        let _ = std::fs::remove_file(&path);

        // Middle line mentions the event and is not valid JSON — a power cut mid-write.
        std::fs::write(
            &path,
            "{\"event\":\"keep\",\"n\":1}\n{\"event\":\"keep\",\"n\":\n{\"event\":\"keep\",\"n\":3}\n",
        )
        .unwrap();

        let log = JsonlLog::new(path);
        let hits = log.recent_matching_including_rotated("keep", 10);
        assert_eq!(hits.len(), 2, "the two intact rows survive: {hits:?}");

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

    #[test]
    fn recent_including_rotated_reaches_past_the_backup_boundary() {
        // A distinct directory from `rotates_when_over_size`, which removes its own
        // `nw-jsonl-rot-{pid}` dir at the end — the two tests sharing one name only survived on
        // timing, and a slow/reordered run could delete files this test was still using.
        let dir = std::env::temp_dir().join(format!("nw-jsonl-rot2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rot.jsonl");
        let backup = path.with_extension("jsonl.1");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&backup);

        // Simulate a rotation: older events sit in the .1 backup, newer in the live file.
        std::fs::write(&backup, "{\"event\":\"old\"}\n").unwrap();
        std::fs::write(&path, "{\"event\":\"new\"}\n").unwrap();

        let log = JsonlLog::new(path.clone());

        // The existing reader sees only the live file — this is the gap being closed.
        let live_only = log.recent(10);
        assert_eq!(live_only.len(), 1);

        let both = log.recent_including_rotated(10);
        assert_eq!(both.len(), 2, "rotated history must still be reachable");
        assert_eq!(both[0]["event"], "new", "newest first");
        assert_eq!(both[1]["event"], "old");

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&backup);
    }
}
