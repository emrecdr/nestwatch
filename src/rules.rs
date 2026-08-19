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

use crate::config::Config;
use crate::control::{ControlError, ProcessInfo, SystemControl};
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
        self.limit_mins > 0 && !self.apps.is_empty()
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
    #[serde(default)]
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
    /// The predicates must match [`Targets::from_rules`] exactly. They didn't: a non-empty
    /// `app_limits` counted here even when every value was `0`, while `Targets` filters those
    /// out — so the enforcer woke up and scanned the process table every 30s with nothing to
    /// enforce, and `doctor` reported "rules active" for rules that could never fire. Same for a
    /// blocklist holding only blank strings, which `norm()` can never match.
    pub fn has_targets(&self) -> bool {
        self.has_any_budget()
            || self.blocklist.iter().any(|b| !b.trim().is_empty())
            || self.app_limits.values().any(|&m| m > 0)
            || self.app_groups.iter().any(|g| g.has_pool())
    }

    /// Whether the enforcer has any work this tick — false when paused, letting the loop skip the
    /// session/process scan entirely.
    pub fn any_configured(&self) -> bool {
        self.enabled && self.has_targets()
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
}

/// Rule-derived, normalized enforcement targets for one tick — built once by `decide` and shared
/// by accrual and the kill checks so the two can't disagree on what's tracked.
#[derive(Default)]
pub(crate) struct Targets {
    /// Per-app limits (minutes), keyed by normalized process name (zero-limit apps dropped).
    app_limits: BTreeMap<String, u32>,
    /// App groups with a shared pool: (name, normalized member set, limit minutes). Only groups
    /// with a positive limit and at least one member are included.
    groups: Vec<(String, BTreeSet<String>, u32)>,
}

