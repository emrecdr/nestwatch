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

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use chrono::NaiveDate;
use serde::Serialize;
use serde_json::Value;

use crate::jsonl::JsonlLog;

/// The event tag every daily rollup row carries, in both `screentime.jsonl` and — for installs
/// predating that file — the legacy `usage.jsonl`. Named once now that it is read back as a filter
/// as well as written, so the two uses cannot drift.
pub const ROLLUP_EVENT: &str = "screentime_daily";

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
        self.0.record(ROLLUP_EVENT, fields);
    }

    /// The most recent `limit` rows, newest first.
    pub fn recent(&self, limit: usize) -> Vec<Value> {
        self.0.recent(limit)
    }

    /// The most recent `limit` rollup rows, newest first, including the rotated backup.
    ///
    /// Filtered by event tag even though this file holds only rollups. It costs a substring scan
    /// the read was doing anyway, and it means a stray line — a hand edit, a row half-written when
    /// the power went — is skipped here rather than reaching `parse_row`.
    pub fn recent_including_rotated(&self, limit: usize) -> Vec<Value> {
        self.0
            .recent_matching_including_rotated(ROLLUP_EVENT, limit)
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
    /// Minutes each app was **running**. What the per-app limits are enforced against.
    pub apps: Vec<AppMinutes>,
    /// Minutes each app actually had **focus**. Empty when the day predates foreground tracking,
    /// or when the watcher wasn't running — which is unknown focus, not zero focus.
    pub focused: Vec<AppMinutes>,
    /// Minutes per browser page title. Same absent-means-unknown rule as `focused`.
    pub pages: Vec<AppMinutes>,
    /// Minutes per **app group** — the categories a parent defined. Same absent-means-unknown rule
    /// again: empty for any day recorded before groups were written to history, which is not the
    /// same as a day whose categories went unused.
    pub groups: Vec<AppMinutes>,
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
            focused: row.focused.clone(),
            pages: row.pages.clone(),
            groups: row.groups.clone(),
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
            focused: Vec::new(),
            pages: Vec::new(),
            groups: Vec::new(),
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
    /// Minutes each app was **running**, summed across every measured day in the window.
    ///
    /// The report could previously answer "what did he use last Tuesday" and not "how much Roblox
    /// this month", which is the question a parent actually arrives with. Each panel showed exactly
    /// one day, because that is all `DayRow` could offer.
    ///
    /// Heaviest first, capped at [`TOP_OVER_WINDOW`]. Summed over measured days only — the same
    /// rule `total_mins` follows, and for the same reason: a day nothing was watching contributes
    /// no evidence, and treating it as a zero would understate by exactly the unknown amount.
    pub app_totals: Vec<AppMinutes>,
    /// Minutes each app actually had **focus**, summed the same way. Empty for a window whose days
    /// all predate foreground tracking, which is not the same as zero focus.
    pub focus_totals: Vec<AppMinutes>,
    /// Minutes per browser page title, summed the same way.
    pub page_totals: Vec<AppMinutes>,
    /// Minutes per **app group**, summed the same way. The category view: "Games: 14 h" rather
    /// than twenty rows of executable names.
    pub group_totals: Vec<AppMinutes>,
    /// Apps that turned up for the first time on the most recent day with focus evidence.
    ///
    /// `None` when the question cannot be answered honestly — see [`first_seen_in`]. The UI must
    /// distinguish that from `Some` with an empty `apps`, which means "checked, nothing new".
    pub first_seen: Option<FirstSeen>,
}

/// How many rows each windowed total carries.
///
/// Wider than the live "today" panel because this is the view a parent scrolls deliberately rather
/// than glances at, and a month legitimately spreads across more programs than an evening. Still a
/// cap: page titles are attacker-influenced and the number of distinct ones over ninety days has no
/// natural bound.
pub const TOP_OVER_WINDOW: usize = 25;

/// Largest baseline the "first seen" comparison will build before giving up.
///
/// The baseline is a union of app names across every retained day, and those names come from the
/// watcher — a process running as the child. `foreground::MAX_APPS` bounds one day at 200, so a
/// year of deliberately-renamed executables could otherwise reach five figures. A cap alone would
/// be worse than none: a truncated baseline silently reports familiar apps as new, which is a false
/// alarm aimed at the parent. So overflow abandons the answer entirely rather than degrading it —
/// the same "absent rather than wrong" rule the rest of this file follows for unmeasured days.
///
/// 2,000 is far above any honest machine. A busy PC sees tens of distinct programs in a year.
const MAX_BASELINE_APPS: usize = 2_000;

