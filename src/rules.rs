//! Usage rules: a daily screen-time budget, an app blocklist (kill-on-sight), and per-app
//! daily time limits — enforced by a background loop alongside the curfew enforcer.
//!
//! Split like `curfew`: the [`RulesEnforcer::decide`] state machine is pure (it takes the
//! process list + an injected clock and returns [`RuleAction`]s), so it is exhaustively
//! unit-testable; [`run_rules_enforcer`] is the only part that reads the clock, persists the
//! running tally, and calls the OS.
//!
//! Interaction with curfew: both enforcers may independently request lock/shutdown — that's
//! safe because those ops are idempotent. `abort_shutdown` is not idempotent in the same way,
//! because there is a single OS pending-shutdown slot and either enforcer's abort would cancel
//! the other's countdown. Curfew is therefore **authoritative**: it aborts whenever it leaves its
//! window, without checking anything. This enforcer may abort only on the falling edge of "a
//! budget shutdown is wanted", and only while curfew is inactive.
//!
//! That rule lives in [`should_abort_budget_shutdown`] and its caller — read those, not this
//! paragraph, before changing anything about aborts. An earlier version of this comment claimed
//! only curfew ever aborts; the rules-side abort landed three days later and the claim went stale
//! for a month, which is why the invariant now points at the code that enforces it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use chrono::{Datelike, NaiveDate, Weekday};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{Config, Language};
use crate::control::{ControlError, RunningProcess, SystemControl};
use crate::countdown::Countdown;
use crate::curfew::{MAX_WARN_SECS, default_warn_secs};

/// How often the enforcer re-checks (matches the curfew enforcer).
const CHECK_INTERVAL: Duration = Duration::from_secs(30);

/// Largest minute value accepted for any limit — a week, so generous per-weekday budgets fit
/// while absurd values are refused with a message rather than silently clamped.
pub const MAX_BUDGET_MINS: u32 = 7 * 24 * 60;
/// Largest number of entries in the blocklist / per-app limits / app groups. Bounds the config
/// file and the per-tick matching work.
pub const MAX_RULE_ENTRIES: usize = 200;

/// What to do when the daily budget is exhausted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EnforceAction {
    /// Lock the screen (re-locking each tick while over budget). The gentle default.
    #[default]
    Lock,
    /// Power off with a warning countdown, like curfew.
    Shutdown,
    /// Record only — no enforcement (soft rollout / observation).
    Warn,
}

/// Serde/`Default` value for [`Rules::enabled`] — enforcement is on unless explicitly paused.
/// A free fn (not `Default`) so a legacy `config.json` with no `enabled` field upgrades to
/// *enabled*, never silently paused.
fn default_true() -> bool {
    true
}

/// A named set of apps that share one daily time pool.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppGroup {
    /// Display/label name, also the key the running tally is stored under.
    pub name: String,
    /// Member process names (case-insensitive), e.g. `["minecraft.exe", "roblox.exe"]`.
    #[serde(default)]
    pub apps: Vec<String>,
    /// Shared daily budget for the whole group, in minutes (`0` = no limit).
    #[serde(default)]
    pub limit_mins: u32,
}

impl AppGroup {
    /// Whether this group can actually enforce anything — a limit *and* someone to apply it to.
    ///
    /// One definition, because three places need the same answer ([`Rules::has_targets`],
    /// [`Targets::from_rules`], and `today_summary`) and a comment used to be the only thing
    /// keeping them equal. It wasn't enough once already: see the note on [`Rules::has_targets`].
    pub fn has_pool(&self) -> bool {
        // `any(non-blank)`, not `!is_empty()`. A group holding only blank rows has members in the
        // `Vec` sense and none in the sense that matters, and the difference was a live kill: the
        // blank normalises to the empty string, which matches any process whose name trims away.
        self.limit_mins > 0 && self.apps.iter().any(|a| !a.trim().is_empty())
    }
}

/// Persisted rule settings (a `Config` field). All defaulted so legacy configs still load.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rules {
    /// Master switch: when `false`, the whole rules enforcer is paused (no budget, blocklist, or
    /// per-app limits) — a one-toggle "free evening". Curfew is separate and still applies.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Minutes of allowed use per day (`0` = no budget). The everyday default, used for all days
    /// unless `budget_by_weekday` overrides it.
    #[serde(default)]
    pub daily_budget_mins: u32,
    /// Optional per-weekday budgets `[Mon, Tue, Wed, Thu, Fri, Sat, Sun]` (minutes; `0` = no limit
    /// that day). When `Some`, it's authoritative and `daily_budget_mins` is ignored; when `None`
    /// (the default, and how legacy configs load), every day uses `daily_budget_mins`. A `Vec`
    /// (not a fixed `[u32; 7]`) on purpose: a wrong-length array in a hand-edited config then falls
    /// back gracefully per day instead of failing to parse and bricking service startup.
    #[serde(default)]
    pub budget_by_weekday: Option<Vec<u32>>,
    /// Process names killed on sight (case-insensitive, e.g. `"game.exe"`).
    ///
    /// Blank rows are dropped on the way in ([`de_blocklist`]), so this vector only ever holds
    /// strings that could name a process.
    #[serde(default, deserialize_with = "de_blocklist")]
    pub blocklist: Vec<String>,
    /// Per-app daily minute limits, keyed by process name.
    #[serde(default)]
    pub app_limits: BTreeMap<String, u32>,
    /// App groups with a **shared** daily pool: e.g. all games share 90 min. Time accrues to the
    /// group whenever any member is running; when the pool is spent, every running member is
    /// killed on sight (like a per-app limit, but pooled across the group).
    #[serde(default)]
    pub app_groups: Vec<AppGroup>,
    /// Grace/warning countdown before the budget action fires.
    #[serde(default = "default_warn_secs")]
    pub warn_secs: u32,
    /// What to do when the daily budget is spent.
    #[serde(default)]
    pub budget_action: EnforceAction,
}

/// Drop blocklist rows that cannot name a process.
///
/// An empty row is what a text input yields before anyone types in it, and it is not nothing: it
/// normalises to the empty string, and `norm` is `trim` + `to_lowercase`, so it would match any
/// process whose name trims away — a kill-on-sight rule nobody wrote. [`Targets::from_rules`]
/// filters at the point of use, which is the guard that actually protects (a `Rules` built in code
/// never passes through serde); this keeps such a row from being *stored* at all.
///
/// Deliberately lenient rather than an error. A parent who adds a row and saves before filling it
/// in has made no mistake worth a red banner, and `validate` rejecting the payload would lose the
/// rest of their edit along with it.
fn de_blocklist<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<String>::deserialize(deserializer)?;
    Ok(raw.into_iter().filter(|b| !b.trim().is_empty()).collect())
}

impl Default for Rules {
    /// A default `Rules` is **enabled** with nothing configured. Hand-written (rather than
    /// derived) so `enabled` defaults to `true`, not `bool`'s `false`.
    ///
    /// `warn_secs` must use [`default_warn_secs`] — the same value serde applies to a config
    /// that omits the field. Hardcoding 0 here meant a *fresh install* locked the child's screen
    /// with no warning at all, while an upgraded config warned for 60s.
    fn default() -> Self {
        Self {
            enabled: true,
            daily_budget_mins: 0,
            budget_by_weekday: None,
            blocklist: Vec::new(),
            app_limits: BTreeMap::new(),
            app_groups: Vec::new(),
            warn_secs: default_warn_secs(),
            budget_action: EnforceAction::Lock,
        }
    }
}

/// What [`run_rules_enforcer`] should do with one tick, from [`Rules::tick_mode`].
///
/// Three states, because the household that has configured nothing is not the household that
/// pressed **Pause**, and treating them alike is what made a fresh install report a confident
/// zero for a day nobody measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickMode {
    /// The parent pressed **Pause**. Measure nothing and enforce nothing.
    ///
    /// Measuring through a pause would be a change to what this product promises, not a bug fix:
    /// Pause is the one control that means "stop watching him", and a parent who presses it is
    /// entitled to a gap in the record rather than a quieter kind of surveillance.
    StandDown,
    /// Enabled, but nothing is configured yet. Count screen time; take no action.
    ///
    /// The process list is not read on this path. It exists only to match blocklist entries,
    /// per-app limits and group members, and by definition there are none — so the scan
    /// `has_targets` was introduced to avoid is still avoided, while the accrual it was
    /// accidentally suppressing now happens.
    Measure,
    /// Enabled with something to enforce: the full path, process scan included.
    Enforce,
}

impl Rules {
    /// The base daily budget (minutes, before any granted extra) for `weekday`: the per-weekday
    /// override if set, else the everyday `daily_budget_mins`. One home for the day-selection
    /// rule so the enforcer, its logging, and the dashboard summary can't drift.
    pub fn base_budget_for(&self, weekday: Weekday) -> u32 {
        match &self.budget_by_weekday {
            // `.get` (not index) so a short/malformed vec falls back per day rather than panicking.
            Some(days) => days
                .get(weekday.num_days_from_monday() as usize)
                .copied()
                .unwrap_or(self.daily_budget_mins),
            None => self.daily_budget_mins,
        }
    }

    /// Whether any day has a budget at all (used to decide if the enforcer has work to do).
    fn has_any_budget(&self) -> bool {
        match &self.budget_by_weekday {
            Some(days) => days.iter().any(|&m| m > 0),
            None => self.daily_budget_mins > 0,
        }
    }

    /// Whether anything is configured that could *actually* enforce, ignoring the pause toggle.
    ///
    /// **Derived from [`Targets::from_rules`] rather than restated.** This used to be four hand-
    /// written predicates that had to agree with it, and a comment saying they "must match
    /// exactly". They did not, twice, and neither drift was visible from either side:
    ///
    /// * A non-empty `app_limits` counted here whatever the values were, while `Targets` dropped
    ///   the zeroes — so the enforcer woke and scanned the process table every 30s with nothing to
    ///   enforce, and `doctor` reported "rules active" for rules that could never fire. Fixed by
    ///   adding a fourth predicate, which is the shape of fix that invites the next drift.
    /// * The reverse, later: a blank-named per-app limit and a group whose only member was blank
    ///   were discounted by neither side, so both reached the enforcing path and were *measured*
    ///   killing a process whose name trimmed to nothing. A blocklist of blank rows was discounted
    ///   here and not by `decide`, which is the same bug in the other direction.
    ///
    /// Asking `Targets` costs a few small allocations once per tick, against a process-table scan
    /// it decides whether to perform. That is not a trade worth a second source of truth.
    pub fn has_targets(&self) -> bool {
        self.has_any_budget() || !Targets::from_rules(self).is_empty()
    }

    /// What this tick should do about these rules.
    ///
    /// Pure, and beside [`has_targets`](Self::has_targets) rather than inline in the loop, for the
    /// reason [`should_abort_budget_shutdown`] is: it is a rule the loop applies, and a rule in
    /// that loop is worth pinning without standing an enforcer up.
    ///
    /// It replaced a single `any_configured()` (`enabled && has_targets()`) that collapsed three
    /// states into two. Both false answers stood the whole tick down — and standing down does not
    /// merely skip enforcement, it skips *measurement*: every accrual site in
    /// [`run_rules_enforcer`] sits below that branch's `continue`. So a fresh install, which is
    /// `enabled` with nothing configured, counted no screen time at all, while the dashboard card,
    /// its source comment and `doctor` each told the parent it was "counting screen time" /
    /// "tracking only". Three statements of a thing that was not happening, and nothing to catch
    /// them, because a household that has set no rules also has no expected number to compare
    /// against.
    pub fn tick_mode(&self) -> TickMode {
        match (self.enabled, self.has_targets()) {
            (false, _) => TickMode::StandDown,
            (true, false) => TickMode::Measure,
            (true, true) => TickMode::Enforce,
        }
    }

    /// Validate (at config load and on POST). Fail-open like curfew: only the warning bound.
    pub fn validate(&self) -> Result<(), String> {
        if self.warn_secs > MAX_WARN_SECS {
            return Err(format!("warning seconds must be <= {MAX_WARN_SECS}"));
        }
        // Bound every minute value and collection length. The arithmetic is saturating, so these
        // aren't load-bearing for safety — they exist so nonsense is rejected at the door with a
        // message, instead of being stored and quietly clamped later. A day is 1440 minutes; the
        // cap allows generous per-weekday values without admitting absurd ones.
        if self.daily_budget_mins > MAX_BUDGET_MINS {
            return Err(format!("daily limit must be <= {MAX_BUDGET_MINS} minutes"));
        }
        if let Some(days) = &self.budget_by_weekday
            && days.iter().any(|&m| m > MAX_BUDGET_MINS)
        {
            return Err(format!(
                "each day's limit must be <= {MAX_BUDGET_MINS} minutes"
            ));
        }
        if self.app_limits.values().any(|&m| m > MAX_BUDGET_MINS)
            || self
                .app_groups
                .iter()
                .any(|g| g.limit_mins > MAX_BUDGET_MINS)
        {
            return Err(format!("app limits must be <= {MAX_BUDGET_MINS} minutes"));
        }
        if self.blocklist.len() > MAX_RULE_ENTRIES
            || self.app_limits.len() > MAX_RULE_ENTRIES
            || self.app_groups.len() > MAX_RULE_ENTRIES
        {
            return Err(format!("at most {MAX_RULE_ENTRIES} entries per list"));
        }
        Ok(())
    }

    /// The effective budget in minutes for `today`: that day's base budget (per-weekday override
    /// or the everyday default) plus any granted extra, or `0` when the day has **no** base budget
    /// (unlimited). Returning 0 in that case — rather than `extra` — keeps the dashboard card and
    /// the enforcer in agreement: granted extra on an unlimited day must not display a phantom
    /// budget the enforcer never applies. The single home for the "budget today" value so
    /// `decide`, its logging, and the summary can't drift.
    pub fn effective_budget_mins(&self, today: NaiveDate, extra: u32) -> u32 {
        let base = self.base_budget_for(today.weekday());
        // Saturating: release builds don't check overflow, so a large `daily_budget_mins` in a
        // hand-edited config plus a granted minute would WRAP to a near-zero budget — enforcement
        // silently inverted, with no error anywhere. Same reasoning for every accumulator below.
        if base > 0 {
            base.saturating_add(extra)
        } else {
            0
        }
    }
}

/// The running daily tally, persisted to a sidecar so a mid-day reboot doesn't reset the budget.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    /// The local date these totals belong to; a change resets them.
    pub day: Option<NaiveDate>,
    /// Seconds of use accrued today.
    pub total_secs: u64,
    /// Per-app seconds today (only for apps that have a limit), keyed by normalized name.
    pub per_app_secs: BTreeMap<String, u64>,
    /// Per-group seconds today (only for groups with a limit), keyed by group name.
    #[serde(default)]
    pub per_group_secs: BTreeMap<String, u64>,
    /// Per-app seconds **with focus** today, for every app seen — not just limited ones.
    ///
    /// Report-only, and deliberately so: it is fed by a watcher running as the child, which makes
    /// every number here attacker-chosen. Nothing in [`RulesEnforcer::decide`] may read it. See
    /// `docs/FOREGROUND-TRACKING.md` and `foreground_time_cannot_trigger_a_per_app_limit`.
    ///
    /// `#[serde(default)]` so a tally written before this field existed still parses — otherwise
    /// `load_or_default` would swallow the error and hand the child a zeroed budget on upgrade day.
    #[serde(default)]
    pub foreground_secs: BTreeMap<String, u64>,
    /// Seconds per **browser page title** today. Report-only, like `foreground_secs`, and bounded
    /// to [`crate::foreground::MAX_PAGES`] entries because the keys are attacker-chosen strings
    /// rather than a fixed set of installed programs.
    #[serde(default)]
    pub page_secs: BTreeMap<String, u64>,
}

/// Rule-derived, normalized enforcement targets for one tick — built once by `decide` and shared
/// by accrual and the kill checks so the two can't disagree on what's tracked.
#[derive(Default)]
pub(crate) struct Targets {
    /// Process names killed on sight, normalized (blank rows dropped).
    blocked: BTreeSet<String>,
    /// Per-app limits (minutes), keyed by normalized process name (zero-limit and blank-named
    /// apps dropped).
    app_limits: BTreeMap<String, u32>,
    /// App groups with a shared pool: (name, normalized member set, limit minutes). Only groups
    /// with a positive limit and at least one member that could name a process are included.
    groups: Vec<(String, BTreeSet<String>, u32)>,
}

/// Normalize process names and drop the ones that cannot name a process.
///
/// `norm` is `trim` + `to_lowercase`, so a blank row becomes the empty string — and the empty
/// string is a perfectly good key, matching any process whose own name trims away. Every
/// collection that is matched against a process list goes through here, so the rule is stated
/// once instead of being remembered three times.
fn matchable(names: impl IntoIterator<Item = impl AsRef<str>>) -> BTreeSet<String> {
    names
        .into_iter()
        .map(|n| norm(n.as_ref()))
        .filter(|n| !n.is_empty())
        .collect()
}

impl Targets {
    fn from_rules(rules: &Rules) -> Self {
        let blocked = matchable(&rules.blocklist);
        let app_limits = rules
            .app_limits
            .iter()
            .filter(|(k, v)| **v > 0 && !k.trim().is_empty())
            .map(|(k, &v)| (norm(k), v))
            .collect();
        let groups = rules
            .app_groups
            .iter()
            .filter(|g| g.has_pool())
            .map(|g| (g.name.clone(), matchable(&g.apps), g.limit_mins))
            .collect();
        Self {
            blocked,
            app_limits,
            groups,
        }
    }