impl Targets {
    fn from_rules(rules: &Rules) -> Self {
        let app_limits = rules
            .app_limits
            .iter()
            .filter(|(_, v)| **v > 0)
            .map(|(k, &v)| (norm(k), v))
            .collect();
        let groups = rules
            .app_groups
            .iter()
            .filter(|g| g.has_pool())
            .map(|g| {
                (
                    g.name.clone(),
                    g.apps.iter().map(|a| norm(a)).collect(),
                    g.limit_mins,
                )
            })
            .collect();
        Self { app_limits, groups }
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
        .app_limits
        .iter()
        .filter(|(_, v)| **v > 0)
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
    /// Advance heads-up: this many minutes of budget remain (one of [`countdown::WARN_AT_MINS`]).
    /// Fires *before* the budget is spent, independent of `budget_action` — a Shutdown-mode day
    /// gets the same countdown as a Lock-mode one, since Windows' own dialog only appears at zero.
    TimeWarning(u32),
}

/// Deadline-based budget state machine (mirrors `curfew::Enforcer`), plus the running tally.
/// The day's tally as it stood *before* a tick ran — the numbers a rollover row describes.
///
/// Produced only by [`RulesEnforcer::decide_after_snapshot`], which is the point: these values are
/// unrecoverable once `decide` has run, so there is no way to ask for them too late.
pub struct PreRollover {
    pub day: Option<NaiveDate>,
    pub total_secs: u64,
    pub per_app_secs: BTreeMap<String, u64>,
}

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
    /// Advance warnings ("15 minutes left"), announced on the way down. See [`countdown`].
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
        procs: &[ProcessInfo],
        t: Tick,
    ) -> (PreRollover, Vec<RuleAction>) {
        let prev = PreRollover {
            day: self.usage.day,
            total_secs: self.usage.total_secs,
            per_app_secs: self.usage.per_app_secs.clone(),
        };
        (prev, self.decide(rules, procs, t))
    }

    pub fn decide(&mut self, rules: &Rules, procs: &[ProcessInfo], t: Tick) -> Vec<RuleAction> {
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
        let blocked: BTreeSet<String> = rules.blocklist.iter().map(|b| norm(b)).collect();
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
fn norm(name: &str) -> String {
    name.trim().to_lowercase()
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
fn rollup_row(
    date: NaiveDate,
    total_secs: u64,
    budget: Option<u32>,
    per_app_secs: &BTreeMap<String, u64>,
) -> Value {
    let mut row = serde_json::Map::new();
    row.insert("date".into(), Value::from(date.to_string()));
    row.insert("minutes_used".into(), Value::from(total_secs / 60));
    if let Some(b) = budget {
        row.insert("budget".into(), Value::from(b));
    }
    row.insert("apps".into(), Value::Object(per_app_minutes(per_app_secs)));
    Value::Object(row)
}

/// Background loop: every [`CHECK_INTERVAL`], enforce the usage rules. Runs for the life of the
/// server; if it ever returns, the caller logs that loudly.
pub async fn run_rules_enforcer(
    control: Arc<dyn SystemControl>,
    config: Arc<RwLock<Config>>,
    usage_log: Arc<crate::usage::UsageLog>,
    screentime_log: Arc<crate::screentime::ScreentimeLog>,
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
        crate::heartbeat::tick(&mut ticker, crate::heartbeat::Enforcer::Rules).await;

        // Charge the time that actually passed, clamped to twice the interval. A hardcoded
        // CHECK_INTERVAL over-charges after any stall (suspend, CPU starvation, a slow scan);
        // real elapsed time alone would charge an entire sleep as screen time on modern-standby
        // laptops, where the monotonic clock keeps running through S0ix. The clamp means one long
        // gap costs at most one extra tick.
        let now = Instant::now();
        let elapsed = now.duration_since(last_tick).min(CHECK_INTERVAL * 2);
        last_tick = now;

        // `accounting_day` may hold the previous date if the clock just jumped — a rollover
        // wipes the tally, so it needs a monotonic sanity check, not just a trusted clock.
        let today = enforcer.accounting_day(crate::config::today(), now);
        // Snapshot the config under the lock, then drop the guard before any await.
        let (rules, extra) = {
            let guard = crate::state::recover_read(&config);
            (guard.rules.clone(), guard.extra.for_day(today))
        };

        if !rules.any_configured() {
            // Nothing to enforce this tick (paused, or no rules). But if we had a budget shutdown
            // in flight, cancel it — otherwise pausing (or clearing the budget) mid-countdown
            // would still power the machine off.
            prev_shutdown_wanted = maybe_abort_budget_shutdown(
                &control,
                &config,
                &usage_log,
                prev_shutdown_wanted,
                false,
                serde_json::json!({ "reason": "paused" }),
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

        let procs = {
            let control = control.clone();
            match tokio::task::spawn_blocking(move || control.list_processes()).await {
                Ok(Ok(procs)) => procs,
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
            let row = rollup_row(pd, prev.total_secs, prev_budget, &prev.per_app_secs);
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
                    let msg = "Screen time is up — shutting down.".to_string();
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
                        &format!(
                            "Screen time is up. This computer will lock in {} seconds.",
                            rules.warn_secs
                        ),
                    )
                    .await;
                }
                RuleAction::TimeWarning(mins) => {
                    // Record the heads-up only if the OS actually took the message. A countdown
                    // the child never saw must not look, in the history, like one they did.
                    if notify_child(&control, &budget_countdown_message(mins)).await {
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
            notify_child(&control, "You've reached today's screen-time limit.").await;
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
async fn notify_child(control: &Arc<dyn SystemControl>, body: &str) -> bool {
    crate::control::notify(control, "Screen time", body).await
}

/// What the child is told at each remaining-time threshold. `mins` is one of
/// [`crate::countdown::WARN_AT_MINS`]; the catch-all keeps the wording sane if that list ever
/// changes, and sidesteps pluralising "1 minute" by naming the singular case outright.
fn budget_countdown_message(mins: u32) -> String {
    match mins {
        1 => "1 minute of screen time left!".to_string(),
        5 => "5 minutes of screen time left — good time to save.".to_string(),
        m => format!("{m} minutes of screen time left today."),
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

    fn proc(pid: u32, name: &str) -> ProcessInfo {
        ProcessInfo {
            pid,
            name: name.into(),
            memory_bytes: 0,
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
    #[test]
    fn rollup_row_omits_budget_when_unknown() {
        let per_app = BTreeMap::new();
        let row = rollup_row(day(), 7_200, None, &per_app);

        assert!(
            row.as_object().unwrap().get("budget").is_none(),
            "an unknown budget must be absent, not a guessed fallback: {row}"
        );
    }

    #[test]
    fn rollup_row_includes_budget_when_known() {
        let per_app = BTreeMap::new();
        let row = rollup_row(day(), 7_200, Some(120), &per_app);

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

        let row = rollup_row(day(), 7_530, Some(90), &per_app);

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
        let row = rollup_row(day(), 14_400, None, &BTreeMap::new()); // 240 min, budget unknown
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

        let row = rollup_row(day(), 7_530, Some(90), &per_app); // 7530s = 125.5 min -> 125

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

    /// Mirror of curfew's `bedtime_messages_read_naturally_at_every_threshold`. Both message
    /// tables hand-write the singular case, so both need pinning — otherwise adding a threshold
    /// breaks one of them and only the other reaches CI.
    #[test]
    fn budget_countdown_messages_read_naturally_at_every_threshold() {
        for m in crate::countdown::WARN_AT_MINS {
            let msg = budget_countdown_message(m);
            assert!(
                msg.contains(&m.to_string()),
                "{msg} should name the minutes"
            );
            assert!(
                !msg.contains("1 minutes"),
                "singular must not be pluralised: {msg}"
            );
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
        assert!(rules.any_configured());
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
        assert!(!rules.any_configured());
        // Flip it back on → active again.
        let on = Rules {
            enabled: true,
            ..rules
        };
        assert!(on.any_configured());
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
