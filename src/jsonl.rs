//! A tiny append-only JSON-Lines log: one JSON object per line, best-effort writes, size-based
//! rotation. Shared by the security audit log ([`crate::audit`]) and the usage-history log
//! ([`crate::usage`]) so the store logic lives in exactly one place.
//!
//! Writes are best-effort: a failure is logged and dropped, never propagated — logging must not
//! be able to break the control path. The file lives inside the data dir that `install` locks to
//! SYSTEM + Administrators, so a standard-user child can't read or delete it (the `(OI)(CI)`
//! inheritance flags cover files created later).
//!
//! # These logs are deliberately not durable across power loss
//!
//! [`append_line`] opens, writes and drops the handle. There is **no `sync_all`**, and that is a
//! decision rather than an omission — worth stating because every other write into this data dir
//! is the other way and reads as the house style: `config.rs` and `rules.rs`'s tally sidecar both
//! go through `write_atomic` (temp file → `fsync` → rename), and `sessionstore.rs` argues its own
//! fsync at length.
//!
//! What a hard power cut costs here is the last few seconds of appended lines. It is bounded, and
//! it is bounded in the direction that matters: readers already tolerate a torn line (see
//! `a_corrupt_line_does_not_break_a_filtered_read`), appends are well under a kilobyte, and the
//! events an attacker would want gone are their own `auth_failure` rows — a handful of which is
//! thin evidence to destroy at the price of pulling the plug at exactly the right instant.
//!
//! Against that, syncing every line means a durability barrier on whichever thread called
//! `record`, and for the audit log that is an axum handler on the async runtime.
//! `docs/DECLINED-OPTIONS.md` already declines the weaker version of this question — moving the
//! audit append off the runtime — on the grounds that all its call sites are bounded by one human
//! action. An fsync per line is the same trade with a worse constant.
//!
//! Recorded here so the asymmetry with the three files that *do* sync reads as a choice, and so
//! nobody has to re-derive it. Revisit if an event on this path ever becomes clock-driven, which
//! is the same trigger that entry names.

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
        // Rotate before appending, and leave a mark if anything was destroyed. Inside the lock, so
        // a concurrent writer cannot land a line between the rotation and its own marker.
        if let Some(lost) = rotate_if_over_size(path)
            && let Err(e) = append_line(path, &envelope(ROTATED_EVENT, lost))
        {
            tracing::warn!(error = %e, "jsonl rotation marker write failed");
        }
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

    /// The live file and its single rotated backup, oldest events first.
    ///
    /// **Backup first: rotation renames the live file to `.1`, so its events are the older ones.**
    /// The one place that ordering lives, for the same reason [`newest_first`](Self::newest_first)
    /// exists — the plain and filtered rotation-inclusive readers had a copy each, and only one
    /// carried the explanation. The other was correct by luck, with nothing pointing a future
    /// editor at the rule it was obeying.
    fn with_rotated(path: &Path, read: impl Fn(&Path) -> Vec<Value>) -> Vec<Value> {
        let mut events = read(&path.with_extension("jsonl.1"));
        events.extend(read(path));
        events
    }

    /// The most recent `limit` events, newest first. Malformed lines are skipped; a missing
    /// file (nothing logged yet) yields an empty list.
    pub fn recent(&self, limit: usize) -> Vec<Value> {
        let Some(path) = &self.path else {
            return Vec::new();
        };
        Self::newest_first(Self::read_events(path), limit)
    }

    /// Whether any line in the current file contains `needle`, without parsing one of them.
    ///
    /// A **reject filter, never the decision** — the same contract as
    /// [`read_events_matching`](Self::read_events_matching), and the same escaping caveat: `record`
    /// never `\u`-escapes ASCII, and a writer that did would cause a miss rather than a false
    /// accept. `false` is authoritative, because a value that never appears as text cannot appear
    /// in a parsed field. `true` means only "worth parsing"; the caller still does the real check.
    ///
    /// Deliberately scans the current file only, matching [`recent`](Self::recent) exactly. Reading
    /// the rotated backup here would be worse than useless: it could answer `true` for a code that
    /// `recent` can no longer see, which is a slower path to the same answer, and answering over a
    /// *wider* set than the caller searches is how a reject filter turns into a wrong one.
    pub fn any_line_contains(&self, needle: &str) -> bool {
        let Some(path) = &self.path else {
            return false;
        };
        std::fs::read_to_string(path).is_ok_and(|c| c.contains(needle))
    }

    /// Like [`Self::recent`], but also reads the single rotated `.1` backup.
    ///
    /// `recent` deliberately does not: after a rotation up to 2 MiB of history is still on disk
    /// but unreachable, which is fine for the audit table (which wants the latest events) and not
    /// fine for a report whose whole purpose is looking backwards.
    pub fn recent_including_rotated(&self, limit: usize) -> Vec<Value> {
        let Some(path) = &self.path else {
            return Vec::new();
        };
        Self::newest_first(Self::with_rotated(path, Self::read_events), limit)
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
        Self::newest_first(
            Self::with_rotated(path, |p| Self::read_events_matching(p, event)),
            limit,
        )
    }
}