/// How many apps the "first seen" list will name.
///
/// Small on purpose: this is a *notice*, and a notice listing thirty things is a list. If a day
/// genuinely introduces more than this, the count still reports the total so nothing is hidden.
const TOP_FIRST_SEEN: usize = 8;

/// Apps that had focus on one day and on no earlier day in the retained history.
///
/// The most actionable thing a usage report can say is not a total but a **change**: something
/// appeared that never appeared before. A newly installed game, a chat client the parent has not
/// heard of. Today a parent has to spot it themselves in a list sorted by minutes, where a program
/// used for twelve minutes sits near the bottom.
///
/// Detection is by **use, not installation**, because that is the only signal available here — this
/// product deliberately watches no registry and reads no install log. That also matches what the
/// market does: Qustodio surfaces a new app once it has been used at least once, not when it lands
/// on disk. An app installed and never opened is not a fact about a child's day.
// No `Default`. It would produce `baseline_days: 0`, which is exactly the condition
// `first_seen_in` returns `None` for — a fabricated answer sitting inside the state the `Option`
// exists to keep out, rendering as "First seen , against 0 earlier days of history".
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FirstSeen {
    /// The day these were first seen, `YYYY-MM-DD`. The most recent **completed** day with focus
    /// evidence — never today, because today's rollup has not been written yet.
    pub date: String,
    /// The names, heaviest first, capped at [`TOP_FIRST_SEEN`].
    pub apps: Vec<AppMinutes>,
    /// How many first appeared, which can exceed `apps.len()`.
    pub count: usize,
    /// How many earlier days carried focus evidence. This is the **strength of the claim** and the
    /// UI must show it: "new, against 40 days of history" and "new, against 1 day" are different
    /// statements, and the second is nearly worthless.
    pub baseline_days: usize,
    /// The check **stopped**: the baseline passed [`MAX_BASELINE_APPS`], so no answer was computed.
    ///
    /// A fourth state rather than a third spelling of `None`, because this is the one case the cap
    /// was built for and it was the one case nothing recorded. Reaching 2,000 distinct executable
    /// names takes deliberate renaming — a child cycling names to keep every day looking new — and
    /// that produced exactly the dashboard and exactly the audit log of a fresh install where the
    /// watcher had never run. Disabling the check silently and permanently was the reward for
    /// attacking it.
    ///
    /// When this is set, `apps` is empty and `count` is zero on purpose: a truncated baseline would
    /// report familiar programs as new, which is a false alarm aimed at the parent. `baseline_days`
    /// is how far it got before giving up, not a claim of strength.
    pub baseline_overflow: bool,
}

/// Compute [`FirstSeen`] for the newest day in `history` that carries focus evidence.
///
/// `None` — rather than an empty result — whenever the question cannot be answered honestly:
///
/// * no day carries focus evidence (the watcher has never reported, or every row predates it);
/// * the newest such day is the only one, so there is no baseline and *everything* would look new;
/// * the baseline exceeded [`MAX_BASELINE_APPS`], where a partial answer would invent new apps.
///
/// A day with an empty `focused` map is treated as carrying **no evidence**, not as evidence of
/// nothing. That is the same rule `DayRow::focused` documents: an absent map means the watcher was
/// not running or the build did not record it, and reading it as "no app had focus" would make
/// every app look new the following day.
fn first_seen_in(history: &BTreeMap<NaiveDate, ParsedRow>) -> Option<FirstSeen> {
    // Newest day with evidence is the subject; everything strictly before it is the baseline.
    let (&target, row) = history
        .iter()
        .rev()
        .find(|(_, row)| !row.focused.is_empty())?;

    let mut baseline: BTreeSet<&str> = BTreeSet::new();
    let mut baseline_days = 0usize;
    for (_, earlier) in history.range(..target) {
        if earlier.focused.is_empty() {
            continue;
        }
        baseline_days += 1;
        for app in &earlier.focused {
            baseline.insert(app.name.as_str());
            if baseline.len() > MAX_BASELINE_APPS {
                // Say that it stopped, rather than returning the `None` a fresh install also
                // returns. See `FirstSeen::baseline_overflow`.
                return Some(FirstSeen {
                    date: target.to_string(),
                    apps: Vec::new(),
                    count: 0,
                    baseline_days,
                    baseline_overflow: true,
                });
            }
        }
    }
    if baseline_days == 0 {
        return None;
    }

    let mut fresh: Vec<AppMinutes> = row
        .focused
        .iter()
        .filter(|a| !baseline.contains(a.name.as_str()))
        .cloned()
        .collect();
    // A day that introduced nothing is the normal case, and it is still an answer worth returning:
    // the UI can say so, or say nothing, but it must not be confused with "we could not tell".
    // `None` is reserved for the latter. That case needs no branch of its own — an empty `fresh`
    // sorts and truncates to itself and reports `count: 0`, which is precisely the answer.
    //
    // There is a second `FirstSeen` literal above, on the overflow path, and keeping the two in
    // step is the compiler's job rather than a reader's: the struct deliberately does not derive
    // `Default`, so a field added to one literal and not the other fails to build instead of
    // defaulting quietly. (This comment previously asserted the opposite.)
    fresh.sort_by(|a, b| b.minutes.cmp(&a.minutes).then_with(|| a.name.cmp(&b.name)));
    let count = fresh.len();
    fresh.truncate(TOP_FIRST_SEEN);
    Some(FirstSeen {
        date: target.to_string(),
        apps: fresh,
        count,
        baseline_days,
        baseline_overflow: false,
    })
}

