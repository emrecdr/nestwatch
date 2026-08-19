//! Append-only **daily screen-time rollups**: one row per completed day.
//!
//! Deliberately its own file (`screentime.jsonl`), separate from `usage.jsonl`. The usage log
//! carries point-in-time events — session edges, countdowns — that a child can generate at will by
//! cycling lock/unlock. Roughly 14k such events rotate the 2 MiB log, and with it any daily
//! rollups sharing the file. Keeping rollups here means noise cannot evict them, whether that
//! noise is incidental or deliberate.
//!
//! This is the same split, and the same reasoning, that already separates `usage` from `audit`
//! (see `usage.rs`) — one level down.
//!
//! Storage is only half of it: this module also owns the pure aggregation that turns stored rows
//! into the windowed [`Report`] served at `GET /api/screentime` — parsing (`parse_row`), merging
//! the two logs that can both carry a given day, and the running/average/vs-previous-period math.
//! None of it touches the clock or the filesystem, so it's exhaustively unit-tested below without
//! a temp directory. (The row *written* on rollover is built by `rules::rollup_row`, which this
//! module's `parse_row` is the deliberate mirror image of.)

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::NaiveDate;
use serde::Serialize;
use serde_json::Value;

use crate::jsonl::JsonlLog;

pub struct ScreentimeLog(JsonlLog);

impl ScreentimeLog {
    /// A rollup log writing `screentime.jsonl` at `path`.
    pub fn new(path: PathBuf) -> Self {
        Self(JsonlLog::new(path))
    }

    /// A no-op log (tests, or any context without a data dir).
    pub fn disabled() -> Self {
        Self(JsonlLog::disabled())
    }

    /// Record one completed day. The event tag is fixed — this file holds nothing else.
    pub fn record(&self, fields: Value) {
        self.0.record("screentime_daily", fields);
    }

    /// The most recent `limit` rows, newest first.
    pub fn recent(&self, limit: usize) -> Vec<Value> {
        self.0.recent(limit)
    }

    /// The most recent `limit` rows, newest first, including the rotated backup.
    pub fn recent_including_rotated(&self, limit: usize) -> Vec<Value> {
        self.0.recent_including_rotated(limit)
    }
}

/// One app's share of a day, in whole minutes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppMinutes {
    pub name: String,
    pub minutes: u64,
}

/// One day in the report window.
///
/// `measured` distinguishes the two things an absent number can mean. A day with no row is one the
/// service never ticked through — the PC was off, or the enforcer was stopped or wedged while the
/// machine was in use. Those are not the same as a day it watched and saw nothing, so
/// `minutes_used` is `None` for the first and `Some(0)` for the second. Collapsing them would let
/// a dead enforcer render exactly like a well-behaved child.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DayRow {
    pub date: String,
    pub measured: bool,
    pub minutes_used: Option<u64>,
    pub budget: Option<u32>,
    pub over_budget: bool,
    pub apps: Vec<AppMinutes>,
}

impl DayRow {
    /// A day the service ticked through. `measured` is derived here rather than passed in, so it
    /// cannot disagree with `minutes_used` — the two used to be set side by side at two call
    /// sites, one `null`-vs-`0` slip away from the confusion this whole type exists to prevent.
    fn measured(date: NaiveDate, row: &ParsedRow) -> Self {
        Self {
            date: date.to_string(),
            measured: true,
            minutes_used: Some(row.minutes_used),
            budget: row.budget,
            over_budget: row
                .budget
                .is_some_and(|b| b > 0 && row.minutes_used > u64::from(b)),
            apps: row.apps.clone(),
        }
    }

    /// A day with no row: the service never ticked. Not the same as a measured zero, and the only
    /// constructor that can produce `minutes_used: None`.
    fn unmeasured(date: NaiveDate) -> Self {
        Self {
            date: date.to_string(),
            measured: false,
            minutes_used: None,
            budget: None,
            over_budget: false,
            apps: Vec::new(),
        }
    }
}