/// The event tag written when a rotation destroys a previous backup.
///
/// `pub` so a reader can find these rows without matching a string literal — the same reason
/// `screentime::ROLLUP_EVENT` is.
pub const ROTATED_EVENT: &str = "rotated";

/// Wrap `fields` in the `{ts, event, …}` envelope every line here carries.
///
/// Extracted from [`JsonlLog::record`] when rotation gained a line of its own: two places building
/// the envelope by hand is how the two come to disagree about the shape, and the shape is what
/// every reader in this module matches on.
fn envelope(event: &str, fields: Value) -> String {
    let mut obj = Map::new();
    obj.insert("ts".into(), Value::String(timestamp()));
    obj.insert("event".into(), Value::String(event.to_string()));
    if let Value::Object(extra) = fields {
        obj.extend(extra);
    }
    Value::Object(obj).to_string()
}

/// RFC3339 UTC timestamp with millisecond precision.
fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Rotate if the live file has outgrown [`MAX_BYTES`], reporting what the rotation **destroyed**.
///
/// Returns `None` when nothing rotated, and also when a rotation destroyed nothing — the first one
/// an install ever does, where there is no `.1` to clobber. A marker for that would be a line
/// saying no data was lost, which is noise in a file a parent reads.
///
/// # Why the loss is reported at all
///
/// `O67` records that rotation *is* the retention policy here: two generations of 2 MiB, and the
/// older is overwritten with no prune, no setting and no notice. The report card already shows
/// *History from …*, but that is **derived from what survived** — it answers "how far back can I
/// see", and cannot distinguish a fresh install from one that silently dropped a year. This is the
/// other half: a row saying what went, which then travels into `GET /api/export` where a parent
/// checking the tool against itself can see the gap rather than having to notice its absence.
///
/// # Cost, and why it is not a full read
///
/// The obvious version counts the lines in the doomed backup, which means reading up to 2 MiB on
/// whichever thread called `record` — and for the audit log that is an axum handler on the async
/// runtime. Instead this takes the file's **size** (a `stat`) and its **last timestamp**, read from
/// a small window at the end. So the amortised cost is one `stat` plus one short read per 2 MiB
/// written, and the answer a parent needs — *history before this instant is gone* — is the part
/// that survives.
///
/// Best-effort throughout, like everything else in this file: a backup that cannot be measured
/// still gets rotated, and the marker simply carries `null` for what could not be read. Refusing to
/// rotate because the marker failed would trade a bounded log for an unbounded one.
fn rotate_if_over_size(path: &Path) -> Option<Value> {
    let over = std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES);
    if !over {
        return None;
    }

    let backup = path.with_extension("jsonl.1");
    // Measured before the rename, because the rename is what destroys it.
    let doomed = std::fs::metadata(&backup).ok().map(|m| m.len());
    let through = doomed.and_then(|_| last_timestamp(&backup));

    if std::fs::rename(path, &backup).is_err() {
        // Nothing was destroyed, so nothing to report. The live file stays and simply keeps
        // growing until the next attempt — the pre-existing behaviour on a failed rename.
        return None;
    }

    // A first rotation clobbers nothing — and neither does one over an empty backup, which is not
    // reachable today (a `.1` is made by renaming a file that just passed 2 MiB) but would produce
    // a row announcing that nothing was lost. Silence is the honest answer to "what went".
    let bytes = doomed.filter(|b| *b > 0)?;
    Some(serde_json::json!({
        "discarded_bytes": bytes,
        "discarded_through": through,
        "note": "rotation keeps two generations; everything older than this was deleted",
    }))
}