/// Sum one `DayRow` field across the window, heaviest first, capped.
///
/// Takes an extractor rather than a key string so a typo cannot silently produce an empty list —
/// the compiler picks the field, not a lookup that quietly misses.
fn totals_across<F>(days: &[DayRow], pick: F) -> Vec<AppMinutes>
where
    F: Fn(&DayRow) -> &Vec<AppMinutes>,
{
    let mut sums: BTreeMap<&str, u64> = BTreeMap::new();
    for day in days {
        // Unmeasured days carry empty vectors, so they contribute nothing without a special case —
        // but the filter is explicit anyway, because "contributes nothing by accident" and
        // "excluded on purpose" read the same right up until `DayRow::unmeasured` changes.
        if !day.measured {
            continue;
        }
        for entry in pick(day) {
            *sums.entry(entry.name.as_str()).or_insert(0) += entry.minutes;
        }
    }

    let mut rows: Vec<AppMinutes> = sums
        .into_iter()
        .map(|(name, minutes)| AppMinutes {
            name: name.to_string(),
            minutes,
        })
        .collect();
    // Heaviest first, ties by name so the order is stable across refreshes.
    rows.sort_by(|a, b| b.minutes.cmp(&a.minutes).then_with(|| a.name.cmp(&b.name)));
    rows.truncate(TOP_OVER_WINDOW);
    rows
}

/// A parsed rollup row, before windowing.
struct ParsedRow {
    minutes_used: u64,
    budget: Option<u32>,
    apps: Vec<AppMinutes>,
    focused: Vec<AppMinutes>,
    pages: Vec<AppMinutes>,
    groups: Vec<AppMinutes>,
}

impl ParsedRow {
    /// How rich this row is, for picking a winner when the same date arrives from both logs.
    ///
    /// Ordered as a tuple rather than a single count, because the two questions are not
    /// commensurable. **Whether a row knows about focus at all** comes first: rows written before
    /// foreground tracking have no `focused` or `pages` key, and one of those naming forty apps
    /// would otherwise outrank a modern row naming one — the richer row losing the tie and having
    /// its extra dimensions discarded, invisibly, because the day still renders. Only within the
    /// same generation does the field count decide.
    fn detail(&self) -> (bool, bool, usize) {
        // Newest generation first, then the next, then the count. Ranking on the count alone let a
        // wide row from an older build outrank a narrow modern one and silently discard the richer
        // data — a row that knows about a *kind* of measurement beats one that does not, however
        // few entries it happens to carry that day.
        //
        // Groups came after focus, so they lead. A row from a build that recorded neither sorts
        // below both, which is what we want when the same date arrives from `screentime.jsonl` and
        // the legacy `usage.jsonl`.
        let knows_groups = !self.groups.is_empty();
        let knows_focus = !self.focused.is_empty() || !self.pages.is_empty();
        (
            knows_groups,
            knows_focus,
            self.apps.len() + self.focused.len() + self.pages.len() + self.groups.len(),
        )
    }
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

    Some((
        date,
        ParsedRow {
            minutes_used,
            budget,
            apps: app_minutes(v, "apps"),
            focused: app_minutes(v, "focused"),
            pages: app_minutes(v, "pages"),
            groups: app_minutes(v, "groups"),
        },
    ))
}