    /// Whether anything here could match a running process. The single definition of "is this
    /// household configured", so [`Rules::has_targets`] cannot drift from what `decide` acts on.
    fn is_empty(&self) -> bool {
        self.blocked.is_empty() && self.app_limits.is_empty() && self.groups.is_empty()
    }
}

impl Usage {
    /// Add `delta_secs` to the total, to each tracked app running, and to each group with a
    /// member running — resetting first if the local day changed. `running` and all `targets`
    /// keys are already normalized. Pure.
    pub(crate) fn accrue(
        &mut self,
        today: NaiveDate,
        delta_secs: u64,
        running: &BTreeSet<String>,
        targets: &Targets,
    ) {
        if self.day != Some(today) {
            self.day = Some(today);
            self.total_secs = 0;
            self.per_app_secs.clear();
            self.per_group_secs.clear();
            self.page_secs.clear();
            // Cleared with the rest: the day's focus figures belong to the day that just ended,
            // and `decide_after_snapshot` has already taken the copy the rollup row is built from.
            self.foreground_secs.clear();
        }
        self.total_secs = self.total_secs.saturating_add(delta_secs);
        for name in running {
            if targets.app_limits.contains_key(name) {
                let slot = self.per_app_secs.entry(name.clone()).or_insert(0);
                *slot = slot.saturating_add(delta_secs);
            }
        }
        for (gname, members, _limit) in &targets.groups {
            if running.iter().any(|r| members.contains(r)) {
                let slot = self.per_group_secs.entry(gname.clone()).or_insert(0);
                *slot = slot.saturating_add(delta_secs);
            }
        }
    }

    fn load_or_default(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Load the persisted tally for read-only display of *today's* usage (the enforcer owns the
    /// live copy). When the stored tally belongs to an earlier day it's treated as empty — but
    /// stamped with `today` — so the dashboard never shows yesterday's numbers before the first
    /// tick of the new day has run.
    pub fn load_for_today(today: NaiveDate) -> Self {
        let stored = Self::load_or_default(&usage_state_path());
        if stored.day == Some(today) {
            stored
        } else {
            Self {
                day: Some(today),
                ..Default::default()
            }
        }
    }

    /// Minutes left against `budget`, or `None` when no budget applies today.
    ///
    /// The one place this subtraction happens, because it has a sharp edge: `used_mins` is `u64`
    /// and `as u32` on a corrupt or absurd tally wraps to a small number, which reads as "plenty
    /// of budget left" — enforcement inverted, silently. Clamp, never truncate.
    ///
    /// Deliberately **not** the same arithmetic as the enforcer's countdown, which floors over
    /// *seconds* to understate (see `decide`). This one is for display; a budget of 10 with 61s
    /// used reads 9 here and 8 there, and both are right for what they do.
    pub fn remaining_mins(&self, budget: u32) -> Option<u32> {
        let used = u32::try_from(self.total_secs / 60).unwrap_or(u32::MAX);
        (budget > 0).then(|| budget.saturating_sub(used))
    }

    /// This tally as JSON, for [`save_tally_if_changed`] to compare and persist.
    fn to_json(&self) -> Option<String> {
        serde_json::to_string(self)
            .inspect_err(|e| tracing::warn!(error = %e, "usage tally serialize failed"))
            .ok()
    }
}

/// Path to the persisted daily-tally sidecar. One home so the enforcer (writer) and the
/// read-only "today's usage" endpoint (reader) can't disagree on the location.
pub(crate) fn usage_state_path() -> std::path::PathBuf {
    crate::config::data_paths().dir.join("usage_state.json")
}

/// Build the read-only "today's usage" summary served at `GET /api/usage/today`: minutes
/// used/remaining against today's effective budget, plus per-app usage for apps that have a
/// limit. The handler supplies the config snapshot, the loaded tally, and the enforcer
/// heartbeat. `remaining_mins` is `null` when no budget is set.
///
/// Genuinely pure, which it previously only claimed to be: it used to call
/// [`crate::heartbeat::worst_age_secs`] itself, reading two process-global atomics and the
/// system clock. The visible symptom was that none of the tests below asserted on
/// `enforcer_age_secs` — the one field they could not pin without becoming order-dependent on
/// whatever else in the test binary had touched those globals. Taking it as an argument moves
/// the "is enforcement alive" read out to the edge, where the rest of the I/O already lives.
///
/// `enforcer_age_secs`: seconds since either background loop last reached a tick, or `None` if
/// neither has ever reported — which, once the service has been up more than a tick, is itself
/// the alarm.
pub fn today_summary(
    rules: &Rules,
    today: NaiveDate,
    extra: u32,
    usage: &Usage,
    enforcer_age_secs: Option<i64>,
) -> serde_json::Value {
    let budget = rules.effective_budget_mins(today, extra);
    let used_mins = usage.total_secs / 60;
    let remaining_mins = usage.remaining_mins(budget);
    let per_app: Vec<serde_json::Value> = rules
        // The same two conditions `Targets::from_rules` applies, because this card is a claim
        // about what is being enforced. A zero limit is off; a blank-named one can never match a
        // process, so listing either would show a parent a limit that does nothing. The group
        // filter below needs no equivalent — `has_pool` is shared with `Targets` and already
        // answers this.
        .app_limits
        .iter()
        .filter(|(name, v)| **v > 0 && !name.trim().is_empty())
        .map(|(name, &lim)| {
            let used = usage.per_app_secs.get(&norm(name)).copied().unwrap_or(0) / 60;
            serde_json::json!({ "name": name, "used_mins": used, "limit_mins": lim })
        })
        .collect();
    let groups: Vec<serde_json::Value> = rules
        .app_groups
        .iter()
        .filter(|g| g.has_pool())
        .map(|g| {
            let used = usage.per_group_secs.get(&g.name).copied().unwrap_or(0) / 60;
            serde_json::json!({ "name": g.name, "used_mins": used, "limit_mins": g.limit_mins })
        })
        .collect();
    serde_json::json!({
        "day": usage.day.map(|d| d.to_string()),
        // Seconds since an enforcer last reached a tick. The dashboard turns a large value into
        // "enforcement may not be running" — the only signal that distinguishes a dead enforcer
        // from a quiet day, since both otherwise show zero minutes used.
        "enforcer_age_secs": enforcer_age_secs,
        "enabled": rules.enabled,
        "budget_mins": budget,
        "used_mins": used_mins,
        "remaining_mins": remaining_mins,
        "extra_mins": extra,
        "per_app": per_app,
        "groups": groups,
        // Today's focus figures, which until now were measured every thirty seconds, written to
        // `usage_state.json`, and shown to nobody until the next day's rollup. Report-only, like
        // everywhere else they appear: the watcher runs as the child, so nothing in `decide` may
        // read them (`foreground_time_cannot_trigger_a_per_app_limit`).
        //
        // Unlike `per_app` above, this is not filtered to apps that already have a limit — the
        // question it answers is "what has he actually been doing", which is precisely about the
        // apps nobody thought to configure.
        "focused": top_by_minutes(&usage.foreground_secs, TOP_TODAY),
        "pages": top_by_minutes(&usage.page_secs, TOP_TODAY),
        // Distinguishes "he focused nothing" from "nothing was watching" — the same distinction
        // `DayRow::measured` draws for completed days, which this card had no equivalent of. An
        // empty list on its own cannot tell them apart, and rendering silence as zero is the
        // failure this codebase has already fixed twice.
        "focus_missing": usage.total_secs >= FOCUS_EVIDENCE_SECS
            && usage.foreground_secs.is_empty(),
    })
}

/// The per-tick clock/context injected into [`RulesEnforcer::decide`] — keeps that function
/// pure (no real clock) and exhaustively testable.
pub struct Tick {
    /// Monotonic "now" (for deadline math).
    pub now: Instant,
    /// Local calendar day (for the daily reset).
    pub today: NaiveDate,
    /// How much time this tick represents (added to the usage tally).
    pub interval: Duration,
    /// Grace/warning countdown before the budget action fires.
    pub warn: Duration,
    /// Extra slack past the shutdown deadline before re-issuing (defeats `shutdown /a`).
    pub slack: Duration,
    /// Extra minutes granted to today's budget (0 if none / not for today).
    pub extra_minutes: u32,
    /// Whether an interactive user is actively using the machine this tick (session unlocked).
    /// When `false` (nobody logged in, or the screen is locked) the budget neither accrues nor
    /// enforces — so a PC left on overnight doesn't burn the day's budget, and a budget lock
    /// isn't re-issued every tick while the screen is already locked.
    pub active: bool,
}

/// An action the enforcer decided on for this tick.
///
/// There is deliberately **no `Abort` variant**: cancelling a pending shutdown is a falling-edge
/// decision about the whole episode, not something a single tick can decide from its own actions.
/// It lives in [`maybe_abort_budget_shutdown`], which is also where the coordination with curfew
/// over the single OS pending-shutdown slot is enforced. Don't add one here.
#[derive(Debug, PartialEq, Eq)]
pub enum RuleAction {
    /// Terminate this PID (blocklisted, or an app over its per-app limit).
    Kill(u32),
    /// Lock the screen (budget spent, action = Lock).
    LockScreen,
    /// Issue the first, warned shutdown of this over-budget episode (action = Shutdown).
    Shutdown,
    /// Re-issue a shutdown **immediately**, with no countdown — the previous one was cancelled.
    /// A standard user holds `SeShutdownPrivilege` and can run `shutdown /a`; re-issuing with the
    /// same warning just handed them another window to cancel. Zero delay leaves nothing pending
    /// to abort. Mirrors `curfew::Action::ShutdownNow`.
    ShutdownNow,
    /// Budget spent, action = Warn — record only, no OS action.
    Warn,
    /// The budget is spent and the grace period just began — tell the child the screen is about
    /// to lock. Only the `Lock` action arms a grace period, so this can only arise there.
    ///
    /// Emitted by the state machine rather than reconstructed by the loop. The loop used to
    /// snapshot the private `budget_deadline` before `decide` and diff it afterwards to recover
    /// this exact transition — which meant the loop couldn't move out of this file, and a fourth
    /// `budget_action` that armed a deadline would have silently inherited the notification.
    LockWarning,
    /// Advance heads-up: this many minutes of budget remain (one of
    /// [`crate::countdown::WARN_AT_MINS`]).
    /// Fires *before* the budget is spent, independent of `budget_action` — a Shutdown-mode day
    /// gets the same countdown as a Lock-mode one, since Windows' own dialog only appears at zero.
    TimeWarning(u32),
}

/// The day's tally as it stood *before* a tick ran — the numbers a rollover row describes.
///
/// Produced only by [`RulesEnforcer::decide_after_snapshot`], which is the point: these values are
/// unrecoverable once `decide` has run, so there is no way to ask for them too late.
#[derive(Default)]
pub struct PreRollover {
    pub day: Option<NaiveDate>,
    pub total_secs: u64,
    pub per_app_secs: BTreeMap<String, u64>,
    pub foreground_secs: BTreeMap<String, u64>,
    pub page_secs: BTreeMap<String, u64>,
    /// Per-group seconds, keyed by the group's display name.
    ///
    /// Groups were the one tally reported for *today* and never written to history, so a parent
    /// could see "Games: 40 min" this afternoon and never "Games: 14 h this month" — which is the
    /// view every comparable product leads with, and the one that turns thirty rows of executable
    /// names into a sentence.
    pub per_group_secs: BTreeMap<String, u64>,
}

/// Deadline-based budget state machine (mirrors `curfew::Enforcer`), plus the running tally.
pub struct RulesEnforcer {
    pub usage: Usage,
    /// When set, the budget is over and this is the grace deadline (Lock) or the expected
    /// shutdown-completion time (Shutdown, for re-issue detection). `None` = under budget.
    budget_deadline: Option<Instant>,
    /// Monotonic time of the last day rollover we honoured. Defence in depth behind
    /// [`crate::clock`]: a rollover wipes the whole day's tally, so however wrong the wall clock
    /// gets, we refuse to do it twice within [`MIN_RESET_GAP`]. `None` until the first rollover,
    /// so a legitimate one right after startup is never blocked.
    last_reset: Option<Instant>,
    /// Whether the child has already been warned during the current over-budget episode. Cleared
    /// only when they go back *under* budget — crucially not when they go inactive, which is what
    /// made Win+L a bypass. See the `Lock` arm of [`RulesEnforcer::decide`].
    episode_warned: bool,
    /// Advance warnings ("15 minutes left"), announced on the way down. See [`crate::countdown`].
    countdown: Countdown,
}

/// Minimum monotonic time between two honoured day rollovers. A real day is 24h; 12h leaves room
/// for DST and a mid-day restart while still making a reset loop useless.
const MIN_RESET_GAP: Duration = Duration::from_secs(12 * 3600);

impl RulesEnforcer {
    /// The date to account against this tick.
    ///
    /// Normally just `today`. But a rollover resets the entire tally, so if the date changes again
    /// within [`MIN_RESET_GAP`] of the last one we keep using the previous day — the tally stands
    /// and the child gains nothing. Two independent defences (this and the anchored clock) because
    /// the failure mode is unlimited screen time and it was reachable in four clicks.
    fn accounting_day(&mut self, today: NaiveDate, now: Instant) -> NaiveDate {
        let Some(current) = self.usage.day else {
            return today; // first tick after start: adopt whatever the clock says
        };
        if current == today {
            return today;
        }
        match self.last_reset {
            Some(last) if now.duration_since(last) < MIN_RESET_GAP => {
                tracing::warn!(
                    "refusing a second day rollover within 12h (clock says {today}, tally is for \
                     {current}) — keeping today's screen-time tally"
                );
                current
            }
            _ => {
                self.last_reset = Some(now);
                today
            }
        }
    }

    fn new(usage: Usage) -> Self {
        Self {
            usage,
            budget_deadline: None,
            last_reset: None,
            episode_warned: false,
            countdown: Countdown::default(),
        }
    }

    /// Decide this tick's actions. Pure: accrues into `self.usage`, updates `budget_deadline`,
    /// and returns the actions — no I/O, no real clock. `now`/`today` are injected.
    /// Take the pre-rollover snapshot and run [`decide`](Self::decide) in one call.
    ///
    /// The two are fused **so the snapshot cannot drift below the decision**. `decide` calls
    /// `accrue`, which clears the day's tally and per-app map on a rollover — and the rollover is
    /// exactly the tick whose row we are about to write. A snapshot taken afterwards would report
    /// zero minutes and `apps:{}` for every completed day, forever, with no error raised and every
    /// existing test still green. Three loose `let`s above the call site held that ordering by
    /// convention; this holds it by shape, the same way [`crate::heartbeat::tick`] welds its stamp
    /// to its await.
    pub fn decide_after_snapshot(
        &mut self,
        rules: &Rules,
        procs: &[RunningProcess],
        t: Tick,
    ) -> (PreRollover, Vec<RuleAction>) {
        let prev = PreRollover {
            day: self.usage.day,
            total_secs: self.usage.total_secs,
            per_app_secs: self.usage.per_app_secs.clone(),
            foreground_secs: self.usage.foreground_secs.clone(),
            page_secs: self.usage.page_secs.clone(),
            per_group_secs: self.usage.per_group_secs.clone(),
        };
        (prev, self.decide(rules, procs, t))
    }

    /// Fold one watcher report into the day's **report-only** tallies.
    ///
    /// Lives here, beside [`decide`](Self::decide), rather than inline in the enforcer loop,
    /// because it carries a three-part ordering invariant that was previously stated only in
    /// prose — bound before accruing, accrue before re-capping — and every part of it could be
    /// broken with the whole suite green. It is pure, so the tests below drive it directly.
    ///
    /// **Call this after [`decide`](Self::decide), never before.** `decide` clears the day's maps
    /// on a rollover and `decide_after_snapshot` has already copied the outgoing day's figures for
    /// the row about to be written; folding in first would add this tick's seconds to yesterday
    /// and then watch them be wiped.
    ///
    /// Nothing here is read by any enforcement path. The watcher runs as the child, so these
    /// numbers are the child's to influence — see `foreground_time_cannot_trigger_a_per_app_limit`.
    pub fn record_foreground(&mut self, sample: crate::foreground::Sample, elapsed: Duration) {
        // Bound first: these figures are attacker-chosen, and only one window holds focus at a
        // time, so neither map can legitimately exceed the interval that just elapsed.
        let bounded = crate::foreground::clamp(sample, elapsed.as_secs());

        // Key app names through `norm` at this boundary rather than trusting the watcher to have
        // done it. `norm` is the one definition of how a process name is keyed, and the dashboard
        // renders `apps` and `focused` side by side on it — if the two disagree, one app silently
        // becomes two rows. Colliding keys merge rather than overwrite.
        // Blank names dropped, for the reason `Targets::from_rules` drops them: `norm` is `trim` +
        // `to_lowercase`, so a name that is only whitespace becomes the empty string, and the
        // empty string is a key like any other. It cannot kill anything here — nothing in
        // `decide` reads these maps — but it would take a row in the report, and
        // `top_by_minutes` ranks by time, so a forged blank with hours against it heads the list
        // of "what has he actually been doing" while naming nothing. The seconds are discarded
        // rather than merged elsewhere: we do not know what app they belonged to.
        let apps = bounded
            .apps
            .into_iter()
            .filter_map(|(name, secs)| {
                let key = norm(&name);
                (!key.is_empty()).then_some((key, secs))
            })
            .fold(BTreeMap::<String, u64>::new(), |mut acc, (key, secs)| {
                let slot = acc.entry(key).or_insert(0);
                *slot = slot.saturating_add(secs);
                acc
            });
        // Capped as it accrues, not afterwards — `accrue_capped` is the only way in, so the count
        // bound cannot be left off here the way it originally was. These maps are persisted every
        // tick and folded into the daily rollup, so a fresh set of names each tick is growth on
        // disk. Both maps, not just the titles: `apps` reads like the safe one because its keys are
        // executables, but that holds only while the watcher is honest, and this module's premise is
        // that it is not. `MAX_APPS` sits far above any real machine, so honest use never meets it.
        crate::foreground::accrue_capped(
            &mut self.usage.foreground_secs,
            apps,
            crate::foreground::MAX_APPS,
        );

        // Page titles are deliberately **not** normalised: they are display text shown back to the
        // parent, not keys matched against a rule.
        crate::foreground::accrue_capped(
            &mut self.usage.page_secs,
            bounded.pages,
            crate::foreground::MAX_PAGES,
        );
    }