/// The `ts` of the last well-formed line in `path`, without reading the whole file.
///
/// Reads a window from the end and takes the last complete line in it. `None` if the file is
/// unreadable, or if the tail holds no parseable line — which is honest: the alternative is
/// scanning backwards until one turns up, and the whole point of this function is not to read 2 MiB
/// on a logging call.
fn last_timestamp(path: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};

    /// Enough for several lines at the ~124 bytes a real row measures, and small enough that the
    /// read is a single page or two.
    const TAIL: u64 = 8 * 1024;

    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut tail = String::new();
    // Lossy on purpose: seeking into the middle of a multi-byte character is expected, and it must
    // cost the first line rather than the whole read.
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    tail.push_str(&String::from_utf8_lossy(&bytes));

    tail.lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find_map(|v| {
            v.get("ts")
                .and_then(Value::as_str)
                .map(std::string::ToString::to_string)
        })
}

/// Append a line. Does not create the parent directory — the data dir is created and ACL-hardened
/// by `install`; if it's absent (e.g. running uninstalled) the open simply fails and the event is
/// dropped.
fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
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
        let dir = crate::testutil::ScratchDir::new("jsonl");
        let log = JsonlLog::new(dir.join("log.jsonl"));

        log.record("first", json!({ "n": 1 }));
        log.record("second", json!({ "n": 2 }));

        let recent = log.recent(10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0]["event"], "second", "newest first");
        assert_eq!(recent[1]["event"], "first");
        assert!(recent[0]["ts"].is_string(), "timestamp present");
    }

    /// The filter keeps what it should and drops what it shouldn't.
    #[test]
    fn a_filtered_read_returns_only_the_named_event() {
        let dir = crate::testutil::ScratchDir::new("jsonl-filter");
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
        let dir = crate::testutil::ScratchDir::new("jsonl-decoy");
        let path = dir.join("decoy.jsonl");
        let _ = std::fs::remove_file(&path);
        let log = JsonlLog::new(path);

        // The child controls this string: `/time-request` takes a free-text reason.
        log.record("time_request", json!({ "reason": "wanted" }));
        log.record("wanted", json!({ "real": true }));

        let hits = log.recent_matching_including_rotated("wanted", 10);
        assert_eq!(hits.len(), 1, "the decoy must not match: {hits:?}");
        assert_eq!(hits[0]["real"], true);
    }

    /// A filtered read reaches into the rotated backup, like its unfiltered sibling.
    #[test]
    fn a_filtered_read_reaches_past_the_backup_boundary() {
        let dir = crate::testutil::ScratchDir::new("jsonl-frot");
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
    }

    /// A truncated or corrupt line is skipped, not fatal — the same tolerance the unfiltered
    /// reader has, since the filtered one is now the report's only route to this file.
    #[test]
    fn a_corrupt_line_does_not_break_a_filtered_read() {
        let dir = crate::testutil::ScratchDir::new("jsonl-corrupt");
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
    }

    #[test]
    fn disabled_is_a_noop() {
        let log = JsonlLog::disabled();
        log.record("x", json!({}));
        assert!(log.recent(10).is_empty());
    }

    #[test]
    fn rotates_when_over_size() {
        let dir = crate::testutil::ScratchDir::new("jsonl-rot");
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
    }

    /// A rotation that destroys a previous backup says what it destroyed.
    ///
    /// `O67`'s remaining half. The report card's *History from …* is derived from what survived, so
    /// it cannot tell a fresh install from one that silently dropped a year; this row is the other
    /// side of that, and it travels into `GET /api/export` where a parent can check the tool
    /// against itself.
    #[test]
    fn a_rotation_that_destroys_a_backup_records_what_went() {
        let dir = crate::testutil::ScratchDir::new("jsonl-mark");
        let path = dir.join("marked.jsonl");
        let backup = path.with_extension("jsonl.1");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&backup);

        // A backup with a known newest timestamp, and a live file already over the threshold.
        std::fs::write(
            &backup,
            "{\"ts\":\"2024-01-01T00:00:00.000Z\",\"event\":\"old\"}\n\
             {\"ts\":\"2024-03-04T05:06:07.000Z\",\"event\":\"newest_lost\"}\n",
        )
        .unwrap();
        std::fs::write(&path, vec![b'x'; MAX_BYTES as usize + 1]).unwrap();

        let log = JsonlLog::new(path.clone());
        log.record("after_rotate", json!({}));

        let rows = log.recent(10);
        let marker = rows
            .iter()
            .find(|r| r["event"] == ROTATED_EVENT)
            .unwrap_or_else(|| panic!("no rotation marker was written: {rows:#?}"));

        assert!(
            marker["discarded_bytes"].as_u64().is_some_and(|b| b > 0),
            "the marker must say how much went: {marker}"
        );
        assert_eq!(
            marker["discarded_through"], "2024-03-04T05:06:07.000Z",
            "the marker must carry the NEWEST timestamp in the destroyed backup — that is the \
             instant before which history no longer exists: {marker}"
        );

        // The caller's own line still landed, and after the marker.
        assert_eq!(rows[0]["event"], "after_rotate", "newest first: {rows:#?}");
    }

    /// The **first** rotation an install ever does destroys nothing, and says nothing.
    ///
    /// A marker there would be a line reporting that no data was lost, in a file a parent reads.
    /// This is also what keeps `rotates_when_over_size` above meaningful: that test asserts the
    /// live file holds exactly one line afterwards, which is only true while a first rotation
    /// stays silent.
    #[test]
    fn a_first_rotation_destroys_nothing_and_says_nothing() {
        let dir = crate::testutil::ScratchDir::new("jsonl-firstrot");
        let path = dir.join("first.jsonl");
        let backup = path.with_extension("jsonl.1");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&backup);

        std::fs::write(&path, vec![b'x'; MAX_BYTES as usize + 1]).unwrap();
        let log = JsonlLog::new(path.clone());
        log.record("after_rotate", json!({}));

        let rows = log.recent(10);
        assert!(
            !rows.iter().any(|r| r["event"] == ROTATED_EVENT),
            "nothing was destroyed, so nothing should be reported: {rows:#?}"
        );
        assert_eq!(rows.len(), 1, "only the caller's line: {rows:#?}");

        // Same rule for an EMPTY backup. Not reachable today — a `.1` is made by renaming a file
        // that just passed 2 MiB — but the marker is about what was destroyed, and zero bytes is
        // nothing destroyed. Kept because the alternative is a row announcing no loss.
        std::fs::write(&backup, b"").unwrap();
        std::fs::write(&path, vec![b'x'; MAX_BYTES as usize + 1]).unwrap();
        let log = JsonlLog::new(path);
        log.record("second_rotate", json!({}));
        let rows = log.recent(10);
        assert!(
            !rows.iter().any(|r| r["event"] == ROTATED_EVENT),
            "an empty backup destroyed nothing: {rows:#?}"
        );
    }

    #[test]
    fn recent_including_rotated_reaches_past_the_backup_boundary() {
        // A distinct directory from `rotates_when_over_size`, which removes its own
        // `nw-jsonl-rot-{pid}` dir at the end — the two tests sharing one name only survived on
        // timing, and a slow/reordered run could delete files this test was still using.
        let dir = crate::testutil::ScratchDir::new("jsonl-rot2");
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
