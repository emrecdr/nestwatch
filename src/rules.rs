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

use chrono::NaiveDate;
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

/// Persisted rule settings (a `Config` field). All defaulted so legacy configs still load.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Rules {
    /// Minutes of allowed use per day (`0` = no budget).
    #[serde(default)]
    pub daily_budget_mins: u32,
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

impl Rules {
    /// Whether anything is configured — lets the loop skip the process scan when idle.
    pub fn any_configured(&self) -> bool {
        self.daily_budget_mins > 0 || !self.blocklist.is_empty() || !self.app_limits.is_empty()
    }

    /// Validate (at config load and on POST). Fail-open like curfew: only the warning bound.
    pub fn validate(&self) -> Result<(), String> {
        if self.warn_secs > MAX_WARN_SECS {
            return Err(format!("warning seconds must be <= {MAX_WARN_SECS}"));
        }
        Ok(())
    }

    /// Today's effective daily budget in minutes: the base plus any granted extra. One home for
    /// the "base + extra" formula so the enforcer and its logging can't drift.
    pub fn effective_budget_mins(&self, extra: u32) -> u32 {
        self.daily_budget_mins + extra
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
pub fn today_summary(rules: &Rules, extra: u32, usage: &Usage) -> serde_json::Value {
    let budget = rules.effective_budget_mins(extra);
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
        if rules.daily_budget_mins > 0 && t.active {
            let budget_secs = rules.effective_budget_mins(t.extra_minutes) as u64 * 60;
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

        let budget = rules.effective_budget_mins(extra);

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
        // parent grants more time, lifting the child back under budget. The trigger is the
        // budget itself (`shutdown_wanted`), not the countdown deadline: merely locking the
        // screen or stepping away while still over budget must NOT rescue an in-flight shutdown.
        // Curfew is read only on the potential falling edge, and the actual decision goes through
        // the pure `should_abort_budget_shutdown` so curfew stays the sole authority over its own
        // shutdowns.
        let over_budget = rules.daily_budget_mins > 0
            && enforcer.usage.total_secs >= rules.effective_budget_mins(extra) as u64 * 60;
        let shutdown_wanted = over_budget && rules.budget_action == EnforceAction::Shutdown;
        let curfew_active = if prev_shutdown_wanted && !shutdown_wanted {
            let guard = crate::state::recover_read(&config);
            guard.curfew.enabled && guard.curfew.is_active_now()
        } else {
            false
        };
        if should_abort_budget_shutdown(prev_shutdown_wanted, shutdown_wanted, curfew_active) {
            let control = control.clone();
            if let Ok(Err(e)) = tokio::task::spawn_blocking(move || control.abort_shutdown()).await
            {
                tracing::warn!(error = %e, "failed to abort budget shutdown after budget cleared");
            }
            usage_log.record(
                "budget_shutdown_aborted",
                serde_json::json!({ "minutes_used": used_mins, "budget": budget }),
            );
        }
        prev_shutdown_wanted = shutdown_wanted;
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
        let s = today_summary(&rules, 30, &usage);
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
        let s = today_summary(&rules, 0, &usage);
        assert_eq!(s["budget_mins"], 0);
        assert_eq!(s["used_mins"], 90);
        assert!(s["remaining_mins"].is_null());
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
