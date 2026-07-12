//! Usage rules: a daily screen-time budget, an app blocklist (kill-on-sight), and per-app
//! daily time limits — enforced by a background loop alongside the curfew enforcer.
//!
//! Split like `curfew`: the [`RulesEnforcer::decide`] state machine is pure (it takes the
//! process list + an injected clock and returns [`RuleAction`]s), so it is exhaustively
//! unit-testable; [`run_rules_enforcer`] is the only part that reads the clock, persists the
//! running tally, and calls the OS.
//!
//! Interaction with curfew: both enforcers may independently request lock/shutdown — that's
//! safe because those ops are idempotent. Only the **curfew** enforcer ever issues
//! `abort_shutdown`, so it stays authoritative over the single OS pending-shutdown slot; the
//! rules enforcer simply stops re-issuing when back under budget.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use chrono::{Datelike, NaiveDate, Weekday};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::control::{ProcessInfo, SystemControl};
use crate::curfew::{MAX_WARN_SECS, default_warn_secs};

/// How often the enforcer re-checks (matches the curfew enforcer).
const CHECK_INTERVAL: Duration = Duration::from_secs(30);

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
    fn default() -> Self {
        Self {
            enabled: true,
            daily_budget_mins: 0,
            budget_by_weekday: None,
            blocklist: Vec::new(),
            app_limits: BTreeMap::new(),
            warn_secs: 0,
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

    /// Whether the enforcer has any work this tick — false when paused, letting the loop skip the
    /// session/process scan entirely.
    pub fn any_configured(&self) -> bool {
        self.enabled
            && (self.has_any_budget() || !self.blocklist.is_empty() || !self.app_limits.is_empty())
    }

    /// Validate (at config load and on POST). Fail-open like curfew: only the warning bound.
    pub fn validate(&self) -> Result<(), String> {
        if self.warn_secs > MAX_WARN_SECS {
            return Err(format!("warning seconds must be <= {MAX_WARN_SECS}"));
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
        if base > 0 { base + extra } else { 0 }
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
}

impl Usage {
    /// Add `delta_secs` to the total and to each tracked app currently running, resetting first
    /// if the local day changed. `running` and `limits` keys are already normalized. Pure.
    pub fn accrue(
        &mut self,
        today: NaiveDate,
        delta_secs: u64,
        running: &BTreeSet<String>,
        limits: &BTreeMap<String, u32>,
    ) {
        if self.day != Some(today) {
            self.day = Some(today);
            self.total_secs = 0;
            self.per_app_secs.clear();
        }
        self.total_secs += delta_secs;
        for name in running {
            if limits.contains_key(name) {
                *self.per_app_secs.entry(name.clone()).or_insert(0) += delta_secs;
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
                total_secs: 0,
                per_app_secs: BTreeMap::new(),
            }
        }
    }

    fn save(&self, path: &std::path::Path) {
        if let Ok(json) = serde_json::to_string(self)
            && let Err(e) = crate::config::write_atomic(path, json.as_bytes())
        {
            tracing::warn!(error = %e, "usage tally save failed");
        }
    }
}

/// Path to the persisted daily-tally sidecar. One home so the enforcer (writer) and the
/// read-only "today's usage" endpoint (reader) can't disagree on the location.
pub(crate) fn usage_state_path() -> std::path::PathBuf {
    crate::config::data_paths().dir.join("usage_state.json")
}

/// Build the read-only "today's usage" summary served at `GET /api/usage/today`: minutes
/// used/remaining against today's effective budget, plus per-app usage for apps that have a
/// limit. Pure (no I/O) so it's unit-tested; the handler supplies the config snapshot and the
/// loaded tally. `remaining_mins` is `null` when no budget is set.
pub fn today_summary(
    rules: &Rules,
    today: NaiveDate,
    extra: u32,
    usage: &Usage,
) -> serde_json::Value {
    let budget = rules.effective_budget_mins(today, extra);
    let used_mins = usage.total_secs / 60;
    let remaining_mins = (budget > 0).then(|| budget.saturating_sub(used_mins as u32));
    let per_app: Vec<serde_json::Value> = rules
        .app_limits
        .iter()
        .filter(|(_, v)| **v > 0)
        .map(|(name, &lim)| {
            let used = usage.per_app_secs.get(&norm(name)).copied().unwrap_or(0) / 60;
            serde_json::json!({ "name": name, "used_mins": used, "limit_mins": lim })
        })
        .collect();
    serde_json::json!({
        "day": usage.day.map(|d| d.to_string()),
        "enabled": rules.enabled,
        "budget_mins": budget,
        "used_mins": used_mins,
        "remaining_mins": remaining_mins,
        "extra_mins": extra,
        "per_app": per_app,
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
/// The rules enforcer deliberately has **no `Abort` variant**: only the curfew enforcer issues
/// `abort_shutdown`, so it stays authoritative over the single OS pending-shutdown slot (two
/// writers would fight). Don't add one here — when back under budget, just stop re-issuing.
#[derive(Debug, PartialEq, Eq)]
pub enum RuleAction {
    /// Terminate this PID (blocklisted, or an app over its per-app limit).
    Kill(u32),
    /// Lock the screen (budget spent, action = Lock).
    LockScreen,
    /// Issue a warned shutdown (budget spent, action = Shutdown).
    Shutdown,
    /// Budget spent, action = Warn — record only, no OS action.
    Warn,
}

/// Deadline-based budget state machine (mirrors `curfew::Enforcer`), plus the running tally.
pub struct RulesEnforcer {
    pub usage: Usage,
    /// When set, the budget is over and this is the grace deadline (Lock) or the expected
    /// shutdown-completion time (Shutdown, for re-issue detection). `None` = under budget.
    budget_deadline: Option<Instant>,
}

impl RulesEnforcer {
    fn new(usage: Usage) -> Self {
        Self {
            usage,
            budget_deadline: None,
        }
    }

    /// Decide this tick's actions. Pure: accrues into `self.usage`, updates `budget_deadline`,
    /// and returns the actions — no I/O, no real clock. `now`/`today` are injected.
    pub fn decide(&mut self, rules: &Rules, procs: &[ProcessInfo], t: Tick) -> Vec<RuleAction> {
        let mut actions = Vec::new();

        // Normalized per-app limits (drop zero limits) and running names.
        let limits: BTreeMap<String, u32> = rules
            .app_limits
            .iter()
            .filter(|(_, v)| **v > 0)
            .map(|(k, &v)| (norm(k), v))
            .collect();
        let running: BTreeSet<String> = procs.iter().map(|p| norm(&p.name)).collect();

        // Only charge screen time while the machine is actively in use. A locked screen or a
        // logged-out console still resets on a new day (accrue handles the rollover), but adds
        // no seconds — so overnight idle time and the budget-lock's own locked screen don't
        // count against the budget.
        if t.active {
            self.usage
                .accrue(t.today, t.interval.as_secs(), &running, &limits);
        } else {
            self.usage.accrue(t.today, 0, &running, &limits);
        }

        // Blocklist (kill on sight) + per-app over-limit → kill those PIDs.
        let blocked: BTreeSet<String> = rules.blocklist.iter().map(|b| norm(b)).collect();
        for p in procs {
            let n = norm(&p.name);
            if blocked.contains(&n) {
                actions.push(RuleAction::Kill(p.pid));
                continue;
            }
            if let Some(&lim) = limits.get(&n)
                && self.usage.per_app_secs.get(&n).copied().unwrap_or(0) >= lim as u64 * 60
            {
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
                match rules.budget_action {
                    EnforceAction::Warn => {
                        self.budget_deadline = None;
                        actions.push(RuleAction::Warn);
                    }
                    EnforceAction::Lock => match self.budget_deadline {
                        None => self.budget_deadline = Some(t.now + t.warn),
                        Some(dl) if t.now >= dl => actions.push(RuleAction::LockScreen),
                        Some(_) => {}
                    },
                    EnforceAction::Shutdown => match self.budget_deadline {
                        None => {
                            self.budget_deadline = Some(t.now + t.warn);
                            actions.push(RuleAction::Shutdown);
                        }
                        Some(dl) if t.now >= dl + t.slack => {
                            self.budget_deadline = Some(t.now + t.warn);
                            actions.push(RuleAction::Shutdown);
                        }
                        Some(_) => {}
                    },
                }
            } else {
                self.budget_deadline = None;
            }
        } else {
            self.budget_deadline = None;
        }

        actions
    }
}

/// Normalize a process name for matching: trimmed + lowercased (`"Chrome.exe"` == `"chrome.exe"`).
fn norm(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Background loop: every [`CHECK_INTERVAL`], enforce the usage rules. Runs for the life of the
/// server; if it ever returns, the caller logs that loudly.
pub async fn run_rules_enforcer(
    control: Arc<dyn SystemControl>,
    config: Arc<RwLock<Config>>,
    usage_log: Arc<crate::usage::UsageLog>,
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

    loop {
        ticker.tick().await;

        let today = crate::config::today();
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

        let prev_day = enforcer.usage.day;
        let prev_total = enforcer.usage.total_secs;
        let was_armed = enforcer.budget_deadline.is_some(); // for the lock-grace warning below
        let actions = enforcer.decide(
            &rules,
            &procs,
            Tick {
                now: Instant::now(),
                today,
                interval: CHECK_INTERVAL,
                warn: Duration::from_secs(rules.warn_secs as u64),
                slack: CHECK_INTERVAL,
                extra_minutes: extra,
                active,
            },
        );
        enforcer.usage.save(&tally_path);

        let budget = rules.effective_budget_mins(today, extra);

        // Log the previous day's total once, on rollover. Report the budget that was in force at
        // the *end of that day* (carried across ticks), not today's — otherwise the fresh day's
        // reset extra-time grant would be misattributed to yesterday's row. On the first tick
        // after a restart we have no carried value, so fall back to today's budget as a proxy.
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

        let mut has_lock = false;
        let mut has_shutdown = false;
        let mut has_warn = false;
        for action in actions {
            match action {
                RuleAction::Kill(pid) => {
                    let control = control.clone();
                    let _ = tokio::task::spawn_blocking(move || control.kill_process(pid)).await;
                }
                RuleAction::LockScreen => {
                    has_lock = true;
                    let control = control.clone();
                    let _ = tokio::task::spawn_blocking(move || control.lock_workstation()).await;
                }
                RuleAction::Shutdown => {
                    has_shutdown = true;
                    let control = control.clone();
                    let msg = "Screen time is up — shutting down.".to_string();
                    let secs = rules.warn_secs;
                    let _ = tokio::task::spawn_blocking(move || control.shutdown(secs, Some(msg)))
                        .await;
                }
                RuleAction::Warn => has_warn = true,
            }
        }

        // Warn the child before enforcement bites, so a lock/limit isn't a silent surprise.
        // Lock: notify the moment the grace period begins (before the screen actually locks),
        // and again each time it re-arms after they return. Warn action: notify on the rising
        // edge. Shutdown already shows Windows' own countdown, so it isn't doubled up here.
        // (Checked before the `log_transition` calls below, which flip `warning`.)
        let grace_started = !was_armed
            && enforcer.budget_deadline.is_some()
            && rules.budget_action == EnforceAction::Lock;
        if grace_started {
            notify_child(
                &control,
                &format!(
                    "Screen time is up. This computer will lock in {} seconds.",
                    rules.warn_secs
                ),
            )
            .await;
        } else if has_warn && !warning {
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
        if let Ok(Err(e)) = tokio::task::spawn_blocking(move || control.abort_shutdown()).await {
            tracing::warn!(error = %e, "failed to abort budget shutdown");
        }
        usage_log.record("budget_shutdown_aborted", detail);
    }
    now_wanted
}

/// Best-effort child-facing notification (offloaded to the blocking pool; failures are logged
/// at debug and swallowed — a missed warning must never stall or crash the enforcer). Title is
/// fixed so callers only pass the message body.
async fn notify_child(control: &Arc<dyn SystemControl>, body: &str) {
    let control = control.clone();
    let (title, body) = ("Screen time".to_string(), body.to_string());
    if let Ok(Err(e)) = tokio::task::spawn_blocking(move || control.notify_user(title, body)).await
    {
        tracing::debug!(error = %e, "child notification failed");
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

    #[test]
    fn accrue_adds_and_resets_on_new_day() {
        let mut u = Usage::default();
        let limits: BTreeMap<String, u32> = [("game.exe".into(), 30)].into();
        let running: BTreeSet<String> = ["game.exe".into()].into();
        u.accrue(day(), 30, &running, &limits);
        u.accrue(day(), 30, &running, &limits);
        assert_eq!(u.total_secs, 60);
        assert_eq!(u.per_app_secs["game.exe"], 60);
        // New day → reset.
        let next = NaiveDate::from_ymd_opt(2026, 7, 10).unwrap();
        u.accrue(next, 30, &running, &limits);
        assert_eq!(u.total_secs, 30);
        assert_eq!(u.per_app_secs["game.exe"], 30);
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

    #[test]
    fn budget_lock_arms_then_locks_after_warn() {
        let rules = Rules {
            daily_budget_mins: 1, // 60s
            budget_action: EnforceAction::Lock,
            ..Default::default()
        };
        let mut e = RulesEnforcer::new(Usage::default());
        let base = Instant::now();
        // Two ticks reach the 60s budget; the second arms the grace deadline (no action yet).
        e.decide(&rules, &[], tk(base, 0));
        let armed = e.decide(&rules, &[], tk(base, 0));
        assert!(armed.is_empty(), "armed grace, not locked yet");
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
        // Still over past deadline+slack (child ran `shutdown /a`) → re-issue.
        let reissue = e.decide(&rules, &[], tk(base + Duration::from_secs(91), 0));
        assert_eq!(reissue, vec![RuleAction::Shutdown]);
    }

    #[test]
    fn extra_minutes_raise_the_budget() {
        let rules = Rules {
            daily_budget_mins: 1, // 60s base
            budget_action: EnforceAction::Lock,
            ..Default::default()
        };
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
        let rules = Rules {
            daily_budget_mins: 1, // 60s
            budget_action: EnforceAction::Lock,
            ..Default::default()
        };
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
        let rules = Rules {
            daily_budget_mins: 1, // 60s
            budget_action: EnforceAction::Lock,
            ..Default::default()
        };
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
        // On return (still over budget) a fresh grace is armed — not an instant lock.
        let back = e.decide(
            &rules,
            &[],
            tk_active(base + Duration::from_secs(95), 0, true),
        );
        assert!(
            back.is_empty(),
            "fresh warning grace, not an immediate lock"
        );
        assert!(e.budget_deadline.is_some());
        // …and after that grace elapses, it locks.
        let relock = e.decide(
            &rules,
            &[],
            tk_active(base + Duration::from_secs(160), 0, true),
        );
        assert_eq!(relock, vec![RuleAction::LockScreen]);
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
        let rules = Rules {
            daily_budget_mins: 1,
            budget_action: EnforceAction::Lock,
            ..Default::default()
        };
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
        };
        usage.per_app_secs.insert("game.exe".into(), 20 * 60); // normalized key
        // +30 granted → effective budget 150, used 47 → remaining 103.
        let s = today_summary(&rules, day(), 30, &usage);
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
        };
        let s = today_summary(&rules, day(), 0, &usage);
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
        };
        let s = today_summary(&rules, day(), 30, &usage); // 30 granted, but base is 0
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
        };
        let s = today_summary(&rules, day(), 0, &usage);
        assert_eq!(s["budget_mins"], 90);
        assert_eq!(s["remaining_mins"], 60);
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