/// The windowed report served at `GET /api/screentime`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    pub days: Vec<DayRow>,
    /// Total across **measured** days only.
    pub total_mins: u64,
    /// How many days in the window actually had a row.
    pub measured_days: usize,
    /// Mean over measured days, or `None` if none were. Averaging unmeasured days as zero would
    /// understate usage by exactly the amount that is unknown.
    pub daily_avg_mins: Option<u64>,
    /// Total over the immediately preceding window of the same length, or `None` if it had no
    /// measured days.
    pub prev_total_mins: Option<u64>,
    /// Percentage change against `prev_total_mins`. `None` — never `0` — when there is no
    /// baseline, so an absent comparison cannot read as "no change".
    pub change_pct: Option<i64>,
}

/// A parsed rollup row, before windowing.
struct ParsedRow {
    minutes_used: u64,
    budget: Option<u32>,
    apps: Vec<AppMinutes>,
}

/// Parse one stored row. Returns `None` for anything malformed — a corrupt line must not be able
/// to break the report.
fn parse_row(v: &Value) -> Option<(NaiveDate, ParsedRow)> {
    let date = NaiveDate::parse_from_str(v.get("date")?.as_str()?, "%Y-%m-%d").ok()?;
    let minutes_used = v.get("minutes_used")?.as_u64()?;
    let budget = v
        .get("budget")
        .and_then(Value::as_u64)
        .map(|b| u32::try_from(b).unwrap_or(u32::MAX));

    let mut apps: Vec<AppMinutes> = v
        .get("apps")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(name, mins)| {
                    Some(AppMinutes {
                        name: name.clone(),
                        minutes: mins.as_u64()?,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    // Heaviest first, then by name so the order is stable for equal minutes.
    apps.sort_by(|a, b| b.minutes.cmp(&a.minutes).then_with(|| a.name.cmp(&b.name)));

    Some((
        date,
        ParsedRow {
            minutes_used,
            budget,
            apps,
        },
    ))
}

/// Sum the measured minutes over an inclusive date range.
fn window_total(
    by_date: &BTreeMap<NaiveDate, ParsedRow>,
    from: NaiveDate,
    to: NaiveDate,
) -> (u64, usize) {
    let mut total = 0;
    let mut count = 0;
    for (_, row) in by_date.range(from..=to) {
        total += row.minutes_used;
        count += 1;
    }
    (total, count)
}

/// Every rollup row available, from both the dedicated store and the legacy usage log.
///
/// Installs that predate `screentime.jsonl` have their history only in `usage.jsonl`, so the
/// report reads both and lets [`build_report`] collapse the overlap. Without this the chart would
/// start empty on upgrade, which reads as "he used nothing" — the exact confusion this feature
/// exists to remove.
pub fn history_rows(screentime: &ScreentimeLog, usage: &crate::usage::UsageLog) -> Vec<Value> {
    let mut rows = screentime.recent_including_rotated(usize::MAX);
    rows.extend(
        usage
            .recent_including_rotated(usize::MAX)
            .into_iter()
            .filter(|v| v.get("event").and_then(Value::as_str) == Some("screentime_daily")),
    );
    rows
}

/// Build the report for the `days` completed days ending yesterday.
///
/// Pure: every input is a parameter, so the whole thing is unit-testable without touching disk or
/// a clock. The `date` field is treated as untrusted — it originates from a wall clock the child
/// may be able to move — so rows dated today or later are ignored and duplicate dates collapse
/// deterministically.
pub fn build_report(rows: &[Value], today: NaiveDate, days: u32) -> Report {
    // Defence in depth: Task 5's `/api/screentime` handler already clamps `days` to 365 before
    // calling here, but this function is public and pure, so it must not rely on the caller for
    // panic-safety against an absurd window width.
    let days = days.clamp(1, 365);

    let mut by_date: BTreeMap<NaiveDate, ParsedRow> = BTreeMap::new();
    for v in rows {
        let Some((date, parsed)) = parse_row(v) else {
            continue;
        };
        // Only completed days. Today's rollup has not run, and anything later is a clock artefact.
        if date >= today {
            continue;
        }
        // The same day can arrive from both logs. Prefer the richer row so the per-app detail in
        // screentime.jsonl wins over a legacy usage.jsonl row that has none.
        match by_date.get(&date) {
            Some(existing) if existing.apps.len() >= parsed.apps.len() => {}
            _ => {
                by_date.insert(date, parsed);
            }
        }
    }

    let Some(end) = today.pred_opt() else {
        // `today` is the minimum representable date; there is no completed day to report.
        return Report {
            days: Vec::new(),
            total_mins: 0,
            measured_days: 0,
            daily_avg_mins: None,
            prev_total_mins: None,
            change_pct: None,
        };
    };
    let span = chrono::Duration::days(i64::from(days) - 1);
    let start = end - span;

    let mut day_rows = Vec::new();
    let mut cursor = start;
    loop {
        day_rows.push(match by_date.get(&cursor) {
            Some(row) => DayRow::measured(cursor, row),
            None => DayRow::unmeasured(cursor),
        });
        if cursor >= end {
            break;
        }
        let Some(next) = cursor.succ_opt() else { break };
        cursor = next;
    }

    let (total_mins, measured_days) = window_total(&by_date, start, end);

    let (prev_total_mins, prev_measured) = match start.pred_opt() {
        Some(prev_end) => {
            let (t, c) = window_total(&by_date, prev_end - span, prev_end);
            (Some(t), c)
        }
        None => (None, 0),
    };

    let change_pct = match prev_total_mins {
        // A fully-measured previous window totaling zero is a real baseline, not an absent one —
        // `prev_total_mins` already reports `Some(0)` for it, and `change_pct` must agree. Zero to
        // zero is a genuine 0% change. Zero to nonzero has an undefined percentage (division by
        // zero), so it stays `None` — do not "fix" this into a divide-by-zero later.
        Some(0) if prev_measured > 0 => {
            if total_mins == 0 {
                Some(0)
            } else {
                None
            }
        }
        Some(prev) if prev_measured > 0 => Some(
            (i64::try_from(total_mins).unwrap_or(i64::MAX)
                - i64::try_from(prev).unwrap_or(i64::MAX))
                * 100
                / i64::try_from(prev).unwrap_or(1),
        ),
        _ => None,
    };

    Report {
        days: day_rows,
        total_mins,
        measured_days,
        daily_avg_mins: (measured_days > 0).then(|| total_mins / measured_days as u64),
        prev_total_mins: (prev_measured > 0).then_some(prev_total_mins).flatten(),
        change_pct,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch path under the OS temp dir, unique per process and test.
    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nw-screentime-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(format!("{name}.jsonl"))
    }

    #[test]
    fn a_recorded_day_round_trips() {
        let path = tmp("round_trip");
        let _ = std::fs::remove_file(&path);
        let log = ScreentimeLog::new(path.clone());

        log.record(serde_json::json!({ "date": "2026-08-16", "minutes_used": 200 }));

        let rows = log.recent(10);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["event"], "screentime_daily");
        assert_eq!(rows[0]["date"], "2026-08-16");
        assert_eq!(rows[0]["minutes_used"], 200);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_disabled_log_writes_nothing_and_reads_empty() {
        let log = ScreentimeLog::disabled();
        log.record(serde_json::json!({ "date": "2026-08-16" }));
        assert!(log.recent(10).is_empty());
    }

    use chrono::NaiveDate;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn row(date: &str, minutes: u64, budget: u32) -> Value {
        serde_json::json!({
            "event": "screentime_daily", "date": date,
            "minutes_used": minutes, "budget": budget, "apps": {}
        })
    }

    /// `measured` and `minutes_used` encode the same fact and must never disagree. The two
    /// constructors above make that structural inside `build_report`; this catches a third
    /// construction site being added later that sets them by hand and gets one wrong.
    #[test]
    fn measured_always_agrees_with_minutes_used() {
        let rows = vec![
            row("2026-08-16", 120, 180),
            row("2026-08-14", 0, 180), // measured zero — still measured
        ];
        let r = build_report(&rows, d("2026-08-17"), 4);
        assert_eq!(
            r.days.len(),
            4,
            "window should include two days with no row at all"
        );
        for day in &r.days {
            assert_eq!(
                day.measured,
                day.minutes_used.is_some(),
                "{}: measured={} but minutes_used={:?}",
                day.date,
                day.measured,
                day.minutes_used
            );
        }
    }

    #[test]
    fn a_gap_is_null_and_a_real_zero_is_zero() {
        // Window is the 3 completed days before 2026-08-17: 14th, 15th, 16th.
        let rows = vec![row("2026-08-14", 120, 180), row("2026-08-16", 0, 180)];
        let r = build_report(&rows, d("2026-08-17"), 3);

        assert_eq!(r.days.len(), 3);
        assert_eq!(r.days[0].date, "2026-08-14");
        assert_eq!(r.days[0].minutes_used, Some(120));

        // The 15th has no row: the service never ticked. Unknown, NOT zero.
        assert_eq!(r.days[1].date, "2026-08-15");
        assert_eq!(r.days[1].minutes_used, None);
        assert!(!r.days[1].measured);

        // The 16th has a row saying zero: measured, and genuinely unused.
        assert_eq!(r.days[2].minutes_used, Some(0));
        assert!(r.days[2].measured);

        // The average must not treat the unmeasured day as a zero — that would understate by
        // exactly the amount nobody knows.
        assert_eq!(r.measured_days, 2);
        assert_eq!(r.total_mins, 120);
        assert_eq!(r.daily_avg_mins, Some(60));
    }

    #[test]
    fn today_is_excluded_because_its_rollup_has_not_run() {
        let rows = vec![row("2026-08-17", 999, 180), row("2026-08-16", 60, 180)];
        let r = build_report(&rows, d("2026-08-17"), 2);
        assert!(
            r.days.iter().all(|x| x.date != "2026-08-17"),
            "today must never appear; its row cannot exist yet"
        );
        assert_eq!(r.total_mins, 60);
    }

    #[test]
    fn future_dated_rows_are_ignored() {
        // A clock pushed forward then back can leave a row dated ahead of today.
        let rows = vec![row("2027-01-01", 500, 180), row("2026-08-16", 30, 180)];
        let r = build_report(&rows, d("2026-08-17"), 2);
        assert_eq!(
            r.total_mins, 30,
            "a future-dated row must not inflate the total"
        );
    }

    #[test]
    fn duplicate_dates_collapse_to_the_richer_row() {
        // The same date arrives from both usage.jsonl (no apps) and screentime.jsonl (with apps).
        let plain = row("2026-08-16", 90, 180);
        let rich = serde_json::json!({
            "event": "screentime_daily", "date": "2026-08-16",
            "minutes_used": 90, "budget": 180, "apps": {"game.exe": 45}
        });
        let r = build_report(&[plain, rich], d("2026-08-17"), 1);

        assert_eq!(r.days.len(), 1);
        assert_eq!(r.total_mins, 90, "a duplicated day must be counted once");
        assert_eq!(r.days[0].apps.len(), 1);
        assert_eq!(r.days[0].apps[0].name, "game.exe");
    }

    #[test]
    fn out_of_order_input_aggregates_identically_to_sorted() {
        let sorted = vec![row("2026-08-14", 10, 180), row("2026-08-15", 20, 180)];
        let jumbled = vec![row("2026-08-15", 20, 180), row("2026-08-14", 10, 180)];
        assert_eq!(
            build_report(&sorted, d("2026-08-16"), 2).days,
            build_report(&jumbled, d("2026-08-16"), 2).days
        );
    }

    #[test]
    fn change_pct_is_null_without_a_baseline_not_zero() {
        // Only the current window has data; the previous window is empty.
        let rows = vec![row("2026-08-16", 100, 180)];
        let r = build_report(&rows, d("2026-08-17"), 1);
        assert_eq!(
            r.change_pct, None,
            "an absent comparison must not render as 'no change'"
        );
    }

    #[test]
    fn change_pct_compares_against_the_preceding_window() {
        // days=1: current window is the 16th, previous window is the 15th.
        let rows = vec![row("2026-08-16", 150, 180), row("2026-08-15", 100, 180)];
        let r = build_report(&rows, d("2026-08-17"), 1);
        assert_eq!(r.prev_total_mins, Some(100));
        assert_eq!(r.change_pct, Some(50));
    }

    #[test]
    fn change_pct_is_zero_when_both_windows_are_measured_and_zero() {
        // days=1: current window is the 16th (measured, 0 minutes), previous window is the 15th
        // (also measured, also 0 minutes). A fully-measured week of nothing followed by another
        // fully-measured week of nothing is a real 0% change, not an absent comparison.
        let rows = vec![row("2026-08-16", 0, 180), row("2026-08-15", 0, 180)];
        let r = build_report(&rows, d("2026-08-17"), 1);
        assert_eq!(r.prev_total_mins, Some(0));
        assert_eq!(r.change_pct, Some(0));
    }

    #[test]
    fn change_pct_is_null_from_a_zero_baseline_to_nonzero() {
        // The previous window is measured but totals zero; the current window has real usage.
        // Percentage change from a zero baseline is mathematically undefined, so this must stay
        // None — never a synthesized number, and never a divide-by-zero.
        let rows = vec![row("2026-08-16", 50, 180), row("2026-08-15", 0, 180)];
        let r = build_report(&rows, d("2026-08-17"), 1);
        assert_eq!(r.prev_total_mins, Some(0));
        assert_eq!(r.change_pct, None);
    }

    #[test]
    fn over_budget_is_flagged_only_when_a_budget_applies() {
        let over = row("2026-08-16", 200, 180);
        let unlimited = serde_json::json!({
            "event": "screentime_daily", "date": "2026-08-15",
            "minutes_used": 500, "budget": 0, "apps": {}
        });
        let r = build_report(&[over, unlimited], d("2026-08-17"), 2);
        assert!(
            !r.days[0].over_budget,
            "budget 0 means no limit, so never over"
        );
        assert!(r.days[1].over_budget);
    }

    #[test]
    fn malformed_rows_are_skipped_not_fatal() {
        let junk = serde_json::json!({ "event": "screentime_daily", "date": "not-a-date" });
        let missing = serde_json::json!({ "event": "screentime_daily" });
        let good = row("2026-08-16", 42, 180);
        let r = build_report(&[junk, missing, good], d("2026-08-17"), 1);
        assert_eq!(r.total_mins, 42);
    }

    #[test]
    fn history_merges_legacy_usage_rows_with_the_dedicated_store() {
        let st_path = tmp("merge_st");
        let us_path = tmp("merge_us");
        let _ = std::fs::remove_file(&st_path);
        let _ = std::fs::remove_file(&us_path);

        let st = ScreentimeLog::new(st_path.clone());
        st.record(serde_json::json!({ "date": "2026-08-16", "minutes_used": 90, "budget": 180 }));

        let us = crate::usage::UsageLog::new(us_path.clone());
        us.record(
            "screentime_daily",
            serde_json::json!({ "date": "2026-08-15", "minutes_used": 60, "budget": 180 }),
        );
        // Noise in the usage log must not reach the report. It carries a valid "date" and
        // "minutes_used" — everything `parse_row` needs to accept it — and a date inside the
        // 3-day window below but distinct from the two real rows, so a broken event filter would
        // show up as a 3rd measured day and a much larger total, not silently vanish into
        // `parse_row`'s unrelated "no date" rejection.
        us.record(
            "session_start",
            serde_json::json!({ "date": "2026-08-14", "minutes_used": 999 }),
        );

        let rows = history_rows(&st, &us);
        let r = build_report(&rows, d("2026-08-17"), 3);

        assert_eq!(
            r.measured_days, 2,
            "both logs contribute, but not the noise"
        );
        assert_eq!(r.total_mins, 150);

        let _ = std::fs::remove_file(&st_path);
        let _ = std::fs::remove_file(&us_path);
    }

    #[test]
    fn history_reaches_legacy_rows_past_the_usage_logs_own_rotation_boundary() {
        // A pre-existing install with no screentime.jsonl yet keeps its entire history in
        // usage.jsonl, and a moderately-used install is plausibly already past that log's own
        // 2 MiB rotation by the time it upgrades. The older half of its legacy screentime_daily
        // rows then lives only in usage.jsonl.1 — history_rows must still find it.
        let st_path = tmp("legacy_rot_st");
        let us_path = tmp("legacy_rot_us");
        let us_backup = us_path.with_extension("jsonl.1");
        for p in [&st_path, &us_path, &us_backup] {
            let _ = std::fs::remove_file(p);
        }

        // No dedicated store yet — this install predates screentime.jsonl entirely.
        let st = ScreentimeLog::new(st_path.clone());

        // Older legacy rollup, sitting only in the rotated backup...
        std::fs::write(
            &us_backup,
            "{\"event\":\"screentime_daily\",\"date\":\"2026-08-15\",\"minutes_used\":60}\n",
        )
        .unwrap();
        // ...and a newer one in the live file.
        std::fs::write(
            &us_path,
            "{\"event\":\"screentime_daily\",\"date\":\"2026-08-16\",\"minutes_used\":90}\n",
        )
        .unwrap();
        let us = crate::usage::UsageLog::new(us_path.clone());

        let rows = history_rows(&st, &us);
        let r = build_report(&rows, d("2026-08-17"), 2);

        assert_eq!(
            r.measured_days, 2,
            "the row behind usage.jsonl's own rotation boundary must still be reachable"
        );
        assert_eq!(r.total_mins, 150);

        for p in [&st_path, &us_path, &us_backup] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn rotating_the_usage_log_does_not_evict_rollup_history() {
        let st_path = tmp("evict_st");
        let us_path = tmp("evict_us");
        let us_backup = us_path.with_extension("jsonl.1");
        for p in [&st_path, &us_path, &us_backup] {
            let _ = std::fs::remove_file(p);
        }

        let st = ScreentimeLog::new(st_path.clone());
        st.record(serde_json::json!({ "date": "2026-08-16", "minutes_used": 90, "budget": 180 }));

        // Push the usage log past its 2 MiB rotation threshold, then write once more so the next
        // append performs the rename. This is what ~14k scripted lock/unlock cycles would do.
        let us = crate::usage::UsageLog::new(us_path.clone());
        std::fs::write(&us_path, "x".repeat(2 * 1024 * 1024 + 1)).unwrap();
        us.record("session_start", serde_json::json!({}));

        assert!(
            us_backup.exists(),
            "precondition: the usage log should have rotated"
        );

        // The rollup is in a different file, so flooding one cannot evict the other.
        let rows = history_rows(&st, &us);
        let r = build_report(&rows, d("2026-08-17"), 1);
        assert_eq!(
            r.measured_days, 1,
            "flooding the usage log must not cost us a day of screen-time history"
        );
        assert_eq!(r.total_mins, 90);

        for p in [&st_path, &us_path, &us_backup] {
            let _ = std::fs::remove_file(p);
        }
    }
}