    pub fn decide(&mut self, rules: &Rules, procs: &[RunningProcess], t: Tick) -> Vec<RuleAction> {
        let mut actions = Vec::new();

        let targets = Targets::from_rules(rules);
        let running: BTreeSet<String> = procs.iter().map(|p| norm(&p.name)).collect();

        // Only charge screen time while the machine is actively in use. A locked screen or a
        // logged-out console still resets on a new day (accrue handles the rollover), but adds
        // no seconds — so overnight idle time and the budget-lock's own locked screen don't
        // count against the budget.
        let delta = if t.active { t.interval.as_secs() } else { 0 };
        self.usage.accrue(t.today, delta, &running, &targets);

        // Members of any group whose shared pool is spent → killed on sight.
        let mut group_over: BTreeSet<String> = BTreeSet::new();
        for (name, members, limit) in &targets.groups {
            if self.usage.per_group_secs.get(name).copied().unwrap_or(0) >= *limit as u64 * 60 {
                group_over.extend(members.iter().cloned());
            }
        }

        // Blocklist (kill on sight) + per-app over-limit + over-pool group members → kill.
        // From `Targets`, not from `rules`, so the blocklist passes the same filter the limits and
        // groups do. Computing it here was how it came to miss one: `norm` is `trim` +
        // `to_lowercase`, so a blank row became the empty string, which matches any process whose
        // name trims to nothing — a kill-on-sight rule nobody wrote, on the path that terminates
        // processes. Filtered in `Targets` rather than at validation because a hand-edited
        // `config.json` reaches `decide` without passing `validate` at all.
        let blocked = &targets.blocked;
        for p in procs {
            let n = norm(&p.name);
            if blocked.contains(&n) {
                actions.push(RuleAction::Kill(p.pid));
                continue;
            }
            if let Some(&lim) = targets.app_limits.get(&n)
                && self.usage.per_app_secs.get(&n).copied().unwrap_or(0) >= lim as u64 * 60
            {
                actions.push(RuleAction::Kill(p.pid));
                continue;
            }
            if group_over.contains(&n) {
                actions.push(RuleAction::Kill(p.pid));
            }
        }

        // Total daily budget with warn-then-act. Enforced only while the machine is actively in
        // use: when inactive we disarm below, so a user who steps away (or is locked out by the
        // budget itself) isn't shut down/re-locked in absentia, and gets a fresh warning grace
        // when they return.
        let budget_mins = rules.effective_budget_mins(t.today, t.extra_minutes);
        if budget_mins > 0 && t.active {
            let budget_secs = budget_mins as u64 * 60;
            if self.usage.total_secs >= budget_secs {
                // Spent — the at-zero notification takes over, so stand the countdown down rather
                // than let it announce "1 minute left" alongside "locking in 60 seconds".
                self.countdown.reset();
                match rules.budget_action {
                    EnforceAction::Warn => {
                        self.budget_deadline = None;
                        actions.push(RuleAction::Warn);
                    }
                    EnforceAction::Lock => match self.budget_deadline {
                        // Already warned during this over-budget episode: lock on the first
                        // active tick, no second grace. Otherwise Win+L was a complete bypass —
                        // the deadline is dropped on any inactive tick (below), so locking the
                        // screen for 30s and unlocking bought another full `warn_secs` of use,
                        // repeatable forever, and the child was never involuntarily locked.
                        None if self.episode_warned => actions.push(RuleAction::LockScreen),
                        None => {
                            self.episode_warned = true;
                            self.budget_deadline = Some(t.now + t.warn);
                            actions.push(RuleAction::LockWarning);
                        }
                        Some(dl) if t.now >= dl => actions.push(RuleAction::LockScreen),
                        Some(_) => {}
                    },
                    EnforceAction::Shutdown => match self.budget_deadline {
                        None => {
                            self.budget_deadline = Some(t.now + t.warn);
                            actions.push(RuleAction::Shutdown);
                        }
                        Some(dl) if t.now >= dl + t.slack => {
                            // Still here long after it should have powered off — cancelled.
                            self.budget_deadline = Some(t.now + t.warn);
                            actions.push(RuleAction::ShutdownNow);
                        }
                        Some(_) => {}
                    },
                }
            } else {
                // Still under budget: announce the remaining-time thresholds as they're crossed,
                // so the lock isn't the first the child hears of it. Floor division deliberately
                // *understates* — "5 minutes left" then means at least five, never four and a
                // half, so the machine never takes back time the popup just promised.
                let remaining_mins = ((budget_secs - self.usage.total_secs) / 60) as u32;
                if let Some(mins) = self.countdown.observe(remaining_mins) {
                    actions.push(RuleAction::TimeWarning(mins));
                }
                // Back under budget (a grant, a code, or the daily reset) — this episode is over,
                // so the next one earns a fresh warning.
                self.budget_deadline = None;
                self.episode_warned = false;
            }
        } else {
            // An unlimited day, or inactive (away, locked, or signed out). Never act in absentia —
            // but deliberately do NOT clear `episode_warned`, or stepping away would launder the
            // warning.
            self.budget_deadline = None;
            // An unlimited day has nothing to count down to. Re-prime, so switching a budget back
            // on mid-day can't announce a threshold measured against the *old* budget's position.
            // (An inactive tick on a limited day deliberately leaves the countdown alone: nothing
            // accrues while away, so the child returns to exactly the remaining time they left.)
            if budget_mins == 0 {
                self.countdown.reset();
            }
        }

        actions
    }
}

/// Normalize a process name for matching: trimmed + lowercased (`"Chrome.exe"` == `"chrome.exe"`).
pub(crate) fn norm(name: &str) -> String {
    name.trim().to_lowercase()
}

/// How many apps and page titles today's live panel carries.
///
/// A card, not a report: the daily rollup keeps everything, and this is the "what has he been on
/// this afternoon" glance. Ten rows is more than a parent reads standing in a kitchen, and it bounds
/// a payload polled every sixty seconds by every open dashboard.
const TOP_TODAY: usize = 10;

/// How much accrued use makes "no focus data at all" evidence of a dead watcher rather than a quiet
/// morning.
///
/// The two are genuinely indistinguishable at small totals. A machine that has been in use for five
/// minutes with a running watcher has certainly had *some* window in front for at least one second,
/// and the tally stores seconds — so an empty map past this threshold means nobody reported. Below
/// it, the honest answer is that we do not know yet, and this stays `false`.
///
/// Deliberately generous. The failure this guards against is telling a parent their watcher is
/// broken when it isn't, which spends the credibility of every other warning on this page.
const FOCUS_EVIDENCE_SECS: u64 = 300;

/// The `n` heaviest entries as `{name, minutes}`, heaviest first, sub-minute entries dropped.
///
/// Ordered output, so it is a JSON array and not an object: a `BTreeMap` serialises sorted by
/// *name*, and the whole point here is sorted by *time*. Ties break on name so a refresh does not
/// reshuffle equal rows under the parent's eyes.
///
/// Not `foreground::retain_top`, which is private and rightly so — that one bounds what gets
/// *stored*, and routing a display projection through it would blur a memory-safety cap with a
/// presentation choice.
fn top_by_minutes(secs: &BTreeMap<String, u64>, n: usize) -> Vec<Value> {
    let mut rows: Vec<(&String, u64)> = secs
        .iter()
        .filter_map(|(name, s)| {
            let mins = s / 60;
            (mins > 0).then_some((name, mins))
        })
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    rows.truncate(n);
    rows.into_iter()
        .map(|(name, mins)| serde_json::json!({ "name": name, "minutes": mins }))
        .collect()
}

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

/// Build the payload for one completed day's rollup row. Shared by both the `usage_log` and
/// `screentime_log` writes in [`run_rules_enforcer`] so the two stores can't drift apart again —
/// and so the shape is unit-testable without the async loop around it.
///
/// `budget: None` **omits** the `budget` key entirely rather than substituting a guess.
/// `parse_row` (`screentime.rs`) treats a missing key as "unknown", which renders as a bare
/// minute count with no over/under verdict. This matters on the first tick after any restart,
/// when there is no carried-forward budget for the day that just ended: guessing *today's*
/// budget used to write a verdict about a day it was never true for — e.g. a Sunday with a
/// 300-minute budget, rolled over into a Monday with 120, rendered as Sunday "over budget"
/// though the child never came close. Unknown budget, no claim — never a wrong claim.
fn rollup_row(prev: &PreRollover, date: NaiveDate, budget: Option<u32>) -> Value {
    let mut row = serde_json::Map::new();
    row.insert("date".into(), Value::from(date.to_string()));
    row.insert("minutes_used".into(), Value::from(prev.total_secs / 60));
    if let Some(b) = budget {
        row.insert("budget".into(), Value::from(b));
    }
    row.insert(
        "apps".into(),
        Value::Object(per_app_minutes(&prev.per_app_secs)),
    );
    // A second map rather than a richer `apps` value: a row written by an older build has no
    // `focused` key at all, and `screentime::parse_row` reads its absence as "not measured"
    // rather than as zero focus. Nesting both under `apps` would have made every historical row
    // parse as though the child stared at nothing.
    row.insert(
        "focused".into(),
        Value::Object(per_app_minutes(&prev.foreground_secs)),
    );
    // Browser page titles, same shape and same absent-means-unknown rule as `focused`. Sub-minute
    // entries are dropped by `per_app_minutes`, which matters more here than anywhere else: a day
    // of browsing touches hundreds of titles for a few seconds each, and none of them is a fact
    // worth storing for a year.
    row.insert(
        "pages".into(),
        Value::Object(per_app_minutes(&prev.page_secs)),
    );
    // Group totals, keyed by the group's display name rather than a process name — the only map
    // here whose keys the parent chose. Same absent-means-unknown rule as the two above: a row
    // written before this existed has no `groups` key, and `parse_row` reads that as "not
    // recorded" rather than as a day with no category time.
    //
    // Written even when empty, so a day where groups *were* configured and genuinely unused is
    // distinguishable from one where the concept did not exist — `per_app_minutes` drops
    // sub-minute entries, so an empty object here means "configured and under a minute", and an
    // absent key means "this build did not record groups".
    row.insert(
        "groups".into(),
        Value::Object(per_app_minutes(&prev.per_group_secs)),
    );
    Value::Object(row)
}

/// Background loop: every [`CHECK_INTERVAL`], enforce the usage rules. Runs for the life of the
/// server; if it ever returns, the caller logs that loudly.
pub async fn run_rules_enforcer(
    control: Arc<dyn SystemControl>,
    config: Arc<RwLock<Config>>,
    usage_log: Arc<crate::usage::UsageLog>,
    screentime_log: Arc<crate::screentime::ScreentimeLog>,
    foreground: crate::foreground::Feed,
    mut wake: crate::heartbeat::Wake,
) {
    let tally_path = usage_state_path();
    let mut enforcer = RulesEnforcer::new(Usage::load_or_default(&tally_path));
    let mut locking = false; // is a budget lock currently in effect? (for transition logging)
    let mut shutting = false;
    let mut warning = false;
    let mut prev_active: Option<bool> = None; // last tick's active-session state (for session_* events)
    let mut prev_shutdown_wanted = false; // did we want a budget shutdown last tick? (to cancel it)
    let mut prev_budget: Option<u32> = None; // effective budget in force at the last tick (for the daily rollup)
    let mut ticker = tokio::time::interval(CHECK_INTERVAL);
    // Default is `Burst`: after a suspend, every missed tick fires back-to-back. An 8-hour sleep
    // would replay ~960 of them, each charging a full interval, and burn the whole day's budget
    // in under a minute — then lock the child out for reasons they can't see. `Delay` drops the
    // backlog and resumes on cadence.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Charge REAL elapsed time, not a hardcoded interval, clamped so one long gap can't be
    // charged as hours of use.
    let mut last_tick = Instant::now();
    // The last tally bytes we know reached disk — see `save_tally_if_changed`.
    let mut last_saved_tally: Option<String> = None;

    loop {
        crate::heartbeat::tick(&mut ticker, crate::heartbeat::Enforcer::Rules, &mut wake).await;

        // Charge the time that actually passed, clamped to twice the interval. A hardcoded
        // CHECK_INTERVAL over-charges after any stall (suspend, CPU starvation, a slow scan);
        // real elapsed time alone would charge an entire sleep as screen time on modern-standby
        // laptops, where the monotonic clock keeps running through S0ix. The clamp means one long
        // gap costs at most one extra tick.
        let now = Instant::now();
        let elapsed = now.duration_since(last_tick).min(CHECK_INTERVAL * 2);
        last_tick = now;

        // Empty the watcher's buffer **every** tick, including ticks that return early below.
        //
        // `Feed::submit` accrues between drains and its doc assumes they are "thirty seconds
        // apart", while `elapsed` above is capped at twice the interval. Draining only on ticks
        // that enforce broke that pairing: a pause of any length piled up in the buffer and then
        // landed clamped to at most 60 s on the tick that resumed, so a paused weekend rendered as
        // about a minute of "time in front" — a fabricated small number where the truth is "not
        // measured", and `MAX_PAGES` had already evicted the tail before the clamp saw it.
        //
        // Held rather than recorded here: `record_foreground` must run *after* `decide`, which
        // clears the day's maps on a rollover (see its doc). So the sample is taken now, to bound
        // what can accumulate, and folded in further down where the ordering is still correct.
        let reported = foreground.drain();

        // `accounting_day` may hold the previous date if the clock just jumped — a rollover
        // wipes the tally, so it needs a monotonic sanity check, not just a trusted clock.
        let today = enforcer.accounting_day(crate::config::today(), now);
        // Snapshot the config under the lock, then drop the guard before any await.
        // `port` and the curfew reading come out of the same guard the tick already takes, rather
        // than a second acquisition later — they feed `ask_hint`, which needs both.
        let (rules, extra, lang, port, curfew_now) = {
            let guard = crate::state::recover_read(&config);
            (
                guard.rules.clone(),
                guard.extra.for_day(today),
                guard.language,
                guard.port,
                guard.curfew.is_active_now(),
            )
        };
        let hint = ask_hint(port, lang, curfew_now);

        let mode = rules.tick_mode();

        if mode == TickMode::StandDown {
            // Only a pause reaches here now. A household that has configured nothing used to land
            // on this branch too, and standing down skips *measurement*, not just enforcement —
            // every accrual site below sits under this `continue`. So the state a fresh install is
            // in was the state in which nothing was counted, while the dashboard called it
            // "tracking only". `tick_mode` splits the two; see [`TickMode::Measure`].
            let reason = "paused";

            // Close an open session before we stop watching it.
            //
            // Without this the stream is unpairable: `prev_active` is discarded below, so the next
            // active tick opens a *second* session with no stop between them. Measured on real
            // data before the fix — six `session_start` and zero `session_stop`.
            //
            // Deliberately `session_stop` rather than a new event name, so that **every start has a
            // matching stop by construction**. A distinct name would leave a future consumer to
            // learn about it, and forgetting would reproduce exactly the orphaning this fixes;
            // `reason` carries the nuance instead. It also keeps the record honest about what
            // ended: enforcement stopped observing, and the child may well still be sitting there.
            //
            // `budget` is omitted rather than guessed. Rules may well be configured on this path
            // now — only a *pause* reaches it — but a paused budget is not one in force, so there
            // is still none to report: the same choice `rollup_row` makes when it cannot know one.
            if prev_active == Some(true) {
                usage_log.record(
                    "session_stop",
                    serde_json::json!({
                        "minutes_used": enforcer.usage.total_secs / 60,
                        "reason": reason,
                    }),
                );
            }

            // Nothing to enforce this tick. But if we had a budget shutdown in flight, cancel it —
            // otherwise pausing (or clearing the budget) mid-countdown would still power the
            // machine off.
            prev_shutdown_wanted = maybe_abort_budget_shutdown(
                &control,
                &config,
                &usage_log,
                prev_shutdown_wanted,
                false,
                serde_json::json!({ "reason": reason }),
            )
            .await;
            prev_active = None; // resume treats the next active tick as a fresh session_start
            continue;
        }

        // Is a user actively at the machine this tick? Best-effort: on a query failure assume
        // active, so a hiccup in the status check never quietly hands out unlimited screen time.
        let active = {
            let control = control.clone();
            match tokio::task::spawn_blocking(move || control.session_state()).await {
                Ok(Ok(state)) => matches!(state, crate::control::SessionState::Active),
                _ => true,
            }
        };

        // Read only when something might match. On `Measure` the list has nothing to be matched
        // against — no blocklist, no per-app limit, no group — so `decide` returns the same empty
        // action set for an empty slice, and skipping the scan keeps the 30-second cost that
        // `has_targets` exists to avoid. Screen time still accrues: `accrue` charges the interval
        // to `total_secs` before it looks at any process.
        let procs = if mode != TickMode::Enforce {
            Vec::new()
        } else {
            let control = control.clone();
            match tokio::task::spawn_blocking(move || control.running_processes()).await {
                Ok(Ok(procs)) => procs,
                // Unchanged on purpose: a failure skips the whole tick rather than falling through
                // with an empty list. An empty list reads as "nothing is running" — no per-app time
                // accrued, nothing killed — which is enforcement silently off for that tick.
                //
                // This drops the tick's foreground sample too, and unlike the stand-down above that
                // is a real loss: the machine *is* in use, we simply could not read the process
                // list. Accepted deliberately rather than by omission. Carrying it to the next tick
                // would put the buffer back outside the "thirty seconds apart" envelope
                // `Feed::submit` is written against, to save at most one interval — and the tick it
                // landed on would then charge two intervals of focus against one of elapsed time,
                // which is the same fabricated number in miniature. One interval unmeasured is the
                // honest answer, and it is the one `total_secs` gives on this path as well.
                _ => continue, // transient list failure; try again next tick
            }
        };

        let (prev, actions) = enforcer.decide_after_snapshot(
            &rules,
            &procs,
            Tick {
                now: Instant::now(),
                today,
                interval: elapsed,
                warn: Duration::from_secs(rules.warn_secs as u64),
                slack: CHECK_INTERVAL,
                extra_minutes: extra,
                active,
            },
        );
        // Fold in what the foreground watcher reported, **after** `decide` — see
        // `record_foreground` for why the order is load-bearing.
        //
        // `None` means no watcher reported during this tick at all: the helper is dead, or nobody
        // is signed in. That leaves the maps untouched rather than writing zeros, so an unmeasured
        // stretch stays unmeasured rather than becoming a confident nothing.
        //
        // Taken at the top of the tick, not here — see the drain for why. A tick that returned
        // early dropped its sample, which is the same answer the rest of this card gives while
        // enforcement is stood down: nothing accrued, because nothing was being counted.
        if let Some(sample) = reported {
            enforcer.record_foreground(sample, elapsed);
        }

        save_tally_if_changed(&enforcer.usage, &tally_path, &mut last_saved_tally).await;

        let budget = rules.effective_budget_mins(today, extra);

        // Log the previous day's total once, on rollover. Report the budget that was in force at
        // the *end of that day* (carried across ticks), not today's — otherwise the fresh day's
        // reset extra-time grant would be misattributed to yesterday's row. On the first tick
        // after a restart we have no carried value: `rollup_row` then omits `budget` rather than
        // guessing today's, which used to write a false "over budget" verdict on a day it never
        // applied to (see `rollup_row`'s doc comment).
        if let Some(pd) = prev.day
            && pd != today
        {
            let row = rollup_row(&prev, pd, prev_budget);
            usage_log.record("screentime_daily", row.clone());
            // The durable copy, in a file noisy events cannot rotate away.
            screentime_log.record(row);
        }
        prev_budget = Some(budget);

        let used_mins = enforcer.usage.total_secs / 60;

        // Record active-use session boundaries in the usage history (rising/falling edge of
        // `active`). The first observed active tick counts as a session start.
        if prev_active != Some(active) {
            if active {
                usage_log.record("session_start", serde_json::json!({}));
            } else if prev_active.is_some() {
                usage_log.record(
                    "session_stop",
                    serde_json::json!({ "minutes_used": used_mins, "budget": budget }),
                );
            }
            prev_active = Some(active);
        }

        // `has_lock` / `has_shutdown` drive what gets written to the usage history, so they must
        // reflect what the OS ACTUALLY did. They used to be set before the call, with the result
        // discarded — so a lock that silently failed (a missing helper binary, a stale console
        // session id, fast-user-switching) still recorded a tidy `budget_lock`, and the parent's
        // dashboard showed enforcement that never happened. Curfew always matched on the result;
        // this loop had drifted.
        let mut has_lock = false;
        let mut has_shutdown = false;
        let mut has_warn = false;
        for action in actions {
            match action {
                RuleAction::Kill(pid) => {
                    let control = control.clone();
                    match tokio::task::spawn_blocking(move || control.kill_process(pid)).await {
                        Ok(Ok(())) => {}
                        // A process that exited between the scan and the kill is routine, not a
                        // failure — don't cry wolf every 30s over a race we expect.
                        Ok(Err(ControlError::ProcessNotFound(_))) => {
                            tracing::debug!(pid, "process was already gone");
                        }
                        Ok(Err(e)) => tracing::warn!(pid, error = %e, "could not kill process"),
                        Err(e) => tracing::error!(pid, error = %e, "kill task panicked"),
                    }
                }
                RuleAction::LockScreen => {
                    let control = control.clone();
                    match tokio::task::spawn_blocking(move || control.lock_workstation()).await {
                        Ok(Ok(())) => has_lock = true,
                        Ok(Err(e)) => {
                            tracing::error!(error = %e, "budget lock FAILED — screen time is not \
                                             being enforced right now")
                        }
                        Err(e) => tracing::error!(error = %e, "budget lock task panicked"),
                    }
                }
                action @ (RuleAction::Shutdown | RuleAction::ShutdownNow) => {
                    // First issue warns; a re-issue means the last one was cancelled, so it goes
                    // immediately (see `RuleAction::ShutdownNow`).
                    let secs = if action == RuleAction::Shutdown {
                        rules.warn_secs
                    } else {
                        tracing::warn!("budget shutdown did not happen (cancelled?) — now");
                        0
                    };
                    let control = control.clone();
                    // Carries the hint for the same reason the lock warning does, and it matters
                    // more here: this is the harsher of the two budget endings, and `/c` is the
                    // *only* text a `shutdown.exe` dialog shows — there is no notification beside
                    // it to carry the address. Without this, a Shutdown-configured install never
                    // told the child where to ask, while a Lock-configured one always did.
                    // Over-long text is truncated at 512 chars by the caller, not rejected, so a
                    // future longer hint degrades the message rather than failing the shutdown.
                    let msg = with_hint(shutdown_message(lang).to_string(), hint.as_deref());
                    match tokio::task::spawn_blocking(move || control.shutdown(secs, Some(msg)))
                        .await
                    {
                        Ok(Ok(())) => has_shutdown = true,
                        Ok(Err(e)) => {
                            tracing::error!(error = %e, "budget shutdown FAILED — screen time is \
                                             not being enforced right now")
                        }
                        Err(e) => tracing::error!(error = %e, "budget shutdown task panicked"),
                    }
                }
                RuleAction::Warn => has_warn = true,
                RuleAction::LockWarning => {
                    notify_child(
                        &control,
                        &with_hint(lock_warning_message(rules.warn_secs, lang), hint.as_deref()),
                        lang,
                    )
                    .await;
                }
                RuleAction::TimeWarning(mins) => {
                    // Record the heads-up only if the OS actually took the message. A countdown
                    // the child never saw must not look, in the history, like one they did.
                    let msg = with_hint(budget_countdown_message(mins, lang), hint.as_deref());
                    if notify_child(&control, &msg, lang).await {
                        usage_log.record(
                            "budget_countdown",
                            serde_json::json!({
                                "minutes_remaining": mins,
                                "minutes_used": used_mins,
                                "budget": budget,
                            }),
                        );
                    }
                }
            }
        }

        // Warn action: notify on the rising edge, so a Warn-mode limit isn't a silent surprise.
        // (The Lock grace notice is `RuleAction::LockWarning`, handled in the loop above; Shutdown
        // already shows Windows' own countdown, so it isn't doubled up.) Checked before the
        // `log_transition` calls below, which flip `warning`.
        if has_warn && !warning {
            let msg = with_hint(limit_reached_message(lang).to_string(), hint.as_deref());
            notify_child(&control, &msg, lang).await;
        }

        // Log budget events once per episode (on the transition into enforcement).
        log_transition(
            &usage_log,
            "budget_lock",
            has_lock,
            &mut locking,
            used_mins,
            budget,
        );
        log_transition(
            &usage_log,
            "budget_shutdown",
            has_shutdown,
            &mut shutting,
            used_mins,
            budget,
        );
        log_transition(
            &usage_log,
            "budget_warn",
            has_warn,
            &mut warning,
            used_mins,
            budget,
        );

        // Cancel a budget shutdown we scheduled once it's no longer warranted — chiefly when the
        // parent grants more time, lifting the child back under budget. The trigger is the budget
        // itself (`shutdown_wanted`), not the countdown deadline: merely locking the screen or
        // stepping away while still over budget must NOT rescue an in-flight shutdown.
        // `budget` (computed above for the rollup) is 0 on an unlimited day, so this also gates
        // out base-0 days without re-deriving anything.
        let over_budget = budget > 0 && enforcer.usage.total_secs >= budget as u64 * 60;
        let shutdown_wanted = over_budget && rules.budget_action == EnforceAction::Shutdown;
        prev_shutdown_wanted = maybe_abort_budget_shutdown(
            &control,
            &config,
            &usage_log,
            prev_shutdown_wanted,
            shutdown_wanted,
            serde_json::json!({ "minutes_used": used_mins, "budget": budget }),
        )
        .await;
    }
}

/// Persist the daily tally, but only when it actually changed, and never on the runtime thread.
///
/// Two things were wrong with saving unconditionally every tick, both measured:
///
/// * **It fsyncs.** `write_atomic` calls `sync_all`, which is a durability barrier, not a byte
///   cost — ~3.6ms whether the tally is 76 bytes or 7.6KB. This was the only OS-touching call in
///   the enforcer loop not on the blocking pool, so it parked a whole tokio worker in an
///   uninterruptible device flush 2,880 times a day.
/// * **Most of those writes were identical.** The tally can only change on an active tick or a day
///   rollover; an inactive tick adds zero to every counter and re-serializes the same bytes. On a
///   machine used 4h of a 12h day that is ~67% of writes, and 83% for a service left up.
///
/// The comparison is on the serialized bytes rather than a `dirty` flag deliberately: `accrue`
/// inserts zero-valued entries for newly-seen apps, so a tick with no elapsed time can still
/// change the JSON. A flag would miss that; a content compare cannot.
///
/// The 30-second cadence itself is **not** negotiable — the child is the adversary and a reboot
/// is their tool. At 30s a reboot forfeits at most half a minute of tally and costs more than that
/// in boot time; at five minutes it becomes "reboot, gain five minutes, repeat".
async fn save_tally_if_changed(
    usage: &Usage,
    path: &std::path::Path,
    last_saved: &mut Option<String>,
) {
    let Some(json) = usage.to_json() else { return };
    if last_saved.as_deref() == Some(json.as_str()) {
        return;
    }
    let (target, bytes) = (path.to_path_buf(), json.clone().into_bytes());
    match tokio::task::spawn_blocking(move || crate::config::write_atomic(&target, &bytes)).await {
        Ok(Ok(())) => *last_saved = Some(json),
        // Leave `last_saved` alone on failure, so the next tick retries rather than assuming the
        // bytes reached disk.
        Ok(Err(e)) => tracing::warn!(error = %e, "usage tally save failed"),
        Err(e) => tracing::error!(error = %e, "usage tally save task panicked"),
    }
}

/// Whether the rules enforcer should cancel a pending OS shutdown *it* previously scheduled.
/// True only on the falling edge of "a budget shutdown is wanted" (e.g. a grant lifted the child
/// back under budget, or the action changed) AND when curfew isn't itself calling for a shutdown
/// — so curfew remains the sole authority over the single OS pending-shutdown slot (the reason
/// [`RuleAction`] has no abort variant). Pure, so the coordination rule is unit-tested.
fn should_abort_budget_shutdown(prev_wanted: bool, now_wanted: bool, curfew_active: bool) -> bool {
    prev_wanted && !now_wanted && !curfew_active
}

/// Cancel a budget shutdown the enforcer previously scheduled when it's no longer wanted this
/// tick (grant lifted the child back under budget, rules paused/cleared, or the action changed)
/// and curfew isn't itself calling for one. Returns `now_wanted` to carry into the next tick.
/// Shared by the normal path and the paused/idle path so the abort behavior lives in one place;
/// curfew is read only on the potential falling edge.
async fn maybe_abort_budget_shutdown(
    control: &Arc<dyn SystemControl>,
    config: &Arc<RwLock<Config>>,
    usage_log: &crate::usage::UsageLog,
    prev_wanted: bool,
    now_wanted: bool,
    detail: serde_json::Value,
) -> bool {
    let curfew_active = if prev_wanted && !now_wanted {
        let guard = crate::state::recover_read(config);
        guard.curfew.enabled && guard.curfew.is_active_now()
    } else {
        false
    };
    if should_abort_budget_shutdown(prev_wanted, now_wanted, curfew_active) {
        let control = control.clone();
        match tokio::task::spawn_blocking(move || control.abort_shutdown()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "failed to abort budget shutdown"),
            // A panicked worker means the abort did NOT happen and the machine will power off
            // anyway. This arm used to be absent, so that outcome was silent.
            Err(e) => tracing::error!(error = %e, "budget shutdown abort task panicked"),
        }
        usage_log.record("budget_shutdown_aborted", detail);
    }
    now_wanted
}

