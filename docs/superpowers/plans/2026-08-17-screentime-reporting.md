# Screen-Time Reporting Implementation Plan

> **Historical working document.** This records how one feature was designed and built, kept
> because the reasoning is still useful. It refers to a repository history that was later
> reset, so commit hashes and version numbers in it will not resolve. Nothing here is a
> current instruction — for what the software does now, see the README and CHANGELOG.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the screen-time history Nestwatch already collects into a readable 30-day report, without touching any enforcement path.

**Architecture:** A dedicated append-only `screentime.jsonl` receives one rollup row per day so noisy session events cannot evict it. A pure function aggregates those rows (plus legacy rows from `usage.jsonl`) into a windowed report; a new read-only endpoint serves it; a dashboard card renders it. Nothing reads the new data back into the control path.

**Tech Stack:** Rust 2024, axum 0.8, serde_json, chrono 0.4 (`NaiveDate`), Alpine.js + DaisyUI/Tailwind in `assets/index.html`, inline SVG (no chart library).

**Spec:** `docs/superpowers/specs/2026-08-17-screentime-reporting-design.md`

## Global Constraints

- **No enforcement path may change.** Do not modify `accrue()`, `decide()`, `Targets`, the persisted `Usage` struct/serialization, `ProcessInfo`, or the `SystemControl` trait. If a task seems to require it, stop and report.
- **No new crate dependencies.** `Cargo.toml` `[dependencies]` must be byte-identical at the end.
- **Retention default is 30 days**, clamped 1–365. Source: Apple Screen Time ≈4 weeks, ICO Age Appropriate Design Code data-minimisation for children.
- **`minutes_used: null` means "not measured"; `0` means "measured zero".** These must never collapse. Averages use measured days only.
- **Best-effort logging.** A logging failure is warned and dropped, never propagated (`jsonl.rs` semantics).
- **Every gate must pass before each commit:** `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all-targets`.
- **Commit messages:** no `Co-Authored-By` trailer.
- **Windows cross-check** after any `src/` change:
  `PATH="$HOME/.rustup/toolchains/1.96.0-aarch64-apple-darwin/bin:$PATH" CARGO_TARGET_DIR=target/win-cross CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc cargo clippy --target x86_64-pc-windows-gnu --all-targets -- -D warnings`

---

## File Structure

| File | Responsibility |
|---|---|
| `src/screentime.rs` | **Create.** The `ScreentimeLog` store, the report types, and the pure aggregation. One file because these change together. |
| `src/lib.rs` | **Modify.** Register `pub mod screentime;`. |
| `src/state.rs` | **Modify.** Add `screentime: Arc<ScreentimeLog>` to `AppState`. |
| `src/jsonl.rs` | **Modify.** Add `recent_including_rotated()` — additive; `recent()` is untouched so `/api/audit` and `/api/usage` behave exactly as before. |
| `src/rules.rs` | **Modify.** Snapshot per-app before `decide()`; write the rich rollup row. Logging only. |
| `src/server.rs` | **Modify.** Pass the new log to the enforcer; register `GET /api/screentime`. |
| `src/api.rs` | **Modify.** Add the `screentime` handler. |
| `assets/index.html` | **Modify.** Add the report card. |
| `docs/*` | **Modify.** Checklist, changelog, open findings. |

---

### Task 1: The `screentime.jsonl` store (H1)

**Files:**
- Create: `src/screentime.rs`
- Modify: `src/lib.rs`, `src/state.rs`

**Interfaces:**
- Consumes: `crate::jsonl::JsonlLog` — `new(PathBuf)`, `disabled()`, `record(&str, Value)`, `recent(usize) -> Vec<Value>`
- Produces: `crate::screentime::ScreentimeLog` with `new(PathBuf)`, `disabled()`, `record(Value)`, `recent(usize) -> Vec<Value>`; `AppState.screentime: Arc<ScreentimeLog>`

- [ ] **Step 1: Write the failing test**

Create `src/screentime.rs` with only this content for now:

```rust
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

use std::path::PathBuf;

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
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib screentime`
Expected: FAIL — `error[E0433]: failed to resolve: use of undeclared crate or module 'screentime'` (the module is not registered yet).

- [ ] **Step 3: Register the module**

In `src/lib.rs`, add alongside the other module declarations (keep alphabetical position next to `pub mod rules;` / `pub mod security;`):

```rust
pub mod screentime;
```

In the `lib.rs` module-list doc comment, extend the existing line that reads
`- `audit` / `usage` / `timereq` / `timecode` / `jsonl` — append-only JSONL logs (security` so it also names `screentime`:

```rust
//! - `audit` / `usage` / `screentime` / `timereq` / `timecode` / `jsonl` — append-only JSONL logs
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib screentime`
Expected: PASS — 2 passed.

- [ ] **Step 5: Wire it into `AppState`**

In `src/state.rs`, add the import beside `use crate::usage::UsageLog;`:

```rust
use crate::screentime::ScreentimeLog;
```

Add the field immediately after the existing `pub usage: Arc<UsageLog>,` field:

```rust
    /// Append-only daily screen-time rollups. Separate from `usage` so point-in-time events
    /// cannot rotate the daily history out — see `screentime.rs`.
    pub screentime: Arc<ScreentimeLog>,
```

In `AppState::new`, add the construction beside the existing `usage` line:

```rust
        let screentime = Arc::new(ScreentimeLog::new(dir.join("screentime.jsonl")));
```

and add `screentime,` to the struct literal immediately after `usage,`.

- [ ] **Step 6: Verify the whole suite still builds and passes**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test --all-targets`
Expected: clippy exit 0; all tests pass (162 existing + 2 new = 164).

- [ ] **Step 7: Commit**

```bash
git add src/screentime.rs src/lib.rs src/state.rs
git commit -m "Add a dedicated screentime.jsonl rollup store

Daily rollups get their own append-only file so point-in-time events cannot
evict them. The usage log carries session edges and countdowns, which a child
can generate at will by cycling lock/unlock — roughly 14k events rotate the
2 MiB log and take any rollups sharing it. Same split, same reasoning, as
usage-vs-audit, one level down.

Store only; nothing writes to it yet."
```

---

### Task 2: Write the rollup row, with per-app (Tier 1)

**Files:**
- Modify: `src/rules.rs` (the `run_rules_enforcer` signature and its rollover block), `src/server.rs:164`
- Test: `src/rules.rs` (unit test for the snapshot helper)

**Interfaces:**
- Consumes: `ScreentimeLog::record(Value)` from Task 1
- Produces: rows shaped `{"event":"screentime_daily","date":"YYYY-MM-DD","minutes_used":u64,"budget":u32,"apps":{name: minutes}}` in `screentime.jsonl`. Task 3 parses exactly this shape. `apps` values are **minutes**, not seconds.

- [ ] **Step 1: Write the failing test**

Add to the `mod tests` block in `src/rules.rs` (place it next to the existing `accrue_adds_and_resets_on_new_day` test):

```rust
    #[test]
    fn per_app_minutes_snapshot_converts_seconds_and_drops_empties() {
        let mut per_app = std::collections::BTreeMap::new();
        per_app.insert("game.exe".to_string(), 3_600u64); // 60 min
        per_app.insert("chrome.exe".to_string(), 90u64); // 1 min (integer division)
        per_app.insert("blip.exe".to_string(), 30u64); // 0 min — dropped

        let apps = per_app_minutes(&per_app);

        assert_eq!(apps["game.exe"], 60);
        assert_eq!(apps["chrome.exe"], 1);
        assert!(
            !apps.contains_key("blip.exe"),
            "a sub-minute app must not occupy a row in the daily history"
        );
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib per_app_minutes_snapshot`
Expected: FAIL — `cannot find function 'per_app_minutes' in this scope`.

- [ ] **Step 3: Add the helper**

In `src/rules.rs`, add this free function immediately above `pub async fn run_rules_enforcer` (module-private; it is pure and therefore testable):

```rust
/// Convert a per-app second tally into the whole-minute map stored in the daily rollup.
///
/// Sub-minute entries are dropped: they are noise in a daily report, and keeping them would let a
/// long tail of briefly-run processes dominate the row's size.
fn per_app_minutes(per_app_secs: &BTreeMap<String, u64>) -> serde_json::Map<String, Value> {
    per_app_secs
        .iter()
        .filter_map(|(name, secs)| {
            let mins = secs / 60;
            (mins > 0).then(|| (name.clone(), Value::from(mins)))
        })
        .collect()
}
```

If `Value` is not already imported in `rules.rs`, add `use serde_json::Value;` to the imports at the top of the file.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib per_app_minutes_snapshot`
Expected: PASS.

- [ ] **Step 5: Thread the new log into the enforcer**

In `src/rules.rs`, change the `run_rules_enforcer` signature to add a fourth parameter:

```rust
pub async fn run_rules_enforcer(
    control: Arc<dyn SystemControl>,
    config: Arc<RwLock<Config>>,
    usage_log: Arc<crate::usage::UsageLog>,
    screentime_log: Arc<crate::screentime::ScreentimeLog>,
) {
```

- [ ] **Step 6: Snapshot per-app before `decide()` and write the rich row**

In `run_rules_enforcer`, find these two existing lines (they sit immediately before the `enforcer.decide(` call):

```rust
        let prev_day = enforcer.usage.day;
        let prev_total = enforcer.usage.total_secs;
```

Add a third snapshot directly beneath them. It must be taken **before** `decide()`, because `decide()` calls `accrue()`, which clears the map on a day change:

```rust
        // Snapshot before `decide()` for the same reason as `prev_total`: a rollover inside
        // `accrue()` clears this map, and the row we write describes the day that just ended.
        let prev_per_app = enforcer.usage.per_app_secs.clone();
```

Then find the existing rollover block and add the rich write **after** the existing `usage_log.record(...)` call, leaving that call exactly as it is (the dashboard's event table still reads it):

```rust
        if let Some(pd) = prev_day
            && pd != today
        {
            usage_log.record(
                "screentime_daily",
                serde_json::json!({
                    "date": pd.to_string(),
                    "minutes_used": prev_total / 60,
                    "budget": prev_budget.unwrap_or(budget),
                }),
            );
            // The durable copy, with per-app detail, in a file noisy events cannot rotate away.
            screentime_log.record(serde_json::json!({
                "date": pd.to_string(),
                "minutes_used": prev_total / 60,
                "budget": prev_budget.unwrap_or(budget),
                "apps": per_app_minutes(&prev_per_app),
            }));
        }
```

- [ ] **Step 7: Update the caller**

In `src/server.rs`, find this exact block (~lines 158-169) — note there are **two** similar blocks; this is the second, for the rules enforcer, not the curfew one above it:

```rust
    // Usage-rules enforcement (screen-time budget, blocklist, per-app limits) runs in parallel.
    {
        let control = state.control.clone();
        let config = state.config.clone();
        let usage = state.usage.clone();
        tokio::spawn(async move {
            crate::rules::run_rules_enforcer(control, config, usage).await;
```

Replace those lines with:

```rust
    // Usage-rules enforcement (screen-time budget, blocklist, per-app limits) runs in parallel.
    {
        let control = state.control.clone();
        let config = state.config.clone();
        let usage = state.usage.clone();
        let screentime = state.screentime.clone();
        tokio::spawn(async move {
            crate::rules::run_rules_enforcer(control, config, usage, screentime).await;
```

Leave the `tracing::error!` line and the closing braces exactly as they are. Do **not** touch the curfew block above it.

- [ ] **Step 8: Run the full suite**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test --all-targets`
Expected: clippy exit 0; all tests pass. If any test constructs `run_rules_enforcer` directly it will need the new argument — pass `Arc::new(crate::screentime::ScreentimeLog::disabled())`.

- [ ] **Step 9: Windows cross-check**

Run the cross-check command from Global Constraints.
Expected: exit 0.

- [ ] **Step 10: Commit**

```bash
git add src/rules.rs src/server.rs
git commit -m "Write the daily rollup to screentime.jsonl, with per-app minutes

Snapshots per_app_secs before decide(), for the same reason prev_total is
snapshotted there: a rollover inside accrue() clears the map, and the row
describes the day that just ended.

Writes a second, richer row to the dedicated store while leaving the existing
usage.jsonl event untouched, so the dashboard's event table is unchanged. Sub-
minute apps are dropped — noise in a daily report.

Logging only. Nothing reads these rows back into the control path."
```

---

### Task 3: The pure aggregation (Tier 0 core, H2, I1)

**Files:**
- Modify: `src/screentime.rs`
- Test: `src/screentime.rs` (`mod tests`)

**Interfaces:**
- Consumes: rows shaped as written in Task 2
- Produces: `build_report(rows: &[Value], today: NaiveDate, days: u32) -> Report`, and the public types `Report`, `DayRow`, `AppMinutes` (all `Serialize`). Task 5 serializes `Report` directly.

**Windowing rule:** the series covers the `days` **completed** days ending **yesterday**. Today is excluded because its rollup has not been written yet; today's live figures already appear on the existing card via `/api/usage/today`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `src/screentime.rs`:

```rust
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
        assert_eq!(r.total_mins, 30, "a future-dated row must not inflate the total");
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
    fn over_budget_is_flagged_only_when_a_budget_applies() {
        let over = row("2026-08-16", 200, 180);
        let unlimited = serde_json::json!({
            "event": "screentime_daily", "date": "2026-08-15",
            "minutes_used": 500, "budget": 0, "apps": {}
        });
        let r = build_report(&[over, unlimited], d("2026-08-17"), 2);
        assert!(!r.days[0].over_budget, "budget 0 means no limit, so never over");
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib screentime`
Expected: FAIL — `cannot find function 'build_report' in this scope`.

- [ ] **Step 3: Implement the types and the aggregation**

Add to `src/screentime.rs`, above the `#[cfg(test)]` block. Extend the imports at the top of the file to:

```rust
use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::NaiveDate;
use serde::Serialize;
use serde_json::Value;

use crate::jsonl::JsonlLog;
```

Then:

```rust
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
fn window_total(by_date: &BTreeMap<NaiveDate, ParsedRow>, from: NaiveDate, to: NaiveDate) -> (u64, usize) {
    let mut total = 0;
    let mut count = 0;
    for (_, row) in by_date.range(from..=to) {
        total += row.minutes_used;
        count += 1;
    }
    (total, count)
}

/// Build the report for the `days` completed days ending yesterday.
///
/// Pure: every input is a parameter, so the whole thing is unit-testable without touching disk or
/// a clock. The `date` field is treated as untrusted — it originates from a wall clock the child
/// may be able to move — so rows dated today or later are ignored and duplicate dates collapse
/// deterministically.
pub fn build_report(rows: &[Value], today: NaiveDate, days: u32) -> Report {
    let days = days.max(1);

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
            Some(row) => DayRow {
                date: cursor.to_string(),
                measured: true,
                minutes_used: Some(row.minutes_used),
                budget: row.budget,
                over_budget: row
                    .budget
                    .is_some_and(|b| b > 0 && row.minutes_used > u64::from(b)),
                apps: row.apps.clone(),
            },
            None => DayRow {
                date: cursor.to_string(),
                measured: false,
                minutes_used: None,
                budget: None,
                over_budget: false,
                apps: Vec::new(),
            },
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
        Some(prev) if prev_measured > 0 && prev > 0 => {
            Some((i64::try_from(total_mins).unwrap_or(i64::MAX) - i64::try_from(prev).unwrap_or(i64::MAX)) * 100 / i64::try_from(prev).unwrap_or(1))
        }
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib screentime`
Expected: PASS — 11 passed (2 from Task 1 + 9 new).

- [ ] **Step 5: Mutation-verify the test that matters most**

Temporarily change the `None =>` arm of the day loop so an unmeasured day reports zero instead:

```rust
                minutes_used: Some(0),
```

Run: `cargo test --lib a_gap_is_null_and_a_real_zero_is_zero`
Expected: **FAIL** — `assertion failed: left: Some(0), right: None`. This proves the test actually pins the distinction rather than passing by construction.

Revert the change (restore `minutes_used: None,`) and re-run:
Expected: PASS.

- [ ] **Step 6: Full gate**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test --all-targets`
Expected: clippy exit 0; all pass.

- [ ] **Step 7: Commit**

```bash
git add src/screentime.rs
git commit -m "Aggregate daily rollups into a windowed report

Pure function over loaded rows: every input is a parameter, so the logic is
unit-testable without disk or a clock — the same shape today_summary was
refactored into.

Treats the date field as untrusted, because it comes from a wall clock the
child may be able to move: rows dated today or later are ignored and duplicate
dates collapse to the richer row deterministically.

The distinction that matters: a day with no row reports null, not zero. No row
means the service never ticked — PC off, or the enforcer stopped while the
machine was used — which is not the same as a day it watched and saw nothing.
Averages count measured days only. Verified by mutation: reporting Some(0) for
a gap fails the test."
```

---

### Task 4: Read history from both logs, including the rotated backup (H4, Tier 1.5)

**Files:**
- Modify: `src/jsonl.rs` (additive method), `src/screentime.rs`
- Test: `src/jsonl.rs`, `src/screentime.rs`

**Interfaces:**
- Consumes: `JsonlLog::recent`, `ScreentimeLog::recent`, `UsageLog::recent`
- Produces: `JsonlLog::recent_including_rotated(usize) -> Vec<Value>`; `ScreentimeLog::recent_including_rotated(usize)`; `screentime::history_rows(&ScreentimeLog, &UsageLog) -> Vec<Value>`

`recent()` is deliberately left untouched so `/api/audit` and `/api/usage` return exactly what they do today.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `src/jsonl.rs`:

```rust
    #[test]
    fn recent_including_rotated_reaches_past_the_backup_boundary() {
        let dir = std::env::temp_dir().join(format!("nw-jsonl-rot-{}", std::process::id()));
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
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib recent_including_rotated`
Expected: FAIL — `no method named 'recent_including_rotated'`.

- [ ] **Step 3: Implement it**

In `src/jsonl.rs`, first factor the parsing out of `recent` so both readers share it. Replace the body of `recent` with a call to a new helper and add the new method:

```rust
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

    /// The most recent `limit` events, newest first. Malformed lines are skipped; a missing
    /// file (nothing logged yet) yields an empty list.
    pub fn recent(&self, limit: usize) -> Vec<Value> {
        let Some(path) = &self.path else {
            return Vec::new();
        };
        let mut events = Self::read_events(path);
        let start = events.len().saturating_sub(limit);
        let mut recent = events.split_off(start);
        recent.reverse();
        recent
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
        let mut events = Self::read_events(&path.with_extension("jsonl.1"));
        events.extend(Self::read_events(path));
        let start = events.len().saturating_sub(limit);
        let mut recent = events.split_off(start);
        recent.reverse();
        recent
    }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --lib jsonl`
Expected: PASS — the new test plus the existing `jsonl` tests.

- [ ] **Step 5: Add the merged reader**

Add to `src/screentime.rs`, in the `impl ScreentimeLog` block:

```rust
    /// The most recent `limit` rows, newest first, including the rotated backup.
    pub fn recent_including_rotated(&self, limit: usize) -> Vec<Value> {
        self.0.recent_including_rotated(limit)
    }
```

And as a free function in the same file, above `build_report`:

```rust
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
            .recent(usize::MAX)
            .into_iter()
            .filter(|v| v.get("event").and_then(Value::as_str) == Some("screentime_daily")),
    );
    rows
}
```

- [ ] **Step 6: Test the merge**

Add to `mod tests` in `src/screentime.rs`:

```rust
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
        // Noise in the usage log must not reach the report.
        us.record("session_start", serde_json::json!({}));

        let rows = history_rows(&st, &us);
        let r = build_report(&rows, d("2026-08-17"), 2);

        assert_eq!(r.measured_days, 2, "both logs contribute");
        assert_eq!(r.total_mins, 150);

        let _ = std::fs::remove_file(&st_path);
        let _ = std::fs::remove_file(&us_path);
    }
```

Run: `cargo test --lib screentime`
Expected: PASS.

- [ ] **Step 7: Test the property H1 exists for**

This is the test that justifies the separate file. It rotates the usage log and asserts the rollup
history survives — the eviction a child can otherwise cause deliberately by cycling lock/unlock.

Add to `mod tests` in `src/screentime.rs`:

```rust
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
```

Run: `cargo test --lib rotating_the_usage_log`
Expected: PASS.

- [ ] **Step 8: Mutation-verify it**

Temporarily point the store at the usage log's path so both share one file, which is the design H1
rejects. In the test, change:

```rust
        let st = ScreentimeLog::new(st_path.clone());
```

to:

```rust
        let st = ScreentimeLog::new(us_path.clone());
```

Run: `cargo test --lib rotating_the_usage_log`
Expected: **FAIL** — `measured_days` is 0, because the rollup was rotated away with everything else. That failure is the whole argument for the separate file, demonstrated rather than asserted.

Restore `st_path` and re-run:
Expected: PASS.

- [ ] **Step 9: Full gate and commit**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test --all-targets`
Expected: all pass.

```bash
git add src/jsonl.rs src/screentime.rs
git commit -m "Read rollup history from both logs and past the rotation boundary

recent() only ever read the live file, so after a rotation up to 2 MiB of
history sat on disk unreachable. Fine for the audit table, which wants the
latest events; wrong for a report whose purpose is looking backwards. Added as
a separate method so /api/audit and /api/usage behave exactly as before.

Also merges legacy screentime_daily rows out of usage.jsonl, so installs
predating the dedicated store do not show an empty chart on upgrade — which
would read as 'he used nothing'."
```

---

### Task 5: The endpoint (Tier 0, H3)

**Files:**
- Modify: `src/api.rs`, `src/server.rs`
- Test: `tests/api.rs`

**Interfaces:**
- Consumes: `screentime::history_rows`, `screentime::build_report`, `AppState.screentime`, `AppState.usage`
- Produces: `GET /api/screentime?days=N` → `Json<Report>`, behind `require_auth`

- [ ] **Step 1: Write the failing test**

Add to `tests/api.rs`. **Use the existing `common` helpers** — `get(app, uri, cookie)` (`tests/common/mod.rs:68`) and `body_json(res)` (`:118`) already do the request-building and body-parsing, and both are already imported at the top of `tests/api.rs`. Do not hand-roll `Request::builder()` / `to_bytes` here; the house pattern is these helpers.

```rust
#[tokio::test]
async fn screentime_requires_auth_and_defaults_to_thirty_days() {
    let app = test_app();

    // Unauthenticated must not reach it.
    let res = get(&app, "/api/screentime", None).await;
    assert_ne!(
        res.status(),
        StatusCode::OK,
        "screen-time history must sit behind require_auth"
    );

    let cookie = login(&app, PASSWORD).await.expect("login");
    let res = get(&app, "/api/screentime", Some(&cookie)).await;
    assert_eq!(res.status(), StatusCode::OK);

    let body = body_json(res).await;
    assert_eq!(
        body["days"].as_array().unwrap().len(),
        30,
        "default window is 30 days"
    );
}

#[tokio::test]
async fn screentime_days_is_clamped() {
    let app = test_app();
    let cookie = login(&app, PASSWORD).await.expect("login");

    for (requested, expected) in [("0", 1usize), ("9999", 365usize)] {
        let uri = format!("/api/screentime?days={requested}");
        let res = get(&app, &uri, Some(&cookie)).await;
        assert_eq!(res.status(), StatusCode::OK);

        let body = body_json(res).await;
        assert_eq!(
            body["days"].as_array().unwrap().len(),
            expected,
            "days={requested} must clamp to {expected}"
        );
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test api screentime`
Expected: FAIL — the unauth assertion may pass incidentally (404 ≠ 200), but the authenticated request returns 404, so `assert_eq!(res.status(), StatusCode::OK)` fails.

- [ ] **Step 3: Add the handler**

In `src/api.rs`, extend the axum extract import to include `Query`:

```rust
use axum::extract::{ConnectInfo, Path, Query, State};
```

Add the handler next to the existing `usage` handler:

```rust
/// Query for [`screentime`]. `days` is optional so `/api/screentime` alone is valid.
#[derive(Deserialize)]
pub struct ScreentimeQuery {
    days: Option<u32>,
}

/// `GET /api/screentime?days=N` → the daily screen-time report: one entry per day for the last
/// `N` completed days, newest last, plus totals and a comparison against the preceding window.
///
/// `days` defaults to 30 — matching what commercial screen-time tools retain, and erring toward
/// keeping less of a child's data — and is clamped to 1..=365 so one request cannot ask for
/// unbounded work. Read-only; behind `require_auth`.
pub async fn screentime(
    State(state): State<AppState>,
    Query(q): Query<ScreentimeQuery>,
) -> Result<Json<crate::screentime::Report>, AppError> {
    let days = q.days.unwrap_or(30).clamp(1, 365);
    let today = crate::config::today();
    let screentime = state.screentime.clone();
    let usage = state.usage.clone();
    let report = spawn(move || {
        let rows = crate::screentime::history_rows(&screentime, &usage);
        crate::screentime::build_report(&rows, today, days)
    })
    .await?;
    Ok(Json(report))
}
```

- [ ] **Step 4: Register the route**

In `src/server.rs`, add to the `/api` router beside the existing usage routes:

```rust
        .route("/screentime", get(api::screentime))
```

Also extend the module doc comment's route list in `src/server.rs` (the `//!` block listing `/api/*`) with:

```rust
//!     GET  /api/screentime
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --test api screentime`
Expected: PASS — 2 passed.

- [ ] **Step 6: Confirm the origin and auth layers cover it automatically**

Run: `cargo test --test origin`
Expected: PASS. The origin check is a global `middleware::from_fn` applied outside the router (`server.rs`), so a new route inherits it with no extra wiring; this run confirms nothing regressed.

- [ ] **Step 7: Full gate, cross-check, commit**

Run: `cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test --all-targets`, then the Windows cross-check from Global Constraints.
Expected: all pass, cross-check exit 0.

```bash
git add src/api.rs src/server.rs tests/api.rs
git commit -m "Serve the screen-time report at GET /api/screentime

Read-only, behind require_auth, and inheriting the global origin check without
extra wiring. days defaults to 30 — what commercial screen-time tools retain,
and the direction that keeps less of a child's data — clamped to 1..=365 so one
request cannot ask for unbounded work."
```

---

### Task 6: The dashboard card (I1, I2, I3)

**Files:**
- Modify: `assets/index.html`

**Interfaces:**
- Consumes: `GET /api/screentime` → `Report`; existing Alpine state `today` (from `/api/usage/today`, which already carries `enforcer_age_secs`)
- Produces: no downstream consumers

**Design notes.** The chart reuses DaisyUI's existing semantic colours — the same `progress-error` / `progress-primary` pairing already used by the per-app bars — so no new palette is introduced and there is no categorical ramp to validate. Not-measured days use a **hatch texture**, not a colour, so the state is never conveyed by colour alone; each bar carries a `<title>` for hover and the day list beneath doubles as the text alternative.

- [ ] **Step 1: Add the Alpine state**

In `assets/index.html`, find the state object containing `usage: [],` (~line 739) and add beside it:

```javascript
          screentime: { days: [], total_mins: 0, measured_days: 0, daily_avg_mins: null, prev_total_mins: null, change_pct: null },
          loadingScreentime: false,
```

- [ ] **Step 2: Add the loader**

Beside the existing `loadUsage()` method (~line 1173), add:

```javascript
          loadScreentime() { return this.loadList("/api/screentime", "screentime", "loadingScreentime", "Failed to load screen-time report"); },
```

And call it where `this.loadUsage();` is called on initial load (~line 771), adding directly beneath:

```javascript
            this.loadScreentime();
```

- [ ] **Step 3: Add the formatting helpers**

Beside `usageDetail(e)` (~line 1244), add:

```javascript
          // Bar height as a percentage of the tallest measured day, floored so a small non-zero
          // day is still visible rather than rounding away to nothing.
          stBarPct(d) {
            const peak = Math.max(1, ...this.screentime.days.map(x => x.minutes_used ?? 0));
            if (d.minutes_used == null) return 100;      // the hatch fills the column
            if (d.minutes_used === 0) return 0;
            return Math.max(4, Math.round((d.minutes_used / peak) * 100));
          },

          stBarTitle(d) {
            if (d.minutes_used == null) return `${d.date}: not measured — the service was not running`;
            const b = d.budget ? ` of ${d.budget} min budget` : "";
            return `${d.date}: ${d.minutes_used} min${b}${d.over_budget ? " (over budget)" : ""}`;
          },

          stChangeLabel() {
            const c = this.screentime.change_pct;
            if (c == null) return "no earlier period to compare";
            const dir = c > 0 ? "▲" : c < 0 ? "▼" : "—";
            return `${dir} ${Math.abs(c)}% vs the previous period`;
          },

          // The enforcer heartbeat, already served by /api/usage/today. A stale or absent value
          // means the figures below may be missing days rather than showing light use.
          stEnforcementStale() {
            const age = this.today?.enforcer_age_secs;
            return age == null || age > 300;
          },
```

- [ ] **Step 4: Add the card markup**

Insert this `<section>` immediately **before** the `<!-- Usage history -->` section (~line 607):

```html
          <!-- Screen-time report -->
          <section class="card bg-base-100 shadow md:col-span-2">
            <div class="card-body">
              <div class="flex items-center justify-between">
                <h2 class="card-title text-base">Screen time</h2>
                <button class="btn btn-ghost btn-sm" @click="loadScreentime()" :disabled="loadingScreentime">
                  <span x-show="loadingScreentime" class="loading loading-spinner loading-xs"></span>
                  Refresh
                </button>
              </div>

              <template x-if="stEnforcementStale()">
                <div role="status" class="alert alert-warning mt-2 py-2 text-sm">
                  <span>⚠ Enforcement may not have been running — days below can be missing rather than quiet.</span>
                </div>
              </template>

              <div class="mt-2 flex flex-wrap gap-4 text-sm">
                <div><span class="opacity-70">Total</span>
                  <span class="ml-1 font-semibold" x-text="`${screentime.total_mins} min`"></span></div>
                <div><span class="opacity-70">Daily average</span>
                  <span class="ml-1 font-semibold"
                        x-text="screentime.daily_avg_mins == null ? '—' : `${screentime.daily_avg_mins} min`"></span></div>
                <div><span class="opacity-70">Measured days</span>
                  <span class="ml-1 font-semibold"
                        x-text="`${screentime.measured_days}/${screentime.days.length}`"></span></div>
                <div class="opacity-70" x-text="stChangeLabel()"></div>
              </div>

              <svg class="mt-3 h-28 w-full" role="img"
                   aria-label="Daily screen-time minutes; hatched columns are days that were not measured"
                   preserveAspectRatio="none">
                <defs>
                  <pattern id="st-nodata" width="6" height="6" patternUnits="userSpaceOnUse"
                           patternTransform="rotate(45)">
                    <rect width="6" height="6" class="fill-base-200"></rect>
                    <line x1="0" y1="0" x2="0" y2="6" class="stroke-base-content" stroke-opacity="0.35"
                          stroke-width="2"></line>
                  </pattern>
                </defs>
                <template x-for="(d, i) in screentime.days" :key="d.date">
                  <g>
                    <title x-text="stBarTitle(d)"></title>
                    <rect :x="`${(i / screentime.days.length) * 100}%`"
                          :width="`${(1 / screentime.days.length) * 100 - 0.4}%`"
                          :y="`${100 - stBarPct(d)}%`"
                          :height="`${stBarPct(d)}%`"
                          rx="2"
                          :fill="d.minutes_used == null ? 'url(#st-nodata)' : null"
                          :class="d.minutes_used == null ? '' : (d.over_budget ? 'fill-error' : 'fill-primary')"></rect>
                  </g>
                </template>
              </svg>

              <div class="mt-1 flex flex-wrap items-center gap-3 text-xs opacity-70">
                <span><span class="inline-block h-2 w-3 align-middle bg-primary"></span> within budget</span>
                <span><span class="inline-block h-2 w-3 align-middle bg-error"></span> over budget</span>
                <span><span class="inline-block h-2 w-3 align-middle bg-base-200 border border-base-content/40"></span> not measured</span>
              </div>

              <template x-if="screentime.days.some(d => d.apps && d.apps.length)">
                <div class="mt-3">
                  <span class="label-text">Most recent measured day</span>
                  <div class="mt-1 flex flex-col gap-1">
                    <template x-for="a in (screentime.days.filter(d => d.apps && d.apps.length).slice(-1)[0]?.apps || [])"
                              :key="a.name">
                      <div class="flex items-center gap-2 text-sm">
                        <span class="w-48 truncate" x-text="a.name"></span>
                        <span class="opacity-70"><span x-text="a.minutes"></span> min</span>
                      </div>
                    </template>
                  </div>
                </div>
              </template>

              <p class="mt-3 text-xs opacity-60">
                Counts time this PC was unlocked with an app running — not time spent looking at it,
                and not per-account. Any account signed in at this machine adds to the same total,
                and a minimised app still counts, so these figures are not comparable to phone
                Screen Time.
              </p>
            </div>
          </section>
```

- [ ] **Step 5: Verify it renders**

Run (per `README.md`; the install step is needed once to create a data dir and password):

```bash
NESTWATCH_PASSWORD=devpassword cargo run -- install
cargo run -- run        # https://localhost:8443
```

(`MIN_PASSWORD_LEN` is 10 — `src/auth.rs:27` — so the README's shorter example is rejected by
`install`.)

Sign in with `devpassword` and confirm:
- the card appears above "Usage history"
- with no history yet, all 30 columns are hatched and "Measured days" reads `0/30`
- hovering a column shows the tooltip text

If the app cannot be run locally, at minimum run `cargo test --all-targets` (the asset is embedded via `rust-embed`, so a malformed file still compiles) and note that visual confirmation is outstanding.

- [ ] **Step 6: Commit**

```bash
git add assets/index.html
git commit -m "Add the screen-time card to the dashboard

Not-measured days are drawn as a hatch, never as a zero-height or zero-value
bar: a day with no row means the service never ticked, and rendering that as 0
would make a dead enforcer look exactly like a well-behaved child. The state is
carried by texture rather than colour alone, each column has a hover title, and
the day list beneath is the text alternative.

Reuses the DaisyUI semantic colours already used by the per-app bars, so no new
palette is introduced. Surfaces the enforcer heartbeat that /api/usage/today
already returns, so the chart cannot quietly disagree with the health signal
beside it, and states plainly that the figures are machine-wide, count running
rather than focused time, and are not comparable to phone Screen Time."
```

---

### Task 7: Documentation

**Files:**
- Modify: `docs/WINDOWS-TESTING.md`, `CHANGELOG.md`, `docs/OPEN-FINDINGS.md`

- [ ] **Step 1: Add the on-device checks**

In `docs/WINDOWS-TESTING.md`, add to section **D. Core features**:

```markdown
- [ ] **Screen-time report (new).** After the PC has been through at least one midnight with the
      service running, the Screen time card shows a bar for that day and "Measured days" counts it.
      Days the PC was off must appear **hatched** ("not measured"), never as a zero bar — that
      distinction is the difference between "he didn't use it" and "we weren't watching".
- [ ] **Per-app rows are plausible.** The most-recent-measured-day list should roughly match what
      he actually ran. Remember it counts apps that are *running*, not focused, so a launcher left
      open all evening will look large — that is expected, not a bug.
```

- [ ] **Step 2: Add the changelog entry**

In `CHANGELOG.md`, under `## [Unreleased]`, add:

```markdown
### Added
- **Screen time report.** The dashboard now charts daily screen-time for the last 30 days, with a
  per-app breakdown for apps that have limits, totals, and a comparison against the previous
  period. Most of this history was already being recorded — it simply had no way to be read back
  more than a few days.

  Days the service wasn't running show as **not measured** rather than as zero, so a stopped
  enforcer can't be mistaken for a quiet week. The figures count time the PC was unlocked with an
  app running — not focused attention, and not per-account — which the card states, because it
  makes them different from the numbers a phone reports.
```

- [ ] **Step 3: Update the open findings**

In `docs/OPEN-FINDINGS.md`, add to the **Open** section:

```markdown
### O6 · Screen-time figures are machine-wide and count running, not focused, time

The report added in the screen-time work counts any account at the console, and counts an app while
its process runs rather than while it has focus. Both are conservative for enforcement and
misleading for a report; both are labelled on the card rather than silently accepted.

**Per-account attribution is cheap and should come first.** `session.rs` already fetches the
console session's `WTSINFOEXW` Level-1 payload and reads `level1.UserName[0]` to detect the
sign-in screen — the username is already in a validated buffer and discarded. Recording it on the
daily rollup is one string per day, no new FFI and no new `unsafe`.

**Foreground accuracy is not cheap.** Microsoft disabled Interactive Service Detection in Windows 10
build 1803, so a session-0 service cannot reach user-session windows at all; it would need a helper
resident in the child's session, well beyond the existing on-demand screenshot helper.
```

- [ ] **Step 4: Commit**

```bash
git add docs/WINDOWS-TESTING.md CHANGELOG.md docs/OPEN-FINDINGS.md
git commit -m "Docs: record the screen-time report and what it does not measure

Adds the on-device checks (a not-measured day must render hatched, never as a
zero bar), the changelog entry, and O6 — which records that per-account
attribution is cheap and should come first, because the username is already
fetched and discarded, while foreground accuracy is blocked by session 0
isolation."
```

---

## Final verification

- [ ] `cargo fmt --all -- --check` → clean
- [ ] `cargo clippy --all-targets -- -D warnings` → exit 0
- [ ] `cargo test --all-targets` → all pass (expect ~176: 162 existing + ~14 new)
- [ ] Windows cross-check (Global Constraints) → exit 0
- [ ] `git status --short` → empty
- [ ] `git diff <base> --stat -- Cargo.toml` → no dependency changes (base = the commit this work started from)
- [ ] Confirm no enforcement path changed:
      `git diff <base> -- src/rules.rs | grep -E '^\+' | grep -E 'accrue|decide|Targets|per_app_secs\s*\+='`
      → only the snapshot clone and the rollup write should appear; **no change to accrual or decisions**