/// Read one `{name: minutes}` map out of a stored row, heaviest app first.
///
/// An **absent** key yields an empty vec, which callers must read as "not recorded" rather than
/// as zero — every row written before foreground tracking existed has no `focused` key at all,
/// and rendering those as a confident zero would misreport a year of history.
fn app_minutes(v: &Value, key: &str) -> Vec<AppMinutes> {
    let mut apps: Vec<AppMinutes> = v
        .get(key)
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
    apps
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
    // Filtered by the *reader*, not after it. `usage.jsonl` is the noisy log — session starts and
    // stops, locks, countdown warnings, grants — and the rollups are a few dozen lines among all of
    // it, up to 4 MiB once the rotated backup is counted. Reading it whole and discarding 99% built
    // a `Value` tree per line on every dashboard load, and the cost scaled with how long the tool
    // had been installed rather than with the thirty days actually asked for.
    rows.extend(usage.recent_matching_including_rotated(ROLLUP_EVENT, usize::MAX));
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
            Some(existing) if existing.detail() >= parsed.detail() => {}
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
            app_totals: Vec::new(),
            focus_totals: Vec::new(),
            page_totals: Vec::new(),
            group_totals: Vec::new(),
            first_seen: None,
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

    // From `by_date` — every completed day in the retained history — rather than from `day_rows`,
    // which is only the chosen window. "First seen" must mean first in everything we know, not
    // first in the last seven days; otherwise narrowing the range would invent new apps, and the
    // same app would be "new" or not depending on which button the parent last pressed.
    let first_seen = first_seen_in(&by_date);

    let app_totals = totals_across(&day_rows, |d| &d.apps);
    let focus_totals = totals_across(&day_rows, |d| &d.focused);
    let page_totals = totals_across(&day_rows, |d| &d.pages);
    let group_totals = totals_across(&day_rows, |d| &d.groups);

    Report {
        first_seen,
        app_totals,
        focus_totals,
        page_totals,
        group_totals,
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

    /// A rollup row carrying focus minutes, for the first-seen tests below.
    fn focus_row(date: &str, focused: &[(&str, u64)]) -> Value {
        let map: serde_json::Map<String, Value> = focused
            .iter()
            .map(|(n, m)| ((*n).to_string(), Value::from(*m)))
            .collect();
        serde_json::json!({
            "event": "screentime_daily", "date": date, "minutes_used": 60,
            "apps": {}, "focused": map
        })
    }

    fn report_of(rows: &[Value]) -> Report {
        build_report(rows, d("2026-08-20"), 30)
    }

    /// The whole point: an app that shows up on the newest day and on no earlier one.
    #[test]
    fn an_app_never_seen_before_is_reported_as_first_seen() {
        let rows = vec![
            focus_row("2026-08-17", &[("chrome.exe", 40)]),
            focus_row("2026-08-18", &[("chrome.exe", 50)]),
            focus_row("2026-08-19", &[("chrome.exe", 30), ("discord.exe", 25)]),
        ];
        let fs = report_of(&rows).first_seen.expect("answerable");
        assert_eq!(fs.date, "2026-08-19");
        assert_eq!(
            fs.apps.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            vec!["discord.exe"],
            "chrome.exe was there on both earlier days and is not new"
        );
        assert_eq!(fs.count, 1);
        assert_eq!(fs.baseline_days, 2, "two earlier days carried evidence");
    }

    /// A quiet day is an answer, not a failure to answer. The UI needs to tell "checked, nothing
    /// new" apart from "cannot tell", and collapsing them would make a working feature look broken.
    #[test]
    fn a_day_that_introduced_nothing_answers_with_an_empty_list() {
        let rows = vec![
            focus_row("2026-08-18", &[("chrome.exe", 50)]),
            focus_row("2026-08-19", &[("chrome.exe", 30)]),
        ];
        let fs = report_of(&rows).first_seen.expect("still answerable");
        assert!(fs.apps.is_empty());
        assert_eq!(fs.count, 0);
        assert_eq!(fs.baseline_days, 1);
        // The answer is about the newest day carrying evidence, not an older one — otherwise an
        // empty list could be a stale day's verdict rather than today's.
        assert_eq!(fs.date, "2026-08-19");
    }

    /// With nothing to compare against, *every* app is trivially new — which is not a finding, it
    /// is the absence of one. Reporting it would greet a parent with a list of everything their
    /// child uses, labelled as new, on the first day the watcher ran.
    #[test]
    fn the_first_ever_day_of_evidence_reports_nothing() {
        let rows = vec![focus_row(
            "2026-08-19",
            &[("chrome.exe", 30), ("roblox.exe", 90)],
        )];
        assert!(
            report_of(&rows).first_seen.is_none(),
            "one day of history cannot establish that anything is new"
        );
    }

    /// A day with no `focused` map is unknown focus, never zero focus. Counting it as a baseline
    /// day would make everything used the next day look new — the exact failure `DayRow::focused`
    /// warns about, one layer up.
    #[test]
    fn days_without_focus_evidence_are_not_a_baseline() {
        let rows = vec![
            row("2026-08-17", 120, 180), // no `focused` key at all
            row("2026-08-18", 120, 180),
            focus_row("2026-08-19", &[("roblox.exe", 90)]),
        ];
        assert!(
            report_of(&rows).first_seen.is_none(),
            "two focus-less days are not evidence that roblox.exe is new"
        );
    }

    /// The baseline reaches past the report window. Otherwise narrowing the range would invent new
    /// apps, and the same app would be "new" or not depending on which button was last pressed.
    #[test]
    fn the_baseline_is_all_history_not_just_the_window() {
        let rows = vec![
            focus_row("2026-07-01", &[("roblox.exe", 90)]), // ~7 weeks before `today`
            focus_row("2026-08-18", &[("chrome.exe", 50)]),
            focus_row("2026-08-19", &[("roblox.exe", 30)]),
        ];
        // A 3-day window: 2026-07-01 is far outside it, but still counts as having seen roblox.
        let narrow = build_report(&rows, d("2026-08-20"), 3)
            .first_seen
            .expect("answerable");
        assert!(
            narrow.apps.is_empty(),
            "roblox.exe was seen in July; a 3-day window must not call it new: {:?}",
            narrow.apps
        );
        assert_eq!(narrow.baseline_days, 2);
    }

    /// The names come from a process running as the child, so the baseline is attacker-influenced.
    /// A truncated baseline would report familiar apps as new — a false alarm aimed at the parent —
    /// so overflow abandons the answer instead of degrading it.
    #[test]
    fn an_oversized_baseline_gives_up_rather_than_crying_wolf() {
        let many: Vec<(String, u64)> = (0..=MAX_BASELINE_APPS)
            .map(|i| (format!("app{i}.exe"), 1))
            .collect();
        let refs: Vec<(&str, u64)> = many.iter().map(|(n, m)| (n.as_str(), *m)).collect();
        let rows = vec![
            focus_row("2026-08-18", &refs),
            focus_row("2026-08-19", &[("brand-new.exe", 10)]),
        ];
        let fs = report_of(&rows)
            .first_seen
            .expect("an abandoned check must say so rather than vanish");
        assert!(
            fs.baseline_overflow,
            "the check stopped; the report has to carry that it stopped"
        );
        assert!(
            fs.apps.is_empty(),
            "a baseline that could not be held completely must not be used to name anything new"
        );
    }

    /// The give-up state is the one the cap exists for, and it must not look like a quiet day.
    ///
    /// Reaching 2,000 distinct executable names takes deliberate renaming — the behaviour the cap's
    /// own comment names as the reason it exists. Before this, that child produced `None`, which is
    /// also what a fresh install produces, so the dashboard and the audit log were identical to a
    /// machine where the watcher had never run. Silently and permanently disabling the check was
    /// the reward for attacking it.
    #[test]
    fn an_abandoned_check_is_distinguishable_from_a_quiet_day() {
        let quiet = report_of(&[
            focus_row("2026-08-18", &[("chrome.exe", 30)]),
            focus_row("2026-08-19", &[("chrome.exe", 20)]),
        ])
        .first_seen
        .expect("a day that introduced nothing is still an answer");
        assert!(!quiet.baseline_overflow);
        assert!(quiet.apps.is_empty());

        let many: Vec<(String, u64)> = (0..=MAX_BASELINE_APPS)
            .map(|i| (format!("app{i}.exe"), 1))
            .collect();
        let refs: Vec<(&str, u64)> = many.iter().map(|(n, m)| (n.as_str(), *m)).collect();
        let overflowed = report_of(&[
            focus_row("2026-08-18", &refs),
            focus_row("2026-08-19", &[("brand-new.exe", 10)]),
        ])
        .first_seen
        .expect("an abandoned check must say so");
        assert!(overflowed.baseline_overflow);

        assert_ne!(
            quiet.baseline_overflow, overflowed.baseline_overflow,
            "these two must not render as the same blank space"
        );
    }

    /// A day introducing more than the display cap still reports the true total, so the notice
    /// cannot understate what happened.
    #[test]
    fn the_count_is_the_truth_even_when_the_list_is_capped() {
        let fresh: Vec<(String, u64)> = (0..TOP_FIRST_SEEN + 5)
            .map(|i| (format!("new{i}.exe"), (i as u64 + 1) * 10))
            .collect();
        let refs: Vec<(&str, u64)> = fresh.iter().map(|(n, m)| (n.as_str(), *m)).collect();
        let rows = vec![
            focus_row("2026-08-18", &[("chrome.exe", 50)]),
            focus_row("2026-08-19", &refs),
        ];
        let fs = report_of(&rows).first_seen.expect("answerable");
        assert_eq!(fs.apps.len(), TOP_FIRST_SEEN, "the list is capped");
        assert_eq!(fs.count, TOP_FIRST_SEEN + 5, "the count is not");
        // Heaviest first, so the cap keeps what matters most.
        assert_eq!(fs.apps[0].minutes, (TOP_FIRST_SEEN as u64 + 5) * 10);
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

    /// A day's row carries both numbers: how long each app was open, and how much of that the
    /// child was actually looking at it.
    #[test]
    fn focused_minutes_are_reported_beside_running_minutes() {
        let row = serde_json::json!({
            "event": "screentime_daily", "date": "2026-08-16",
            "minutes_used": 90, "budget": 180,
            "apps": {"roblox.exe": 60},
            "focused": {"roblox.exe": 40},
        });
        let r = build_report(&[row], d("2026-08-17"), 1);

        assert_eq!(r.days[0].apps[0].minutes, 60, "60 minutes with it open");
        assert_eq!(
            r.days[0].focused[0].minutes, 40,
            "40 of them actually looking at it"
        );
        assert_eq!(r.days[0].focused[0].name, "roblox.exe");
    }

    /// Every row written before this feature existed lacks the key entirely. That must read as
    /// "nobody was watching", not as "he looked at nothing" — the same distinction `measured`
    /// draws for the day as a whole. Reporting a confident zero for a year of history would be
    /// the most misleading thing this feature could do.
    #[test]
    fn a_row_predating_foreground_tracking_claims_no_focus_data() {
        let legacy = row("2026-08-16", 90, 180);
        let r = build_report(&[legacy], d("2026-08-17"), 1);

        assert!(
            r.days[0].focused.is_empty(),
            "an absent key is unknown focus, never measured-zero focus"
        );
        assert_eq!(
            r.days[0].minutes_used,
            Some(90),
            "the day itself is still measured"
        );
    }

    /// Browser page titles ride alongside the app figures, under the same absent-means-unknown
    /// rule — a day with no `pages` key was one nothing was watching, not one nothing was read.
    #[test]
    fn page_titles_are_reported_and_an_absent_key_stays_empty() {
        let with_pages = serde_json::json!({
            "event": "screentime_daily", "date": "2026-08-16",
            "minutes_used": 90, "apps": {"chrome.exe": 60},
            "pages": {"Roblox": 45, "Homework": 10},
        });
        let r = build_report(&[with_pages], d("2026-08-17"), 1);
        assert_eq!(r.days[0].pages.len(), 2);
        assert_eq!(r.days[0].pages[0].name, "Roblox", "heaviest page first");
        assert_eq!(r.days[0].pages[0].minutes, 45);

        let legacy = row("2026-08-16", 90, 180);
        let r = build_report(&[legacy], d("2026-08-17"), 1);
        assert!(r.days[0].pages.is_empty(), "absent is unknown, not zero");
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

    /// The same day arrives from both logs, and only one of the two carries focus data.
    ///
    /// `rollup_row` writes an identical row to `usage.jsonl` and `screentime.jsonl`, so on an
    /// install that upgraded mid-life a given date can have a legacy row (no `focused`) and a new
    /// one (with it), holding the same apps. Collapsing on app count alone lets the legacy row win
    /// a tie and silently discards the focus data — invisible, because the day still renders.
    #[test]
    fn a_tie_on_apps_is_broken_by_whichever_row_has_focus_data() {
        let legacy = serde_json::json!({
            "event": "screentime_daily", "date": "2026-08-16",
            "minutes_used": 90, "budget": 180, "apps": {"roblox.exe": 60},
        });
        let with_focus = serde_json::json!({
            "event": "screentime_daily", "date": "2026-08-16",
            "minutes_used": 90, "budget": 180,
            "apps": {"roblox.exe": 60}, "focused": {"roblox.exe": 40},
        });

        // Legacy first, so it is the incumbent when the richer row arrives.
        let r = build_report(&[legacy, with_focus], d("2026-08-17"), 1);

        assert_eq!(r.total_mins, 90, "still counted once");
        assert_eq!(
            r.days[0].focused.len(),
            1,
            "the row that knows about focus must win the tie"
        );
    }

    /// A wide legacy row must not outrank a narrow modern one.
    ///
    /// Counting fields alone, a row with many apps and no focus data beats a row with fewer apps
    /// *plus* focus and page data — so the richer row loses the tie and its extra dimensions are
    /// discarded. Silent, because the day still renders; only the detail goes missing.
    #[test]
    fn a_row_carrying_focus_data_wins_however_few_apps_it_names() {
        let mut wide = serde_json::Map::new();
        for i in 0..40 {
            wide.insert(format!("app{i}.exe"), serde_json::json!(1));
        }
        let legacy = serde_json::json!({
            "event": "screentime_daily", "date": "2026-08-16",
            "minutes_used": 90, "apps": wide,
        });
        let modern = serde_json::json!({
            "event": "screentime_daily", "date": "2026-08-16",
            "minutes_used": 90,
            "apps": {"roblox.exe": 60},
            "focused": {"roblox.exe": 40},
            "pages": {"Roblox": 20},
        });

        let r = build_report(&[legacy, modern], d("2026-08-17"), 1);

        assert_eq!(
            r.days[0].focused.len(),
            1,
            "the row that knows about focus must win, even naming 39 fewer apps"
        );
        assert_eq!(r.days[0].pages.len(), 1);
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

    /// Category time now survives into history, which is the whole point of this test.
    #[test]
    fn group_minutes_are_recorded_and_summed_across_the_window() {
        let rows = vec![
            serde_json::json!({ "date": "2026-08-20", "minutes_used": 120,
                                "apps": {}, "groups": { "Games": 60, "School": 30 } }),
            serde_json::json!({ "date": "2026-08-21", "minutes_used": 90,
                                "apps": {}, "groups": { "Games": 45 } }),
        ];
        let r = build_report(&rows, d("2026-08-22"), 30);

        assert_eq!(r.group_totals[0].name, "Games");
        assert_eq!(r.group_totals[0].minutes, 105);
        assert_eq!(r.group_totals[1].name, "School");
        assert_eq!(r.group_totals[1].minutes, 30);
    }

    /// A row from a build that never recorded groups reports none — not a day of zero category use.
    #[test]
    fn a_row_predating_group_history_claims_no_group_data() {
        let rows = vec![
            serde_json::json!({ "date": "2026-08-21", "minutes_used": 90,
                                            "apps": { "roblox.exe": 90 } }),
        ];
        let r = build_report(&rows, d("2026-08-22"), 30);

        assert!(
            r.days.iter().any(|x| x.measured),
            "the day itself was measured"
        );
        assert!(
            r.group_totals.is_empty(),
            "an absent `groups` key means the build did not record them, not that nothing was used"
        );
    }

    /// Knowing about a newer kind of measurement outranks carrying more of an older one.
    ///
    /// The same date can arrive from `screentime.jsonl` and the legacy `usage.jsonl`. Ranking on
    /// the entry count alone let a wide legacy row beat a narrow modern one and drop the richer
    /// data without trace — which is why `detail` leads with generation, newest first.
    #[test]
    fn a_row_that_knows_about_groups_wins_against_a_wider_one_that_does_not() {
        let rows = vec![
            // Wide, but from a build that recorded neither focus nor groups.
            serde_json::json!({ "date": "2026-08-21", "minutes_used": 90,
                                "apps": { "a.exe": 10, "b.exe": 10, "c.exe": 10,
                                          "d.exe": 10, "e.exe": 10, "f.exe": 10 } }),
            // Narrow, but it knows what a group is.
            serde_json::json!({ "date": "2026-08-21", "minutes_used": 90,
                                "apps": { "a.exe": 10 }, "groups": { "Games": 40 } }),
        ];
        let r = build_report(&rows, d("2026-08-22"), 30);

        assert_eq!(r.group_totals.len(), 1, "the group-aware row must win");
        assert_eq!(r.group_totals[0].name, "Games");
        assert_eq!(
            r.app_totals.len(),
            1,
            "and it brings its own narrower app data with it"
        );
    }

    /// The question the report could not answer: how much of one app across the whole window.
    #[test]
    fn per_app_totals_sum_across_the_window_heaviest_first() {
        let rows = vec![
            serde_json::json!({ "date": "2026-08-20", "minutes_used": 60,
                    "apps": { "roblox.exe": 40, "chrome.exe": 20 } }),
            serde_json::json!({ "date": "2026-08-21", "minutes_used": 60,
                    "apps": { "roblox.exe": 30, "notepad.exe": 5 } }),
        ];
        let r = build_report(&rows, d("2026-08-22"), 30);

        let names: Vec<&str> = r.app_totals.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["roblox.exe", "chrome.exe", "notepad.exe"]);
        assert_eq!(r.app_totals[0].minutes, 70, "40 + 30 across two days");
        assert_eq!(r.app_totals[1].minutes, 20);
    }

    /// Focus and page totals are summed independently of the running-app totals.
    #[test]
    fn focus_and_page_totals_are_summed_separately() {
        let rows = vec![
            serde_json::json!({ "date": "2026-08-20", "minutes_used": 60,
                    "apps": { "chrome.exe": 60 },
                    "focused": { "chrome.exe": 25 },
                    "pages": { "Roblox": 10 } }),
            serde_json::json!({ "date": "2026-08-21", "minutes_used": 60,
                    "apps": { "chrome.exe": 60 },
                    "focused": { "chrome.exe": 15 },
                    "pages": { "Roblox": 20, "Homework": 5 } }),
        ];
        let r = build_report(&rows, d("2026-08-22"), 30);

        assert_eq!(r.app_totals[0].minutes, 120, "running time");
        assert_eq!(
            r.focus_totals[0].minutes, 40,
            "focus is a different, smaller number"
        );
        assert_eq!(r.page_totals[0].name, "Roblox");
        assert_eq!(r.page_totals[0].minutes, 30);
    }

    /// A day nobody measured contributes nothing — it is not a zero.
    ///
    /// The same rule `total_mins` follows. Counting a gap as zero would understate every total by
    /// exactly the amount that is unknown, which is the confusion `DayRow::measured` exists to stop.
    #[test]
    fn unmeasured_days_do_not_dilute_the_totals() {
        // Only one row in a thirty-day window: the other twenty-nine are gaps.
        let rows = vec![
            serde_json::json!({ "date": "2026-08-21", "minutes_used": 90,
                                "apps": { "roblox.exe": 90 } }),
        ];
        let r = build_report(&rows, d("2026-08-22"), 30);

        assert_eq!(r.days.len(), 30, "the window is still thirty days");
        assert_eq!(r.measured_days, 1);
        assert_eq!(r.app_totals.len(), 1);
        assert_eq!(
            r.app_totals[0].minutes, 90,
            "the gaps add nothing, and subtract nothing"
        );
    }

    /// The cap holds and keeps the heaviest, not the alphabetically luckiest.
    #[test]
    fn windowed_totals_are_capped_at_the_heaviest() {
        let mut apps = serde_json::Map::new();
        for i in 0..(TOP_OVER_WINDOW + 15) {
            apps.insert(format!("app{i:03}.exe"), Value::from(i as u64 + 1));
        }
        let rows =
            vec![serde_json::json!({ "date": "2026-08-21", "minutes_used": 500, "apps": apps })];
        let r = build_report(&rows, d("2026-08-22"), 30);

        assert_eq!(r.app_totals.len(), TOP_OVER_WINDOW);
        assert_eq!(
            r.app_totals[0].name,
            format!("app{:03}.exe", TOP_OVER_WINDOW + 14),
            "the heaviest survives the cap"
        );
    }

    /// A window with no completed days at all still produces a well-formed report.
    #[test]
    fn totals_are_empty_rather_than_absent_when_nothing_was_recorded() {
        let r = build_report(&[], d("2026-08-22"), 30);
        assert!(r.app_totals.is_empty());
        assert!(r.focus_totals.is_empty());
        assert!(r.page_totals.is_empty());
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