/// Best-effort child-facing notification under the fixed "Screen time" title, so callers only
/// pass the body. Returns whether the OS took the message — see [`crate::control::notify`]; the
/// countdown checks it before recording, the at-zero warnings don't (their history rows already
/// record the enforcement action itself, which is the thing that matters).
async fn notify_child(control: &Arc<dyn SystemControl>, body: &str, lang: Language) -> bool {
    let title = match lang {
        Language::En => "Screen time",
        Language::Nl => "Schermtijd",
    };
    crate::control::notify(control, title, body).await
}

/// "Need more? <url>", or `None` when asking cannot help.
///
/// The child is sitting at the machine, so **`localhost`** is the right address for them: it needs
/// no DHCP lease, no name resolution, and no knowledge of what the PC is called. It is also a SAN
/// on the certificate `install` generates and is admitted by the LAN gate as loopback, so the page
/// actually opens. The hostname and LAN IP that `install` prints are for the *parent's* phone,
/// which is a different problem.
///
/// The port comes from the live config rather than the 8443 default, because `install --port N`
/// moves it and a wrong port here is worse than no address at all.
///
/// **`None` while a curfew window is open**, which is the half worth the sentence. A grant cannot
/// move bedtime — `curfew.rs` never reads `Config::extra` — so printing "ask for more time" to a
/// child whose machine is already shutting down for the night promises something the system cannot
/// deliver, and the parent would then have to be the one to say no. This is the same rule
/// `api::grant_shadowed_by_curfew` applies at the other end of the same conversation.
///
/// Only *while the window is open*, deliberately. If bedtime is twenty minutes out and screen time
/// runs out now, asking is still worth doing — they would get the minutes until bedtime. Silence
/// there would be over-correction.
fn ask_hint(port: u16, lang: Language, curfew_active: bool) -> Option<String> {
    if curfew_active {
        return None;
    }
    Some(match lang {
        Language::En => format!("Need more? https://localhost:{port}/ask"),
        Language::Nl => format!("Meer nodig? https://localhost:{port}/ask"),
    })
}

/// Append the hint on its own line, when there is one.
fn with_hint(body: String, hint: Option<&str>) -> String {
    match hint {
        Some(h) => format!("{body}\n{h}"),
        None => body,
    }
}

/// "The machine is about to lock", with its countdown. Child-facing.
fn lock_warning_message(secs: u32, lang: Language) -> String {
    match lang {
        Language::En => format!("Screen time is up. This computer will lock in {secs} seconds."),
        Language::Nl => {
            format!("Je schermtijd is op. Deze computer gaat over {secs} seconden op slot.")
        }
    }
}

/// "You are out of time", shown once when the budget runs out. Child-facing.
fn limit_reached_message(lang: Language) -> &'static str {
    match lang {
        Language::En => "You've reached today's screen-time limit.",
        Language::Nl => "Je hebt je schermtijd voor vandaag opgebruikt.",
    }
}

/// The reason Windows shows in its own shutdown dialog when the budget runs out. Child-facing.
///
/// A function rather than the literal it replaced, which is the whole point: every other string
/// the child reads is built by one of these and guarded by a test that walks `Language::ALL`, so
/// the one string that had no function was the one string that was never translated. Reusing the
/// exact opening of [`lock_warning_message`] is deliberate — the two are alternative endings to
/// the same sentence, chosen by `budget_action`, and a child who has seen one should recognise
/// the other.
fn shutdown_message(lang: Language) -> &'static str {
    match lang {
        Language::En => "Screen time is up — this computer is shutting down.",
        Language::Nl => "Je schermtijd is op — deze computer wordt afgesloten.",
    }
}

/// What the child is told at each remaining-time threshold. `mins` is one of
/// [`crate::countdown::WARN_AT_MINS`]; the catch-all keeps the wording sane if that list ever
/// changes, and sidesteps pluralising "1 minute" by naming the singular case outright.
fn budget_countdown_message(mins: u32, lang: Language) -> String {
    match (lang, mins) {
        (Language::En, 1) => "1 minute of screen time left!".to_string(),
        (Language::En, 5) => "5 minutes of screen time left — good time to save.".to_string(),
        (Language::En, m) => format!("{m} minutes of screen time left today."),
        // See the note in `curfew::bedtime_message`: the 1/5/rest shape survives into Dutch
        // because minuut/minuten splits where minute/minutes does.
        (Language::Nl, 1) => "Nog 1 minuut schermtijd!".to_string(),
        (Language::Nl, 5) => "Nog 5 minuten schermtijd — sla je werk even op.".to_string(),
        (Language::Nl, m) => format!("Nog {m} minuten schermtijd vandaag."),
    }
}

/// Record an event on the rising edge of `active`, tracked via `state`.
fn log_transition(
    usage_log: &crate::usage::UsageLog,
    event: &str,
    active: bool,
    state: &mut bool,
    used_mins: u64,
    budget: u32,
) {
    if active && !*state {
        usage_log.record(
            event,
            serde_json::json!({ "minutes_used": used_mins, "budget": budget }),
        );
    }
    *state = active;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, name: &str) -> RunningProcess {
        RunningProcess {
            pid,
            name: name.into(),
        }
    }

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 7, 9).unwrap()
    }

    const TICK: Duration = Duration::from_secs(30);
    const SLACK: Duration = Duration::from_secs(30);
    const WARN: Duration = Duration::from_secs(60);

    /// A `Tick` at `now` with `extra` granted minutes and the fixed test day/intervals. Active
    /// (a user is at the machine) — the common case for these tests.
    fn tk(now: Instant, extra: u32) -> Tick {
        tk_active(now, extra, true)
    }

    /// Like [`tk`], but lets a test set whether the machine is actively in use this tick.
    fn tk_active(now: Instant, extra: u32, active: bool) -> Tick {
        Tick {
            now,
            today: day(),
            interval: TICK,
            warn: WARN,
            slack: SLACK,
            extra_minutes: extra,
            active,
        }
    }

    /// `Rules::default()` (fresh install) and a config that omits the fields (upgrade) must agree.
    /// They didn't: `Default` hardcoded `warn_secs: 0` while serde applied 60, so a brand-new
    /// install locked the child's screen with no warning while an upgraded one warned for 60s.
    #[test]
    fn fresh_install_defaults_match_serde_defaults() {
        let fresh = Rules::default();
        let upgraded: Rules = serde_json::from_str("{}").expect("empty rules object");

        assert_eq!(
            fresh.warn_secs, upgraded.warn_secs,
            "a fresh install and an upgraded config must get the same warning grace"
        );
        assert!(
            fresh.warn_secs > 0,
            "the child must get some warning before the screen locks"
        );
        // Guard the whole struct, so any future field added with a `#[serde(default = ...)]`
        // that Default forgets to mirror fails here rather than on someone's kid's laptop.
        // Compared as JSON because `Rules` intentionally doesn't derive `PartialEq`.
        assert_eq!(
            serde_json::to_value(&fresh).unwrap(),
            serde_json::to_value(&upgraded).unwrap()
        );
    }

    /// A tally written before foreground tracking existed must still load. Failing this bricks
    /// the enforcer on upgrade: `load_or_default` swallows the parse error and silently returns a
    /// zeroed tally, handing the child a fresh budget on the day of the update.
    #[test]
    fn a_tally_written_before_foreground_tracking_still_loads() {
        let legacy = r#"{"day":null,"total_secs":120,"per_app_secs":{"roblox.exe":60}}"#;
        let u: Usage =
            serde_json::from_str(legacy).expect("a pre-foreground tally must still load");
        assert_eq!(u.total_secs, 120);
        assert_eq!(u.per_app_secs.get("roblox.exe"), Some(&60));
        assert!(
            u.foreground_secs.is_empty(),
            "an absent field means no focus data, not zero focus"
        );
    }

    /// The whole safety argument for this feature in one test.
    ///
    /// Foreground figures come from a process running as the child, so they are attacker-chosen.
    /// They are for the report only — no enforcement path may read them. If this ever fails, a
    /// child can either free themselves from a limit or lock themselves out of the machine by
    /// lying to the watcher.
    #[test]
    fn foreground_time_cannot_trigger_a_per_app_limit() {
        let rules = Rules {
            app_limits: [("game.exe".to_string(), 1u32)].into_iter().collect(),
            ..Default::default()
        };
        let mut e = RulesEnforcer::new(Usage::default());
        // A wildly-inflated focus figure for the very app that has a limit.
        e.usage
            .foreground_secs
            .insert("game.exe".to_string(), 99_999);

        let actions = e.decide(&rules, &[proc(1, "game.exe")], tk(Instant::now(), 0));

        assert!(
            !actions.iter().any(|a| matches!(a, RuleAction::Kill(_))),
            "enforcement must read running time, never focused time"
        );
    }

    /// The map that is actually saved, not just the report that reaches it.
    ///
    /// `clamp` bounds a single report, and `page_secs` is re-capped here because forty *different*
    /// titles a tick still reaches thousands by bedtime. `foreground_secs` had the same exposure
    /// and no cap: this map is persisted to disk every tick and folded into the daily rollup, so
    /// unbounded growth here is unbounded growth on disk.
    #[test]
    fn a_days_worth_of_forged_app_names_cannot_grow_the_stored_tally() {
        let mut e = RulesEnforcer::new(Usage::default());

        // 200 ticks, each naming thirty executables nobody has installed.
        for tick in 0..200u64 {
            let mut sample = crate::foreground::Sample::default();
            for i in 0..30u64 {
                sample.apps.insert(format!("app{tick}-{i}.exe"), 1);
            }
            e.record_foreground(sample, Duration::from_secs(30));
        }

        assert!(
            e.usage.foreground_secs.len() <= crate::foreground::MAX_APPS,
            "stored tally grew to {} apps, cap is {}",
            e.usage.foreground_secs.len(),
            crate::foreground::MAX_APPS
        );
    }

    /// The cap has to cost the flood, not the measurement. An app with real hours behind it
    /// outweighs any number of one-second forgeries, so it survives being buried in them.
    #[test]
    fn a_forged_flood_does_not_evict_a_genuinely_used_app() {
        let mut e = RulesEnforcer::new(Usage::default());
        let mut real = crate::foreground::Sample::default();
        real.apps.insert("roblox.exe".to_string(), 30);
        e.record_foreground(real, Duration::from_secs(30));

        for tick in 0..200u64 {
            let mut sample = crate::foreground::Sample::default();
            for i in 0..30u64 {
                sample.apps.insert(format!("app{tick}-{i}.exe"), 1);
            }
            e.record_foreground(sample, Duration::from_secs(30));
        }

        assert_eq!(
            e.usage.foreground_secs.get("roblox.exe"),
            Some(&30),
            "real measured time must outlast the flood it is buried in"
        );
    }

    /// The security boundary for watcher input, pinned.
    ///
    /// The figures arrive from a process running as the child, so a report claiming more time than
    /// the tick actually lasted must be scaled down before it reaches a tally the parent reads.
    /// Skipping the clamp here is invisible — the number simply looks large.
    #[test]
    fn a_forged_report_is_bounded_before_it_reaches_the_tally() {
        let mut e = RulesEnforcer::new(Usage::default());
        let mut sample = crate::foreground::Sample::default();
        for i in 0..20 {
            sample.apps.insert(format!("app{i}.exe"), 30);
        }

        e.record_foreground(sample, Duration::from_secs(30));

        let total: u64 = e.usage.foreground_secs.values().sum();
        assert!(
            total <= 30,
            "20 apps each claiming the full tick must not sum to 600s, got {total}"
        );
    }

    /// Keys are normalised on the way in, so the watcher cannot split one app across two rows by
    /// sending a differently-cased name than the enforcement tally uses.
    #[test]
    fn watcher_keys_are_normalised_on_ingest() {
        let mut e = RulesEnforcer::new(Usage::default());
        let mut sample = crate::foreground::Sample::default();
        sample.apps.insert("  Roblox.EXE  ".to_string(), 10);

        e.record_foreground(sample, Duration::from_secs(30));

        assert_eq!(e.usage.foreground_secs.get("roblox.exe"), Some(&10));
        assert_eq!(
            e.usage.foreground_secs.len(),
            1,
            "no second casing survives"
        );
    }

    /// The day-level cap, not just the per-report one. Forty *different* titles every tick would
    /// otherwise reach thousands by bedtime in a map that is persisted and rolled up.
    #[test]
    fn page_titles_stay_capped_across_a_whole_day() {
        let mut e = RulesEnforcer::new(Usage::default());
        for tick in 0..50 {
            let mut sample = crate::foreground::Sample::default();
            for i in 0..40 {
                sample.pages.insert(format!("tick {tick} page {i}"), 1);
            }
            e.record_foreground(sample, Duration::from_secs(30));
        }

        assert!(
            e.usage.page_secs.len() <= crate::foreground::MAX_PAGES,
            "day-level cap failed: {} titles stored",
            e.usage.page_secs.len()
        );
    }

    /// A blank app name from the watcher must not become a row in the parent's report.
    ///
    /// Report-only, so unlike the blocklist and per-app cases this one cannot kill anything — but
    /// it arrives over the same pipe from the same process running as the child, and it is the
    /// last place in the crate that turns a name into a key without asking whether the name is
    /// one. `top_by_minutes` shows the heaviest entries, so a blank key with hours against it
    /// takes the top row of "what has he actually been doing" and says nothing.
    ///
    /// `pages` deliberately gets no equivalent filter: titles are display text rather than keys,
    /// a window genuinely may have no title, and `MAX_PAGES` already bounds them.
    #[test]
    fn a_blank_app_name_from_the_watcher_is_not_a_row() {
        let mut e = RulesEnforcer::new(Usage::default());
        let mut sample = crate::foreground::Sample::default();
        sample.apps.insert("   ".into(), 10);
        sample.apps.insert("Roblox.exe".into(), 10);

        e.record_foreground(sample, Duration::from_secs(30));

        assert_eq!(
            e.usage.foreground_secs.keys().collect::<Vec<_>>(),
            vec!["roblox.exe"],
            "only names that name something belong in the report"
        );
    }

    /// Page titles are display text, not match keys — lowercasing them would render "Roblox" as
    /// "roblox" for no benefit.
    #[test]
    fn page_titles_keep_their_original_case() {
        let mut e = RulesEnforcer::new(Usage::default());
        let mut sample = crate::foreground::Sample::default();
        sample.pages.insert("Roblox".to_string(), 10);

        e.record_foreground(sample, Duration::from_secs(30));

        assert_eq!(e.usage.page_secs.get("Roblox"), Some(&10));
    }

    /// Both numbers survive into the stored row, under distinct keys.
    #[test]
    fn the_rollup_row_carries_focused_minutes_beside_running_minutes() {
        let running: BTreeMap<String, u64> = [("roblox.exe".to_string(), 3600)].into();
        let focused: BTreeMap<String, u64> = [("roblox.exe".to_string(), 2400)].into();

        let row = rollup_row(
            &PreRollover {
                total_secs: 3600,
                per_app_secs: running.clone(),
                foreground_secs: focused.clone(),
                ..Default::default()
            },
            day(),
            Some(180),
        );

        assert_eq!(
            row["apps"]["roblox.exe"], 60,
            "60 minutes with the app open"
        );
        assert_eq!(
            row["focused"]["roblox.exe"], 40,
            "40 of those minutes actually looking at it"
        );
    }

    /// Regression: budget arithmetic must saturate, not wrap.
    ///
    /// Release builds don't check overflow, so `daily_budget_mins: u32::MAX` plus a single granted
    /// minute used to wrap to 0 — inverting enforcement (unlimited becomes instantly-over) with no
    /// error anywhere. `validate` now rejects such a value up front, but the arithmetic must be
    /// safe regardless, since a config can be hand-edited past validation.
    #[test]
    fn budget_math_saturates_instead_of_wrapping() {
        let rules = Rules {
            daily_budget_mins: u32::MAX,
            ..Default::default()
        };
        let day = day();
        assert_eq!(
            rules.effective_budget_mins(day, 1),
            u32::MAX,
            "a huge budget plus a grant must saturate, never wrap to a small number"
        );

        // And the validator refuses it before it can be stored.
        assert!(
            rules.validate().is_err(),
            "an absurd budget must be rejected"
        );

        // A grant on top of a near-max stored grant must not wrap either.
        let mut grant = crate::config::DailyGrant::default();
        grant.add(day, u32::MAX);
        grant.add(day, 60);
        assert_eq!(grant.for_day(day), u32::MAX);
    }

    /// Regression: flipping the timezone must not hand out a fresh budget.
    ///
    /// The attack was four clicks in Settings (changing the *time zone* needs no UAC prompt) between
    /// two zones a day apart: each flip changed the local date, and a date change wipes the whole
    /// tally. Repeatable every 30s, so screen-time limits became advisory. `accounting_day` is the
    /// second line of defence behind the anchored clock — a rollover is honoured at most once per
    /// 12 monotonic hours.
    #[test]
    fn a_flipping_clock_cannot_reset_the_days_tally() {
        let mut e = RulesEnforcer::new(Usage::default());
        let mon = NaiveDate::from_ymd_opt(2026, 8, 10).unwrap();
        let tue = NaiveDate::from_ymd_opt(2026, 8, 11).unwrap();
        let t0 = Instant::now();

        // First tick adopts whatever the clock says, and the first rollover is legitimate.
        assert_eq!(e.accounting_day(mon, t0), mon);
        e.usage.day = Some(mon);
        assert_eq!(e.accounting_day(tue, t0), tue, "the first rollover is real");
        e.usage.day = Some(tue);

        // Now the flip-flop: every further date change within 12h is refused.
        for i in 1..=20 {
            let flip = if i % 2 == 0 { tue } else { mon };
            assert_eq!(
                e.accounting_day(flip, t0 + Duration::from_secs(30 * i as u64)),
                tue,
                "flip {i} must not roll the day over"
            );
        }

        // A genuine next day, 12h+ later, still works — the guard delays resets, never blocks them.
        assert_eq!(
            e.accounting_day(mon, t0 + Duration::from_secs(13 * 3600)),
            mon,
            "a real rollover after 12h must be honoured"
        );
    }

    #[test]
    fn accrue_adds_and_resets_on_new_day() {
        let mut u = Usage::default();
        let targets = Targets {
            app_limits: [("game.exe".into(), 30)].into(),
            ..Default::default()
        };
        let running: BTreeSet<String> = ["game.exe".into()].into();
        u.accrue(day(), 30, &running, &targets);
        u.accrue(day(), 30, &running, &targets);
        assert_eq!(u.total_secs, 60);
        assert_eq!(u.per_app_secs["game.exe"], 60);
        // New day → reset.
        let next = NaiveDate::from_ymd_opt(2026, 7, 10).unwrap();
        u.accrue(next, 30, &running, &targets);
        assert_eq!(u.total_secs, 30);
        assert_eq!(u.per_app_secs["game.exe"], 30);
    }

    /// The live panel is ordered by time, capped, and drops what would render as a zero.
    #[test]
    fn todays_focus_list_is_heaviest_first_capped_and_free_of_zero_rows() {
        let mut secs = std::collections::BTreeMap::new();
        // Named so alphabetical order is the *opposite* of time order — if the sort were on the
        // key, as a BTreeMap's own iteration is, this test would catch it.
        secs.insert("a_small.exe".to_string(), 60u64); // 1 min
        secs.insert("z_big.exe".to_string(), 7_200u64); // 120 min
        secs.insert("m_mid.exe".to_string(), 600u64); // 10 min
        secs.insert("blip.exe".to_string(), 59u64); // 0 min — dropped

        let rows = top_by_minutes(&secs, TOP_TODAY);

        assert_eq!(
            rows.len(),
            3,
            "the sub-minute entry must not take a row: {rows:?}"
        );
        assert_eq!(rows[0]["name"], "z_big.exe");
        assert_eq!(rows[0]["minutes"], 120);
        assert_eq!(rows[1]["name"], "m_mid.exe");
        assert_eq!(rows[2]["name"], "a_small.exe");
    }

    /// The cap holds, and it keeps the heaviest rather than the alphabetically luckiest.
    #[test]
    fn todays_focus_list_keeps_the_heaviest_when_it_has_to_choose() {
        let mut secs = std::collections::BTreeMap::new();
        for i in 0..40u64 {
            // Later names carry more time, so a truncate-before-sort would keep exactly the wrong
            // half and this test would fail.
            secs.insert(format!("app{i:02}.exe"), (i + 1) * 60);
        }

        let rows = top_by_minutes(&secs, TOP_TODAY);

        assert_eq!(rows.len(), TOP_TODAY);
        assert_eq!(rows[0]["name"], "app39.exe", "heaviest first");
        assert_eq!(
            rows[TOP_TODAY - 1]["name"],
            "app30.exe",
            "tenth heaviest last"
        );
    }

    /// Ties are broken on name, so a refresh does not reshuffle equal rows.
    #[test]
    fn equal_focus_times_keep_a_stable_order() {
        let mut secs = std::collections::BTreeMap::new();
        secs.insert("b.exe".to_string(), 600u64);
        secs.insert("a.exe".to_string(), 600u64);
        secs.insert("c.exe".to_string(), 600u64);

        let names: Vec<String> = top_by_minutes(&secs, TOP_TODAY)
            .iter()
            .map(|r| r["name"].as_str().unwrap().to_string())
            .collect();

        assert_eq!(names, vec!["a.exe", "b.exe", "c.exe"]);
    }

    /// "Nobody was watching" and "he focused nothing" must not render the same.
    ///
    /// This is the distinction `DayRow::measured` draws for completed days and the Today card had
    /// no equivalent of. The threshold is what keeps it honest in both directions: silent below it
    /// (a quiet morning is not evidence of anything), confident above it.
    #[test]
    fn absent_focus_data_is_only_called_missing_once_there_is_use_to_contradict_it() {
        let rules = Rules::default();
        let day = NaiveDate::from_ymd_opt(2026, 8, 24).unwrap();

        let summary = |total_secs: u64, focus: &[(&str, u64)]| {
            let mut usage = Usage {
                day: Some(day),
                total_secs,
                ..Default::default()
            };
            for (name, secs) in focus {
                usage.foreground_secs.insert((*name).to_string(), *secs);
            }
            today_summary(&rules, day, 0, &usage, Some(1))
        };

        assert_eq!(
            summary(0, &[])["focus_missing"],
            false,
            "an unused machine says nothing about the watcher"
        );
        assert_eq!(
            summary(FOCUS_EVIDENCE_SECS - 1, &[])["focus_missing"],
            false,
            "just under the threshold is still 'we don't know'"
        );
        assert_eq!(
            summary(FOCUS_EVIDENCE_SECS, &[])["focus_missing"],
            true,
            "used this long with nothing reported means nothing is reporting"
        );
        assert_eq!(
            summary(86_400, &[("chrome.exe", 30)])["focus_missing"],
            false,
            "a single sub-minute report still proves the watcher is alive"
        );
    }

    /// The whole point of the feature: today's focus reaches the payload at all.
    #[test]
    fn today_summary_carries_todays_focus_and_page_figures() {
        let rules = Rules::default();
        let day = NaiveDate::from_ymd_opt(2026, 8, 24).unwrap();
        let mut usage = Usage {
            day: Some(day),
            total_secs: 3_600,
            ..Default::default()
        };
        usage.foreground_secs.insert("roblox.exe".into(), 1_800);
        usage.page_secs.insert("Roblox".into(), 900);

        let s = today_summary(&rules, day, 0, &usage, Some(1));

        assert_eq!(s["focused"][0]["name"], "roblox.exe");
        assert_eq!(s["focused"][0]["minutes"], 30);
        assert_eq!(s["pages"][0]["name"], "Roblox");
        assert_eq!(s["pages"][0]["minutes"], 15);
        assert_eq!(s["focus_missing"], false);
    }

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

    /// Important 1: an unknown budget must produce a row with no `budget` key at all — not a
    /// guessed value. `parse_row` treats a missing key as "unknown" (`over_budget: false`); a
    /// present-but-wrong key would instead render a false verdict on the wrong day (see
    /// `rollup_row`'s doc comment for the concrete Sunday/Monday scenario this guards against).
    /// The stand-down branch must close an open session before it forgets one is open.
    ///
    /// A source scan, because the property is the *existence of a call site* and no unit test can
    /// see one deleted: `should_abort_budget_shutdown` stays green whether or not anything calls it, and the
    /// emission itself lives inside `run_rules_enforcer`'s async loop where pinning it would mean
    /// standing up an enforcer. Mutation-checked by deleting the guard, which this catches and the
    /// unit tests do not.
    ///
    /// Worth a guard rather than a comment because the failure is **latent and silent**. Nothing
    /// reads `session_start`/`session_stop` today — the dashboard prints them verbatim and derives
    /// nothing — so losing the stop again would produce no symptom at all until somebody builds the
    /// timeline, and then it would produce a wrong one: spans shaded from a mid-afternoon pause
    /// through to bedtime, labelled as use. See `docs/OPEN-FINDINGS.md`, O36.
    /// The foreground feed must be drained on **every** tick, not only on ticks that enforce.
    ///
    /// A source scan, for the reason `standing_down_closes_an_open_session` gives: the property is
    /// *where a call site sits* relative to two early `continue`s, and no unit test can see one
    /// move. `record_foreground` stays correct in isolation whatever the loop does with it.
    ///
    /// The failure this guards is a fabricated number, not a missing one. `Feed::submit` accrues
    /// between drains — its own doc assumes they are "thirty seconds apart" — while `elapsed` is
    /// capped at `CHECK_INTERVAL * 2`. So if the drain sits below the stand-down branch, a paused
    /// weekend accumulates in the buffer and then lands clamped to at most 60 s on the tick that
    /// resumes: the report shows about a minute of "time in front" for two days of real use, which
    /// reads as measured rather than as unmeasured. `MAX_APPS`/`MAX_PAGES` evict the tail first, so
    /// the surviving minute is not even a fair sample of it.
    ///
    /// Draining above the branch and dropping the sample when the tick returns early is deliberate:
    /// `decide` does not run there either, so `total_secs` and `per_app_secs` do not accrue while
    /// paused. Banking focus time alone would leave the card internally inconsistent, and a sample
    /// carries no timestamps, so across a multi-day pause it cannot be attributed to a day anyway.
    #[test]
    fn the_foreground_feed_is_drained_before_any_early_continue() {
        const SRC: &str = include_str!("rules.rs");
        let code = SRC
            .split_once("\n#[cfg(test)]")
            .map_or(SRC, |(before, _)| before);

        // Anchored on the loop and its *first* `continue`, not on any line of tick arithmetic.
        // An earlier version split on the literal `let elapsed = now.duration_since(last_tick)`,
        // which pins prose rather than the property: renaming that binding would have silently
        // retired the guard while leaving it green. Whatever the tick comes to look like, the rule
        // is the same — the drain happens before the loop can take any early exit.
        let loop_body = code
            .split_once("\n    loop {")
            .expect("`run_rules_enforcer` must still be a `loop`")
            .1;
        let before_first_exit = loop_body
            .split_once("continue;")
            .expect("the loop must still have an early `continue`")
            .0;

        assert!(
            before_first_exit.contains("foreground.drain()"),
            "`foreground.drain()` does not run before the stand-down branch, so a pause of any \
             length accumulates in the feed and lands clamped to one tick when it resumes — \
             roughly a minute of \"time in front\" for a whole paused weekend, rendered as \
             measured. Drain once, above both early `continue`s, and drop the sample if the tick \
             returns early."
        );
    }

    #[test]
    fn standing_down_closes_an_open_session() {
        const SRC: &str = include_str!("rules.rs");
        // Drop this test module before scanning, or the prose above matches itself.
        let code = SRC
            .split_once("\n#[cfg(test)]")
            .map_or(SRC, |(before, _)| before);

        let branch = code
            .split_once("if mode == TickMode::StandDown {")
            .expect("the stand-down branch must exist")
            .1
            .split_once("continue;")
            .expect("the stand-down branch must end in `continue`")
            .0;

        assert!(
            branch.contains("\"session_stop\""),
            "the stand-down branch discards `prev_active` without recording a `session_stop`, so \
             the next active tick opens a second session with no stop between them. Measured on \
             real data when this was last broken: six `session_start` and zero `session_stop`."
        );
        assert!(
            branch.contains("prev_active == Some(true)"),
            "the stop must be conditional on a session actually being open — writing one \
             unconditionally invents a stop on every paused tick"
        );
    }

    /// The tick may stand down for a pause, and for nothing else.
    ///
    /// A source scan, for the reason its two neighbours are: the property is *which* `TickMode`
    /// values reach the early `continue`, and that lives inside `run_rules_enforcer`'s async loop
    /// where no unit test can observe it. `tick_mode` stays green whatever the loop does with its
    /// answer — which is precisely how the original defect survived. `any_configured()` was never
    /// wrong about what it computed; the loop drew the wrong conclusion from it.
    ///
    /// The failure it guards is silent and total. Standing down skips every accrual site in the
    /// loop, not just the enforcement ones, so widening this condition back to "not enforcing"
    /// stops the clock for every household that has not set a rule yet — which is every household
    /// on its first day — while `doctor` and the dashboard card both go on saying screen time is
    /// being counted. Nothing errors, nothing renders empty, and a household with no rules has no
    /// expected figure to check the zero against.
    #[test]
    fn only_a_pause_stands_the_tick_down() {
        const SRC: &str = include_str!("rules.rs");
        let code = SRC
            .split_once("\n#[cfg(test)]")
            .map_or(SRC, |(before, _)| before);
        let loop_body = code
            .split_once("\n    loop {")
            .expect("`run_rules_enforcer` must still be a `loop`")
            .1;
        let before_first_exit = loop_body
            .split_once("continue;")
            .expect("the loop must still have an early `continue`")
            .0;

        assert!(
            before_first_exit.contains("if mode == TickMode::StandDown {"),
            "the early `continue` is no longer reached by `TickMode::StandDown` alone. Standing \
             down skips measurement as well as enforcement, so any condition that also catches \
             `TickMode::Measure` stops the clock on every install nobody has set a rule on yet, \
             while `doctor` and the dashboard both still report screen time as being counted."
        );
        assert!(
            !before_first_exit.contains("has_targets"),
            "the stand-down condition must ask `tick_mode` rather than re-derive itself from \
             `has_targets` — folding the two questions back into one expression is exactly how \
             the three states collapsed into two the first time."
        );
    }

    /// A pause and an empty rule set are different facts and must not share a branch.
    ///
    /// They used to. `any_configured()` was `enabled && has_targets()`, so both answers stood the
    /// whole tick down — and standing down skips *measurement*, not just enforcement. A fresh
    /// install is `enabled` with nothing configured, so the state every new user starts in was the
    /// state in which no screen time was counted, while the dashboard card, its source comment and
    /// `doctor` each said it was counting. Nothing caught it because a household with no rules has
    /// no expected number to check the zero against.
    #[test]
    fn a_fresh_install_is_measured_rather_than_stood_down() {
        assert_eq!(
            Rules::default().tick_mode(),
            TickMode::Measure,
            "a fresh install is enabled with nothing configured — the one state where a parent is \
             told \"tracking only\", so it must be the state that tracks"
        );
    }

    /// Pause must keep meaning "stop watching him", not "watch him more quietly".
    #[test]
    fn pausing_stands_the_tick_down_however_much_is_configured() {
        let paused = Rules {
            enabled: false,
            daily_budget_mins: 60,
            blocklist: vec!["game.exe".into()],
            ..Default::default()
        };
        assert_eq!(paused.tick_mode(), TickMode::StandDown);
        assert_eq!(
            Rules {
                enabled: false,
                ..Default::default()
            }
            .tick_mode(),
            TickMode::StandDown,
            "paused with nothing configured is still paused"
        );
    }

    /// The load-bearing pair: measuring counts time, and measuring never acts.
    ///
    /// The second half is what makes the first safe to ship. `Measure` runs the same `decide` the
    /// enforcing path does, so the guarantee that it cannot kill, lock, warn or shut down is a
    /// property of the rules being empty rather than of a separate code path — and a property is
    /// worth a test where a second code path would have been worth a review.
    #[test]
    fn measuring_accrues_screen_time_and_enforces_nothing() {
        let rules = Rules::default();
        let mut e = RulesEnforcer::new(Usage::default());
        let now = Instant::now();

        // `&[]` is what the loop passes on this path — `Measure` skips the process scan.
        let first = e.decide(&rules, &[], tk(now, 0));
        assert!(
            first.is_empty(),
            "an unconfigured household must never be acted on: {first:?}"
        );
        assert_eq!(
            e.usage.total_secs,
            TICK.as_secs(),
            "the tick's seconds must reach the tally — this is the number the card reads"
        );

        // And it accumulates rather than being overwritten each tick.
        e.decide(&rules, &[], tk(now + TICK, 0));
        assert_eq!(e.usage.total_secs, TICK.as_secs() * 2);

        // The foreground watcher's report folds in on this path too, which is what puts app and
        // page rows on a dashboard belonging to a parent who has set no rules at all.
        let mut sample = crate::foreground::Sample::default();
        sample.apps.insert("Roblox.exe".into(), 30);
        sample.pages.insert("Roblox".into(), 30);
        e.record_foreground(sample, TICK);
        assert_eq!(e.usage.foreground_secs.get("roblox.exe"), Some(&30));
        assert_eq!(e.usage.page_secs.get("Roblox"), Some(&30));
    }

    /// Why `Measure` may skip the process scan: with nothing configured, the list changes nothing.
    ///
    /// `has_targets` was introduced because scanning the process table every 30 seconds with
    /// nothing to match was pure waste. That reasoning still holds and is why the scan is gated on
    /// `Enforce` — but it is only *safe* while an empty list and the real one produce the same
    /// tally. Pinned rather than argued, because the day someone adds a rule that reads `procs`
    /// unconditionally, this is what fails.
    ///
    /// Deliberately not `Rules::default()`. `has_targets` is false for three *non-empty*
    /// collections as well as for absent ones — a blocklist of blank strings, app limits that are
    /// all zero, groups with no pool — so `Measure` is reachable with all three populated, and
    /// those are the shapes where an empty `procs` could plausibly diverge from a real one. The
    /// equivalence rests on three separate filters agreeing (`Targets::from_rules` for limits and
    /// groups, and `decide`'s own filter for the blocklist), which is worth a test rather than a
    /// reading. It was not a reading that held: the blocklist filter did not exist until
    /// `a_blank_blocklist_entry_matches_nothing` was written.
    #[test]
    fn the_skipped_process_scan_cannot_change_what_is_measured() {
        let rules = Rules {
            // Every collection non-empty, and every entry one `has_targets` discounts.
            blocklist: vec!["".into(), "   ".into()],
            app_limits: [("roblox.exe".to_string(), 0u32)].into_iter().collect(),
            app_groups: vec![AppGroup {
                name: "Games".into(),
                apps: vec!["roblox.exe".into()],
                limit_mins: 0,
            }],
            ..Default::default()
        };
        assert_eq!(
            rules.tick_mode(),
            TickMode::Measure,
            "three populated collections that still amount to nothing to enforce"
        );

        // Ordinary names, plus one that normalises to the empty string — the case that made the
        // blocklist filter necessary.
        let procs = [proc(1, "roblox.exe"), proc(2, "chrome.exe"), proc(3, "   ")];
        let now = Instant::now();

        let mut scanned = RulesEnforcer::new(Usage::default());
        let mut skipped = RulesEnforcer::new(Usage::default());
        let acted = scanned.decide(&rules, &procs, tk(now, 0));
        skipped.decide(&rules, &[], tk(now, 0));

        assert!(
            acted.is_empty(),
            "nothing here is a target, so nothing may be acted on: {acted:?}"
        );
        assert_eq!(
            scanned.usage.to_json(),
            skipped.usage.to_json(),
            "reading the process list while measuring must be an optimisation to skip, not a \
             difference in what gets recorded"
        );
    }

    #[test]
    fn rollup_row_omits_budget_when_unknown() {
        let per_app = BTreeMap::new();
        let row = rollup_row(
            &PreRollover {
                total_secs: 7_200,
                per_app_secs: per_app.clone(),
                ..Default::default()
            },
            day(),
            None,
        );

        assert!(
            row.as_object().unwrap().get("budget").is_none(),
            "an unknown budget must be absent, not a guessed fallback: {row}"
        );
    }

    #[test]
    fn rollup_row_includes_budget_when_known() {
        let per_app = BTreeMap::new();
        let row = rollup_row(
            &PreRollover {
                total_secs: 7_200,
                per_app_secs: per_app.clone(),
                ..Default::default()
            },
            day(),
            Some(120),
        );

        assert_eq!(row["budget"], 120);
    }

    /// The writer (`rollup_row`, here) and the reader (`screentime::parse_row`) each call the
    /// other its "mirror image" in a doc comment — but until this test that relationship lived
    /// only in prose. Both sides' own tests use hand-written fixtures, so renaming a field or
    /// changing an encoding would leave both suites green and simply drop every row in production.
    /// This is the one test a drift between them actually breaks.
    #[test]
    fn a_written_row_round_trips_through_the_screentime_reader() {
        let mut per_app = BTreeMap::new();
        per_app.insert("game.exe".to_string(), 3_600u64);

        let row = rollup_row(
            &PreRollover {
                total_secs: 7_530,
                per_app_secs: per_app.clone(),
                ..Default::default()
            },
            day(),
            Some(90),
        );

        // Read it back the way the report does, with a window ending on the day just written.
        let report = crate::screentime::build_report(&[row], day().succ_opt().unwrap(), 1);

        assert_eq!(report.days.len(), 1);
        assert_eq!(report.days[0].date, day().to_string());
        assert_eq!(report.days[0].minutes_used, Some(125));
        assert_eq!(report.days[0].budget, Some(90));
        assert_eq!(report.days[0].apps[0].name, "game.exe");
        assert_eq!(report.days[0].apps[0].minutes, 60);
    }

    /// The other half of the contract: an omitted budget must survive the trip as "no verdict",
    /// never as a zero that would read as "no limit" or flag a false over-budget day.
    #[test]
    fn a_row_written_without_a_budget_round_trips_as_no_verdict() {
        let row = rollup_row(
            &PreRollover {
                total_secs: 14_400,
                per_app_secs: BTreeMap::new(),
                ..Default::default()
            },
            day(),
            None,
        ); // 240 min, budget unknown
        let report = crate::screentime::build_report(&[row], day().succ_opt().unwrap(), 1);

        assert_eq!(report.days[0].minutes_used, Some(240));
        assert_eq!(report.days[0].budget, None);
        assert!(
            !report.days[0].over_budget,
            "an unknown budget must never produce an over-budget verdict"
        );
    }

    /// Important 2 (the spec's "Per-app rollup" test): the row carries the exact per-app map it
    /// was given, converted to minutes — the map a caller passes in, not some other snapshot.
    #[test]
    fn rollup_row_converts_seconds_to_minutes_and_carries_the_per_app_map() {
        let mut per_app = BTreeMap::new();
        per_app.insert("game.exe".to_string(), 3_600u64); // 60 min
        per_app.insert("blip.exe".to_string(), 30u64); // 0 min — dropped, same as per_app_minutes

        let row = rollup_row(
            &PreRollover {
                total_secs: 7_530,
                per_app_secs: per_app.clone(),
                ..Default::default()
            },
            day(),
            Some(90),
        ); // 7530s = 125.5 min -> 125

        assert_eq!(row["date"], day().to_string());
        assert_eq!(
            row["minutes_used"], 125,
            "seconds must convert to whole minutes"
        );
        assert_eq!(
            row["apps"]["game.exe"], 60,
            "must carry the map it was given"
        );
        assert!(
            row["apps"].get("blip.exe").is_none(),
            "sub-minute apps still drop out, same as the direct per_app_minutes conversion"
        );
    }

    #[test]
    fn group_pool_kills_all_members_when_spent() {
        // A 1-minute pool shared by two games. Running either accrues to the pool; once spent,
        // every running member is killed.
        let rules = Rules {
            app_groups: vec![AppGroup {
                name: "Games".into(),
                apps: vec!["Minecraft.exe".into(), "roblox.exe".into()],
                limit_mins: 1,
            }],
            ..Default::default()
        };
        let mut e = RulesEnforcer::new(Usage::default());
        let procs = [proc(10, "minecraft.exe"), proc(11, "roblox.exe")];
        let now = Instant::now();
        // First 30s tick: under the pool, no kills.
        assert!(e.decide(&rules, &procs, tk(now, 0)).is_empty());
        // Second 30s tick reaches the 60s pool → both members killed.
        let a = e.decide(&rules, &procs, tk(now, 0));
        assert_eq!(a, vec![RuleAction::Kill(10), RuleAction::Kill(11)]);
        // A non-member is untouched.
        let with_other = [proc(10, "minecraft.exe"), proc(12, "notepad.exe")];
        let a2 = e.decide(&rules, &with_other, tk(now, 0));
        assert_eq!(a2, vec![RuleAction::Kill(10)]);
    }

    #[test]
    fn group_pool_shared_across_members() {
        // The pool is shared: alternating between two members still drains the one pool.
        let rules = Rules {
            app_groups: vec![AppGroup {
                name: "Games".into(),
                apps: vec!["a.exe".into(), "b.exe".into()],
                limit_mins: 1,
            }],
            ..Default::default()
        };
        let mut e = RulesEnforcer::new(Usage::default());
        let now = Instant::now();
        e.decide(&rules, &[proc(1, "a.exe")], tk(now, 0)); // 30s on a
        let a = e.decide(&rules, &[proc(2, "b.exe")], tk(now, 0)); // 30s on b → pool spent
        assert_eq!(a, vec![RuleAction::Kill(2)]);
    }

    /// Everything `has_targets` calls "nothing configured" must also be unable to act.
    ///
    /// The class, not an instance of it. `has_targets` discounts five different shapes, and each
    /// one is a promise that nothing in it can match a process — a promise kept by a *different*
    /// filter in a different place. The blocklist's filter was missing entirely, and fixing only
    /// the blocklist left the same hole in `app_limits` (which filters on the value and never
    /// looks at the key) and in `has_pool` (which asks whether `apps` is non-empty, not whether
    /// any entry could name anything). Both were measured killing a blank-named process while
    /// `has_targets` called them unconfigured.
    ///
    /// Written as a table over shapes rather than one test per shape, because the next collection
    /// added to `Rules` should fail here by omission if its author forgets the same filter.
    #[test]
    fn nothing_has_targets_discounts_can_ever_act() {
        let group = |apps: Vec<&str>, limit: u32| AppGroup {
            name: "Games".into(),
            apps: apps.into_iter().map(String::from).collect(),
            limit_mins: limit,
        };
        let shapes: Vec<(&str, Rules)> = vec![
            (
                "a blocklist of blank rows",
                Rules {
                    blocklist: vec!["".into(), "   ".into()],
                    ..Default::default()
                },
            ),
            (
                "per-app limits that are all zero",
                Rules {
                    app_limits: [("game.exe".to_string(), 0u32)].into_iter().collect(),
                    ..Default::default()
                },
            ),
            (
                "a per-app limit under a blank name",
                Rules {
                    app_limits: [("   ".to_string(), 60u32)].into_iter().collect(),
                    ..Default::default()
                },
            ),
            (
                "a group with no pool",
                Rules {
                    app_groups: vec![group(vec!["game.exe"], 0)],
                    ..Default::default()
                },
            ),
            (
                "a group whose only member is blank",
                Rules {
                    app_groups: vec![group(vec!["", "  "], 60)],
                    ..Default::default()
                },
            ),
        ];

        // Ordinary names plus the two that normalise away — the ones a missing filter matches.
        let procs = [
            proc(1, "game.exe"),
            proc(2, "roblox.exe"),
            proc(3, ""),
            proc(4, "   "),
        ];

        for (label, rules) in shapes {
            assert_eq!(
                rules.tick_mode(),
                TickMode::Measure,
                "{label}: has_targets must discount this, or the enforcer scans the process \
                 table every 30s for a rule that can never fire"
            );

            // Two ticks, so anything that accrues has time to cross a one-minute limit.
            let mut e = RulesEnforcer::new(Usage::default());
            let now = Instant::now();
            e.decide(&rules, &procs, tk(now, 0));
            let actions = e.decide(&rules, &procs, tk(now, 0));
            assert!(
                actions.is_empty(),
                "{label}: discounted by `has_targets` and yet able to act — {actions:?}"
            );
        }
    }

    /// A blank row never reaches memory in the first place.
    ///
    /// `Targets::from_rules` filters blanks at the point of use and that is the guard that
    /// matters — a `Rules` built in code never passes through serde. This is the other half:
    /// dropping them on the way in keeps them out of `config.json`, off the dashboard as a
    /// phantom empty row, and out of `MAX_RULE_ENTRIES`, where a hundred blank rows would
    /// otherwise crowd out real ones.
    ///
    /// Done in `Deserialize` rather than in the POST handler deliberately. Two endpoints accept a
    /// `Rules` — `set_rules` and `save_routine`, the latter storing a preset applied later — and a
    /// third path loads one from disk. A `sanitize()` call would have to be remembered at each,
    /// and the one that forgot would be the one that reintroduced the row.
    #[test]
    fn blank_blocklist_rows_never_survive_deserialization() {
        let r: Rules = serde_json::from_str(r#"{"blocklist":["","   ","\t","game.exe"]}"#)
            .expect("rules with blank rows must still parse, not fail");
        assert_eq!(
            r.blocklist,
            vec!["game.exe".to_string()],
            "only rows that could name a process may survive"
        );

        // The same door a hand-edited config comes through, and the reason `has_targets` must
        // agree: an all-blank list is not a configured blocklist.
        let all_blank: Rules = serde_json::from_str(r#"{"blocklist":["",""]}"#).expect("parses");
        assert!(all_blank.blocklist.is_empty());
        assert_eq!(all_blank.tick_mode(), TickMode::Measure);
    }

    /// A blank blocklist entry must match nothing — including a process whose name is blank.
    ///
    /// `has_targets` discounts blank entries, and its doc once justified that by claiming
    /// `norm()` "can never match" them. It was wrong. `norm` is `trim` plus `to_lowercase`, so
    /// `""` and `"   "` both normalise to the empty string, and the empty string is a perfectly
    /// good key — it matches any process whose name trims to nothing.
    ///
    /// Found while proving that `TickMode::Measure` may skip the process scan. It is reachable on
    /// the enforcing path too, which is the one that kills: a parent needs only a real limit (so
    /// `has_targets` is true) plus one stray empty row in the blocklist, which an empty text input
    /// produces without ceremony. `validate` bounds the list's length and not its contents, and
    /// this codebase already assumes a hand-edited config can walk past validation entirely.
    ///
    /// Whether Windows ever reports a blank process name is unproven — `sysinfo` takes it from the
    /// toolhelp snapshot — so this is a latent defect, not an observed one. It is worth closing
    /// anyway, because the fix is to make `decide` agree with the predicate that already claims to
    /// describe it: `has_targets`'s own doc requires these predicates to match `Targets::from_rules`
    /// exactly, and the blocklist is the one collection that never passes through `Targets`.
    #[test]
    fn a_blank_blocklist_entry_matches_nothing() {
        let rules = Rules {
            daily_budget_mins: 60, // a real target, so this is the enforcing path
            blocklist: vec!["".into(), "   ".into()],
            ..Default::default()
        };
        assert_eq!(
            rules.tick_mode(),
            TickMode::Enforce,
            "a budget makes this the live killing path, blank entries and all"
        );

        let mut e = RulesEnforcer::new(Usage::default());
        let actions = e.decide(&rules, &[proc(9, "   ")], tk(Instant::now(), 0));

        assert!(
            actions.is_empty(),
            "a blank blocklist row must not become a kill-on-sight rule for blank-named \
             processes: {actions:?}"
        );
    }

    #[test]
    fn blocklist_produces_kill() {
        let rules = Rules {
            blocklist: vec!["Game.exe".into()], // case-insensitive
            ..Default::default()
        };
        let mut e = RulesEnforcer::new(Usage::default());
        let procs = [proc(10, "game.exe"), proc(11, "notepad.exe")];
        let actions = e.decide(&rules, &procs, tk(Instant::now(), 0));
        assert_eq!(actions, vec![RuleAction::Kill(10)]);
    }

    #[test]
    fn app_limit_kills_when_exceeded() {
        let rules = Rules {
            app_limits: [("game.exe".into(), 1)].into(), // 1 minute
            ..Default::default()
        };
        let mut e = RulesEnforcer::new(Usage::default());
        let procs = [proc(10, "game.exe")];
        let now = Instant::now();
        // First two 30s ticks = 60s = the 1-min limit → the second tick kills.
        let a1 = e.decide(&rules, &procs, tk(now, 0));
        assert!(a1.is_empty(), "30s in, under the limit");
        let a2 = e.decide(&rules, &procs, tk(now, 0));
        assert_eq!(a2, vec![RuleAction::Kill(10)]);
    }

    /// Collect the advance warnings emitted over `ticks` ticks of `rules`.
    fn countdown_over(e: &mut RulesEnforcer, rules: &Rules, extra: u32, ticks: usize) -> Vec<u32> {
        let base = Instant::now();
        (0..ticks)
            .flat_map(|_| e.decide(rules, &[], tk(base, extra)))
            .filter_map(|a| match a {
                RuleAction::TimeWarning(m) => Some(m),
                _ => None,
            })
            .collect()
    }

    /// The module's standard fixture: a Lock-mode `Rules` with an `mins`-minute daily budget.
    /// One minute is 60s, i.e. exactly two [`TICK`]s — which is why most tests here use `budget(1)`.
    fn budget(mins: u32) -> Rules {
        Rules {
            daily_budget_mins: mins,
            budget_action: EnforceAction::Lock,
            ..Default::default()
        }
    }

    /// `remaining_mins` must clamp, not truncate. Both the parent dashboard and the child's own
    /// page derive remaining time from this; `as u32` on an absurd tally wraps to a small number,
    /// which reads as "plenty of budget left" — enforcement inverted, in the UI, silently.
    /// `child_status` used to do exactly that cast while `today_summary` clamped.
    #[test]
    fn remaining_mins_clamps_an_absurd_tally_instead_of_wrapping() {
        let huge = Usage {
            day: Some(day()),
            total_secs: u64::MAX,
            ..Default::default()
        };
        assert_eq!(huge.remaining_mins(60), Some(0), "spent, not 'plenty left'");

        let normal = Usage {
            day: Some(day()),
            total_secs: 40 * 60,
            ..Default::default()
        };
        assert_eq!(normal.remaining_mins(60), Some(20));
        assert_eq!(normal.remaining_mins(0), None, "no budget today");
    }

    /// The two child-facing strings that only the enforcement loop ever builds.
    ///
    /// Found by the first coverage run this project has had: `budget_countdown_message` below was
    /// pinned, but its two neighbours were reachable *only* from `run_rules_enforcer`, which no
    /// test drives — so both Dutch translations shipped with nothing asserting them. A wrong or
    /// empty `Nl` arm would have reached a child's screen with every gate green.
    ///
    /// Pins the property that survives rewording: each language says something, says something
    /// *different* from the others (a copy-pasted English arm is the likely mistake), and names
    /// the number it was given.
    /// The child is at the machine, so the address has to work from there: `localhost`, which
    /// needs no DHCP lease or machine name, is a SAN on the generated certificate, and is admitted
    /// by the LAN gate as loopback. The port is the configured one — `install --port N` moves it,
    /// and a wrong port is worse than no address.
    #[test]
    fn the_child_is_told_where_to_ask_at_an_address_that_works_from_the_machine() {
        for lang in Language::ALL {
            let hint = ask_hint(9443, lang, false).expect("a hint when curfew is not open");
            assert!(
                hint.contains("https://localhost:9443/ask"),
                "{lang:?}: the address must be reachable from the child's own PC: {hint}"
            );
            assert!(
                !hint.contains("8443"),
                "{lang:?}: the port must come from config, not a hardcoded default: {hint}"
            );
        }
        let en = ask_hint(8443, Language::En, false).unwrap();
        let nl = ask_hint(8443, Language::Nl, false).unwrap();
        assert_ne!(en, nl, "one language was never translated");
    }

    /// The reported evening, from the child's side. A grant cannot move bedtime, so while the
    /// window is open "ask for more time" is a promise the system cannot keep — and the parent
    /// would be the one left saying no to a request that was never going to work.
    #[test]
    fn no_one_is_invited_to_ask_while_bedtime_is_shutting_the_machine_down() {
        for lang in Language::ALL {
            assert_eq!(
                ask_hint(8443, lang, true),
                None,
                "{lang:?}: asking cannot help once the curfew window is open"
            );
        }
    }

    /// The other half of that rule, and the reason it is scoped to "window open" rather than
    /// "bedtime is coming". With twenty minutes until bedtime and screen time gone now, asking is
    /// still worth doing — the child would get the minutes in between.
    #[test]
    fn the_invitation_survives_a_bedtime_that_has_not_arrived() {
        assert!(ask_hint(8443, Language::En, false).is_some());
    }

    /// The hint is appended, never substituted — a child who cannot act on the address still has
    /// to be told what is happening to their machine.
    #[test]
    fn the_countdown_still_says_what_it_always_said() {
        let plain = budget_countdown_message(5, Language::En);
        let with = with_hint(plain.clone(), Some("Need more? https://localhost:8443/ask"));
        assert!(
            with.starts_with(&plain),
            "the warning itself must survive: {with}"
        );
        assert!(with.contains("/ask"));
        assert_eq!(
            with_hint(plain.clone(), None),
            plain,
            "with no hint the message is untouched"
        );
    }

    #[test]
    fn every_language_has_its_own_lock_and_limit_wording() {
        let locks: Vec<String> = Language::ALL
            .iter()
            .map(|&l| lock_warning_message(30, l))
            .collect();
        for (lang, msg) in Language::ALL.iter().zip(&locks) {
            assert!(!msg.trim().is_empty(), "{lang:?} has no lock warning");
            assert!(
                msg.contains("30"),
                "{lang:?} lock warning drops the seconds: {msg}"
            );
        }
        for (i, a) in locks.iter().enumerate() {
            for b in &locks[i + 1..] {
                assert_ne!(
                    a, b,
                    "two languages share a lock warning — one was never translated"
                );
            }
        }

        // The shutdown notice, which until now was a bare English literal. It is the harsher of
        // the two budget endings — `budget_action` picks between this and the lock warning above —
        // and it was the only one of the pair that a Dutch install showed in English.
        let shutdowns: Vec<&str> = Language::ALL.iter().map(|&l| shutdown_message(l)).collect();
        for (lang, msg) in Language::ALL.iter().zip(&shutdowns) {
            assert!(!msg.trim().is_empty(), "{lang:?} has no shutdown notice");
        }
        for (i, a) in shutdowns.iter().enumerate() {
            for b in &shutdowns[i + 1..] {
                assert_ne!(
                    a, b,
                    "two languages share a shutdown notice — one was never translated"
                );
            }
        }

        let limits: Vec<&str> = Language::ALL
            .iter()
            .map(|&l| limit_reached_message(l))
            .collect();
        for (lang, msg) in Language::ALL.iter().zip(&limits) {
            assert!(
                !msg.trim().is_empty(),
                "{lang:?} has no limit-reached message"
            );
        }
        for (i, a) in limits.iter().enumerate() {
            for b in &limits[i + 1..] {
                assert_ne!(
                    a, b,
                    "two languages share a limit message — one was never translated"
                );
            }
        }
    }

    /// Mirror of curfew's `bedtime_messages_read_naturally_at_every_threshold`. Both message
    /// tables hand-write the singular case, so both need pinning — otherwise adding a threshold
    /// breaks one of them and only the other reaches CI.
    #[test]
    fn budget_countdown_messages_read_naturally_at_every_threshold() {
        for m in crate::countdown::WARN_AT_MINS {
            // Both languages, because a translation that pluralises "1 minuten" is exactly the
            // trap the singular arms exist to avoid, and English passing says nothing about Dutch.
            // Derived from `Language::ALL` so a third language cannot be added past this test.
            for lang in Language::ALL {
                let msg = budget_countdown_message(m, lang);
                assert!(
                    msg.contains(&m.to_string()),
                    "{msg} should name the minutes"
                );
                assert!(
                    !msg.contains("1 minutes") && !msg.contains("1 minuten"),
                    "singular must not be pluralised ({lang:?}): {msg}"
                );
                assert!(!msg.is_empty(), "{lang:?} must have a string for {m}");
            }
        }
    }

    /// The child gets advance notice at every threshold, exactly once, *before* the budget runs
    /// out. Without this the screen locking was the first they heard of it — `warn_secs` is a
    /// grace period after exhaustion, not a heads-up before it.
    #[test]
    fn budget_announces_each_countdown_threshold_once() {
        let mut e = RulesEnforcer::new(Usage::default());
        // 40 ticks of 30s == the whole 20-minute budget.
        let announced = countdown_over(&mut e, &budget(20), 0, 40);
        assert_eq!(announced, crate::countdown::WARN_AT_MINS.to_vec());
    }

    /// Granted time must earn a fresh countdown. Otherwise the child gets warnings on the way
    /// down the original budget and then silence through the grant, right up to the lock.
    #[test]
    fn granted_time_earns_a_fresh_countdown() {
        let rules = budget(1);
        let mut e = RulesEnforcer::new(Usage::default());
        let base = Instant::now();
        e.decide(&rules, &[], tk(base, 0)); // 30s
        e.decide(&rules, &[], tk(base, 0)); // 60s → spent
        // The parent grants 20 more minutes; the ladder re-arms with no explicit reset anywhere.
        let announced = countdown_over(&mut e, &rules, 20, 40);
        assert_eq!(announced, vec![15, 5, 1]);
    }

    /// Turning a budget on mid-day must not announce a threshold the child never crossed. With
    /// 8 minutes left on a fresh 10-minute budget, "15 minutes remaining" would be a lie — and
    /// the countdown has no way to know it wasn't already counting before the budget existed.
    #[test]
    fn turning_a_budget_on_mid_day_does_not_announce_a_stale_threshold() {
        let mut e = RulesEnforcer::new(Usage::default());
        // Two minutes of an unlimited day...
        countdown_over(&mut e, &budget(0), 0, 4);
        // ...then a 10-minute budget appears, leaving 8.
        let announced = countdown_over(&mut e, &budget(10), 0, 20);
        assert_eq!(announced, vec![5, 1], "never above 15, so 15 must not fire");
    }

    /// Once the budget is spent the at-zero warning takes over; the countdown must stay quiet
    /// rather than talk over it with "1 minute left" as the screen is locking.
    #[test]
    fn countdown_is_silent_once_the_budget_is_spent() {
        let mut e = RulesEnforcer::new(Usage::default());
        // A 3-minute budget: 6 ticks to spend it, 6 more past exhaustion.
        let announced = countdown_over(&mut e, &budget(3), 0, 12);
        assert_eq!(
            announced,
            vec![1],
            "warned once at a minute left, then silent through exhaustion"
        );
    }

    #[test]
    fn budget_lock_arms_then_locks_after_warn() {
        let rules = budget(1);
        let mut e = RulesEnforcer::new(Usage::default());
        let base = Instant::now();
        // Two ticks reach the 60s budget; the second arms the grace deadline and says so.
        e.decide(&rules, &[], tk(base, 0));
        let armed = e.decide(&rules, &[], tk(base, 0));
        assert_eq!(
            armed,
            vec![RuleAction::LockWarning],
            "the grace notice is the machine's decision, not something the loop infers"
        );
        // Past the warn deadline → lock.
        let locked = e.decide(&rules, &[], tk(base + Duration::from_secs(61), 0));
        assert_eq!(locked, vec![RuleAction::LockScreen]);
    }

    #[test]
    fn budget_shutdown_issues_then_reissues() {
        let rules = Rules {
            daily_budget_mins: 1,
            budget_action: EnforceAction::Shutdown,
            ..Default::default()
        };
        let mut e = RulesEnforcer::new(Usage::default());
        let base = Instant::now();
        e.decide(&rules, &[], tk(base, 0)); // 30s
        let first = e.decide(&rules, &[], tk(base, 0)); // 60s → over
        assert_eq!(first, vec![RuleAction::Shutdown], "issued with countdown");
        // Still over past deadline+slack (child ran `shutdown /a`) → re-issue with NO countdown.
        // A second warned shutdown just gave them another window to cancel.
        let reissue = e.decide(&rules, &[], tk(base + Duration::from_secs(91), 0));
        assert_eq!(reissue, vec![RuleAction::ShutdownNow]);
        // Repeated cancelling never earns another grace period.
        for i in 1..=5 {
            let t = base + Duration::from_secs(91 + 91 * i);
            assert_eq!(
                e.decide(&rules, &[], tk(t, 0)),
                vec![RuleAction::ShutdownNow],
                "cancel attempt {i} must not earn another countdown"
            );
        }
    }

    #[test]
    fn extra_minutes_raise_the_budget() {
        let rules = budget(1);
        let mut e = RulesEnforcer::new(Usage::default());
        let now = Instant::now();
        // 60s used, but +1 extra minute → budget is 120s, so not over yet.
        e.decide(&rules, &[], tk(now, 1));
        let a = e.decide(&rules, &[], tk(now, 1));
        assert!(a.is_empty(), "extra minute keeps us under budget");
    }

    #[test]
    fn warn_action_records_but_does_not_enforce() {
        let rules = Rules {
            daily_budget_mins: 1,
            budget_action: EnforceAction::Warn,
            ..Default::default()
        };
        let mut e = RulesEnforcer::new(Usage::default());
        let now = Instant::now();
        e.decide(&rules, &[], tk(now, 0));
        let a = e.decide(&rules, &[], tk(now, 0));
        assert_eq!(a, vec![RuleAction::Warn]);
    }

    #[test]
    fn inactive_ticks_do_not_accrue_time() {
        let rules = budget(1);
        let mut e = RulesEnforcer::new(Usage::default());
        let now = Instant::now();
        // Two inactive ticks (nobody logged in / screen locked) accrue nothing…
        e.decide(&rules, &[], tk_active(now, 0, false));
        e.decide(&rules, &[], tk_active(now, 0, false));
        assert_eq!(e.usage.total_secs, 0, "no time charged while inactive");
        // …so an active tick afterwards is still well under budget (no lock).
        let a = e.decide(&rules, &[], tk_active(now, 0, true));
        assert!(a.is_empty());
        assert_eq!(e.usage.total_secs, 30);
    }

    #[test]
    fn inactive_over_budget_does_not_enforce_and_rearms_on_return() {
        let rules = budget(1);
        let mut e = RulesEnforcer::new(Usage::default());
        let base = Instant::now();
        // Spend the budget while active, arming the grace deadline.
        e.decide(&rules, &[], tk_active(base, 0, true));
        e.decide(&rules, &[], tk_active(base, 0, true)); // 60s → over, deadline armed
        assert!(e.budget_deadline.is_some());
        // The screen locks (our budget lock, or the child): now inactive. Even well past the
        // old deadline we neither lock nor keep the deadline armed — no in-absentia re-locking.
        let locked = e.decide(
            &rules,
            &[],
            tk_active(base + Duration::from_secs(90), 0, false),
        );
        assert!(locked.is_empty(), "no lock re-issued while inactive");
        assert!(
            e.budget_deadline.is_none(),
            "deadline disarmed while inactive"
        );
        // On return, still over budget and ALREADY warned this episode → lock on the first active
        // tick. This assertion is the inverse of what it used to be: the old code armed a fresh
        // grace here, which made Win+L a complete bypass (lock, wait one tick, unlock, collect
        // another `warn_secs` — forever, never involuntarily locked). Going inactive must not
        // launder the warning.
        let back = e.decide(
            &rules,
            &[],
            tk_active(base + Duration::from_secs(95), 0, true),
        );
        assert_eq!(
            back,
            vec![RuleAction::LockScreen],
            "already warned this episode — returning must re-lock, not re-grant the grace"
        );
    }

    /// The bypass itself: cycle inactive/active repeatedly and assert it buys no free time.
    #[test]
    fn locking_the_screen_does_not_earn_more_time() {
        let rules = budget(1);
        let mut e = RulesEnforcer::new(Usage::default());
        let base = Instant::now();

        // Burn the budget and take the one legitimate warning.
        e.decide(&rules, &[], tk_active(base, 0, true));
        e.decide(&rules, &[], tk_active(base, 0, true));
        assert!(e.budget_deadline.is_some(), "warned once, grace armed");

        // Now the attack: lock (inactive tick) then unlock (active tick), over and over.
        for i in 1..=10 {
            let t = base + Duration::from_secs(100 * i);
            let away = e.decide(&rules, &[], tk_active(t, 0, false));
            assert!(away.is_empty(), "round {i}: never act in absentia");

            let back = e.decide(&rules, &[], tk_active(t + Duration::from_secs(5), 0, true));
            assert_eq!(
                back,
                vec![RuleAction::LockScreen],
                "round {i}: cycling the lock must not buy another grace period"
            );
        }

        // A grant ends the episode, so the next one earns a warning again rather than an
        // instant lock — the guard must not make the tool feel punitive after more time is given.
        let after_grant = e.decide(
            &rules,
            &[],
            tk_active(base + Duration::from_secs(2000), 60, true),
        );
        assert!(
            after_grant.is_empty(),
            "a grant puts them back under budget"
        );
        assert!(!e.episode_warned, "a new episode earns a fresh warning");
    }

    #[test]
    fn blocklist_kills_even_while_inactive() {
        // Kill-on-sight isn't time-based, so it fires regardless of session state.
        let rules = Rules {
            blocklist: vec!["game.exe".into()],
            ..Default::default()
        };
        let mut e = RulesEnforcer::new(Usage::default());
        let procs = [proc(10, "game.exe")];
        let actions = e.decide(&rules, &procs, tk_active(Instant::now(), 0, false));
        assert_eq!(actions, vec![RuleAction::Kill(10)]);
    }

    #[test]
    fn under_budget_clears_the_deadline() {
        let rules = budget(1);
        let mut e = RulesEnforcer::new(Usage::default());
        let now = Instant::now();
        e.decide(&rules, &[], tk(now, 0));
        e.decide(&rules, &[], tk(now, 0)); // over → armed
        assert!(e.budget_deadline.is_some());
        // A big grant puts us back under budget → deadline cleared.
        e.decide(&rules, &[], tk(now, 60));
        assert!(e.budget_deadline.is_none());
    }

    #[test]
    fn abort_budget_shutdown_only_on_falling_edge_and_off_curfew() {
        // Still over budget under Shutdown → leave the countdown running.
        assert!(!should_abort_budget_shutdown(true, true, false));
        // Grant lifted us back under budget, curfew inactive → cancel the pending shutdown.
        assert!(should_abort_budget_shutdown(true, false, false));
        // Back under budget, but curfew is active → it's curfew's shutdown now; don't touch it.
        assert!(!should_abort_budget_shutdown(true, false, true));
        // Nothing was pending → nothing to cancel.
        assert!(!should_abort_budget_shutdown(false, false, false));
    }

    #[test]
    fn the_today_card_lists_no_limit_that_cannot_fire() {
        // A blank-named limit is discounted by `has_targets` and dropped by `Targets`, so it can
        // never fire. Listing it on the card would show a parent a limit that does nothing —
        // the same class of untrue statement as the zero-valued limit filtered beside it.
        let rules = Rules {
            app_limits: [("   ".to_string(), 60u32), ("game.exe".to_string(), 30)].into(),
            ..Default::default()
        };
        let usage = Usage {
            day: Some(day()),
            ..Default::default()
        };

        let s = today_summary(&rules, day(), 0, &usage, Some(1));
        let per_app = s["per_app"].as_array().unwrap();

        assert_eq!(
            per_app.len(),
            1,
            "only the limit that could actually fire belongs on the card: {per_app:?}"
        );
        assert_eq!(per_app[0]["name"], "game.exe");
    }

    #[test]
    fn today_summary_reports_used_remaining_and_per_app() {
        let rules = Rules {
            daily_budget_mins: 120,
            app_limits: [("Game.exe".into(), 60), ("chrome.exe".into(), 0)].into(), // 0 = off
            ..Default::default()
        };
        let mut usage = Usage {
            day: Some(day()),
            total_secs: 47 * 60,
            per_app_secs: Default::default(),
            per_group_secs: Default::default(),
            foreground_secs: Default::default(),
            page_secs: Default::default(),
        };
        usage.per_app_secs.insert("game.exe".into(), 20 * 60); // normalized key
        // +30 granted → effective budget 150, used 47 → remaining 103.
        let s = today_summary(&rules, day(), 30, &usage, Some(12));
        assert_eq!(s["budget_mins"], 150);
        assert_eq!(s["used_mins"], 47);
        assert_eq!(s["remaining_mins"], 103);
        assert_eq!(s["extra_mins"], 30);
        // Only the limited, non-zero app is listed; its raw name is shown, usage from the
        // normalized tally key.
        let per_app = s["per_app"].as_array().unwrap();
        assert_eq!(per_app.len(), 1);
        assert_eq!(per_app[0]["name"], "Game.exe");
        assert_eq!(per_app[0]["used_mins"], 20);
        assert_eq!(per_app[0]["limit_mins"], 60);
    }

    #[test]
    fn today_summary_has_null_remaining_without_a_budget() {
        let rules = Rules::default(); // no daily budget
        let usage = Usage {
            day: Some(day()),
            total_secs: 90 * 60,
            per_app_secs: Default::default(),
            per_group_secs: Default::default(),
            foreground_secs: Default::default(),
            page_secs: Default::default(),
        };
        let s = today_summary(&rules, day(), 0, &usage, Some(12));
        assert_eq!(s["budget_mins"], 0);
        assert_eq!(s["used_mins"], 90);
        assert!(s["remaining_mins"].is_null());
    }

    #[test]
    fn today_summary_ignores_extra_on_an_unlimited_day() {
        // No base budget for the day → a stray granted `extra` must NOT show a phantom budget the
        // enforcer would never apply (card and enforcer must agree).
        let rules = Rules::default(); // no daily budget
        let usage = Usage {
            day: Some(day()),
            total_secs: 10 * 60,
            per_app_secs: Default::default(),
            per_group_secs: Default::default(),
            foreground_secs: Default::default(),
            page_secs: Default::default(),
        };
        let s = today_summary(&rules, day(), 30, &usage, Some(12)); // 30 granted, but base is 0
        assert_eq!(s["budget_mins"], 0);
        assert!(s["remaining_mins"].is_null());
    }

    #[test]
    fn today_summary_uses_the_weekday_budget() {
        // day() is Thursday (index 3) → base 90.
        let rules = Rules {
            budget_by_weekday: Some(vec![10, 10, 10, 90, 10, 240, 240]),
            ..Default::default()
        };
        let usage = Usage {
            day: Some(day()),
            total_secs: 30 * 60,
            per_app_secs: Default::default(),
            per_group_secs: Default::default(),
            foreground_secs: Default::default(),
            page_secs: Default::default(),
        };
        let s = today_summary(&rules, day(), 0, &usage, Some(12));
        assert_eq!(s["budget_mins"], 90);
        assert_eq!(s["remaining_mins"], 60);
    }

    /// The field that used to be untestable, and the one that matters most when it's wrong.
    ///
    /// While `today_summary` read the heartbeat globals itself, asserting on `enforcer_age_secs`
    /// would have coupled this test to whatever else in the binary had called `beat()` — so the
    /// impure field was precisely the unasserted one. Both cases below are load-bearing: a stale
    /// age is what the dashboard turns into "enforcement may not be running", and `null` is the
    /// never-reported case, which after one tick's uptime means the loops never started.
    #[test]
    fn today_summary_passes_the_enforcer_heartbeat_through() {
        let rules = Rules::default();
        let usage = Usage {
            day: Some(day()),
            total_secs: 0,
            per_app_secs: Default::default(),
            per_group_secs: Default::default(),
            foreground_secs: Default::default(),
            page_secs: Default::default(),
        };

        let fresh = today_summary(&rules, day(), 0, &usage, Some(7));
        assert_eq!(fresh["enforcer_age_secs"], 7);

        let stale = today_summary(&rules, day(), 0, &usage, Some(3600));
        assert_eq!(stale["enforcer_age_secs"], 3600);

        let never = today_summary(&rules, day(), 0, &usage, None);
        assert!(
            never["enforcer_age_secs"].is_null(),
            "a never-reported enforcer must surface as null, not as a healthy-looking zero"
        );
    }

    #[test]
    fn per_weekday_budget_overrides_the_default() {
        let thu = day(); // 2026-07-09 is a Thursday
        assert_eq!(thu.weekday(), Weekday::Thu);
        let rules = Rules {
            daily_budget_mins: 60,
            budget_by_weekday: Some(vec![30, 30, 30, 30, 30, 120, 120]), // Mon..Sun
            ..Default::default()
        };
        // Thursday uses its override (30), not the everyday default (60).
        assert_eq!(rules.base_budget_for(Weekday::Thu), 30);
        assert_eq!(rules.base_budget_for(Weekday::Sat), 120);
        assert_eq!(rules.effective_budget_mins(thu, 15), 45); // 30 + 15 granted
        // Without the override, the everyday default applies to every day.
        let plain = Rules {
            daily_budget_mins: 60,
            ..Default::default()
        };
        assert_eq!(plain.base_budget_for(Weekday::Thu), 60);
    }

    #[test]
    fn per_weekday_zero_means_no_budget_that_day() {
        // Weekdays off, weekends 240. day() is a Thursday → 0 → no enforcement.
        let rules = Rules {
            budget_by_weekday: Some(vec![0, 0, 0, 0, 0, 240, 240]),
            budget_action: EnforceAction::Lock,
            ..Default::default()
        };
        let mut e = RulesEnforcer::new(Usage::default());
        let now = Instant::now();
        e.decide(&rules, &[], tk(now, 0));
        let a = e.decide(&rules, &[], tk(now, 0));
        assert!(a.is_empty(), "Thursday has no budget → never locks");
        assert!(e.budget_deadline.is_none());
        // But the weekend budgets still make the enforcer active (so it runs on Sat/Sun).
        assert_eq!(rules.tick_mode(), TickMode::Enforce);
    }

    #[test]
    fn pausing_disables_all_rules() {
        let rules = Rules {
            enabled: false,
            daily_budget_mins: 60,
            blocklist: vec!["game.exe".into()],
            ..Default::default()
        };
        // Paused → the loop skips everything, even a configured blocklist.
        assert_eq!(rules.tick_mode(), TickMode::StandDown);
        // Flip it back on → active again.
        let on = Rules {
            enabled: true,
            ..rules
        };
        assert_eq!(on.tick_mode(), TickMode::Enforce);
    }

    #[test]
    fn validate_rejects_huge_warn() {
        let ok = Rules::default();
        assert!(ok.validate().is_ok());
        let bad = Rules {
            warn_secs: MAX_WARN_SECS + 1,
            ..Default::default()
        };
        assert!(bad.validate().is_err());
    }
}
