//! Proof that the enforcers are alive.
//!
//! Everything else in this tool can fail loudly. The enforcers can fail *quietly*: they're bare
//! `tokio::spawn`s whose `JoinHandle` is dropped, so if one panics or its loop exits, the service
//! keeps serving the dashboard and the dashboard keeps looking normal. A dead rules enforcer is
//! indistinguishable from "the child wasn't on the PC today" — both show zero minutes used.
//!
//! That's the worst failure this product can have, because the parent's belief that limits are
//! being enforced is the whole value. So each enforcer stamps the time of its most recent tick
//! here, and the dashboard shows how long ago that was. (Stamped at the *start* of a tick, not
//! the end — see [`beat`], which explains why that placement is load-bearing.)
//!
//! **This signal is reported, not acted on.** Nothing restarts a wedged enforcer; the parent has
//! to notice the dashboard or run `doctor`. Closing that loop is tracked as O4 in
//! `docs/OPEN-FINDINGS.md`.
//!
//! Deliberately a plain timestamp rather than a health *judgement*: the freshness threshold is a
//! presentation decision (the UI says "stale" past a couple of minutes) and putting it here would
//! bury it. Storing Unix seconds in an `AtomicI64` keeps this lock-free and callable from a tick
//! loop without touching `AppState`.

use std::sync::atomic::{AtomicI64, Ordering};

/// Which background loop reported in.
///
/// Not all of these enforce, which [`worst_age_secs`] is where it matters — see the note there
/// on why [`Enforcer::Probe`] is stamped like the other two and reported nothing like them.
#[derive(Clone, Copy)]
pub enum Enforcer {
    Rules,
    Curfew,
    /// The provider-probe scheduler (`crate::probe::run_scheduler`).
    Probe,
}

static RULES_TICK: AtomicI64 = AtomicI64::new(0);
static CURFEW_TICK: AtomicI64 = AtomicI64::new(0);
static PROBE_TICK: AtomicI64 = AtomicI64::new(0);

fn cell(which: Enforcer) -> &'static AtomicI64 {
    match which {
        Enforcer::Rules => &RULES_TICK,
        Enforcer::Curfew => &CURFEW_TICK,
        Enforcer::Probe => &PROBE_TICK,
    }
}

/// Wait for `ticker`'s next tick, then stamp the heartbeat for `which`.
///
/// The two are fused into one call **so the stamp cannot drift away from the await**. The
/// tempting change is to stamp at the *end* of a loop body instead, on the reasoning that a
/// heartbeat should prove a tick finished rather than merely started. That is a bug:
/// `run_rules_enforcer` has two early `continue` paths — the parent pressed **Pause**
/// (`TickMode::StandDown`) and a transient process-list failure — and stamping at the end would
/// skip both, so using Pause would make the dashboard report enforcement as **dead** within a
/// couple of minutes, every time. This shape leaves no end-of-body to move it to.
///
/// What it proves is therefore narrower than "a tick completed", but still the thing that
/// matters: the loop is scheduled and its timer is firing. A loop that has panicked (the release
/// build aborts, and the SCM restarts us), exited, or wedged inside a tick stops stamping, and
/// the age grows without bound — the silent-death case this module exists to surface. The cost
/// of stamping early is one tick of extra latency before a wedged tick reads as stale.
pub async fn tick(ticker: &mut tokio::time::Interval, which: Enforcer, wake: &mut Wake) {
    // Either the timer fired, or a parent changed something that affects enforcement.
    //
    // The second arm is what stops a pending shutdown outliving the decision to cancel it. Both
    // loops own a shutdown they may have to abort, and both learned about a cancellation only at
    // their next tick — up to `CHECK_INTERVAL` away, against a warning countdown that defaults to
    // 60 seconds. So a parent who extended bedtime, granted time, paused the rules or switched
    // curfew off in the last half-minute watched the machine power off anyway, having been told
    // the opposite. Waking on the change closes that window to a round trip.
    //
    // Safe to wake early on both loops, and worth stating because it would not be for a loop that
    // assumed its own cadence: `run_rules_enforcer` measures `now.duration_since(last_tick)` for
    // accrual rather than adding a fixed interval, and the curfew enforcer reasons about deadlines
    // rather than counting ticks. An extra evaluation costs one more pass over pure decision code.
    //
    // The stamp still happens on every path, so this cannot make a live loop look dead.
    tokio::select! {
        _ = ticker.tick() => {}
        changed = wake.changed() => {
            // The sender lives in `AppState`, which outlives these loops in the real server. If it
            // is ever dropped first, `changed()` returns `Err` immediately and forever — so fall
            // back to the timer rather than spinning on a closed channel.
            if changed.is_err() {
                ticker.tick().await;
            }
        }
    }
    beat(which);
}

/// The receiving half of "a parent changed something — re-evaluate now".
///
/// A `watch` rather than a `Notify`: each receiver tracks its own version, so a change published
/// while a loop is busy is still seen at its next await, and two loops both get it. A missed
/// notification here is a shutdown that proceeds after it was cancelled.
pub type Wake = tokio::sync::watch::Receiver<u64>;

/// The sending half. `send_modify` bumps a counter; the value carries no meaning beyond "changed".
pub type Waker = tokio::sync::watch::Sender<u64>;

/// Tell both enforcers to re-evaluate now rather than at their next tick.
pub fn wake(waker: &Waker) {
    waker.send_modify(|n| *n = n.wrapping_add(1));
}

/// Record that `which` reached the top of a tick. Private: reaching it only through [`tick`] is
/// what keeps the stamp welded to the await.
fn beat(which: Enforcer) {
    cell(which).store(now_secs(), Ordering::Relaxed);
}

/// Seconds since `which` last *reached* a tick, or `None` if it has never reported (the first
/// tick hasn't landed yet, or the loop died before reaching one).
///
/// "Reached", not "completed" — see [`tick`]. A loop wedged part-way through a tick keeps the
/// stamp it made on entry, so the age it reports lags the wedge by at most one interval.
pub fn age_secs(which: Enforcer) -> Option<i64> {
    let last = cell(which).load(Ordering::Relaxed);
    (last > 0).then(|| (now_secs() - last).max(0))
}

/// Seconds since the *least recently* seen enforcer ticked — the number to surface, since either
/// one being dead means limits aren't being applied.
///
/// **[`Enforcer::Probe`] is deliberately not in here**, and it is the only cell that is left out.
/// The number this returns is the one behind the dashboard's *enforcement alive* banner and
/// `doctor`'s enforcement check, and both sentences mean exactly one thing: **limits are not being
/// applied**. A dead probe scheduler does not stop a single limit — the budget, the blocklist,
/// per-app limits and curfew all keep running; what stops is the earning of *extra* time, which
/// fails toward the base budget by design (`crate::probe`). Folding it in would raise the alarm
/// that means "your child is unsupervised" for a condition that means "your child cannot earn a
/// bonus", and a banner that cries wolf is worth less than no banner. So it is stamped here, read
/// through [`age_secs`], and surfaced on the integrations card where the thing it describes lives.
/// Was `O102`; the argument for surfacing it *at all*, and for this exclusion, is in
/// `docs/PLUGIN-SYSTEM.md`, *The loop that says nothing when it dies*.
pub fn worst_age_secs() -> Option<i64> {
    match (age_secs(Enforcer::Rules), age_secs(Enforcer::Curfew)) {
        (Some(a), Some(b)) => Some(a.max(b)),
        // One silent is as bad as both; report whichever has spoken, so "never ticked" doesn't
        // masquerade as healthy.
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    }
}

/// Wall-clock seconds. Uses the system clock (not `Instant`) because the value is compared
/// against `now` in a later, unrelated call — but only ever as a *difference*, and it's clamped
/// at zero, so a clock that jumps backwards reports "just now" rather than a negative age.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These touch process-global state; keep them in one test so they can't interleave.
    #[test]
    fn beats_are_recorded_per_enforcer_and_the_worst_is_reported() {
        // Nothing has ticked yet in a fresh process — but other tests may have run first, so
        // assert the shape rather than the absence.
        beat(Enforcer::Rules);
        assert!(age_secs(Enforcer::Rules).is_some_and(|a| a < 5));

        beat(Enforcer::Curfew);
        assert!(age_secs(Enforcer::Curfew).is_some_and(|a| a < 5));

        // The three cells are distinct, which `cell` is the only place that can get wrong. Aged
        // through the statics and then stamped through `beat`, so the reading afterwards can only
        // come back young for the arm that was stamped: an arm pointing at a neighbour's cell
        // moves a number this checks. Writing the probe's own value in directly instead would
        // hide exactly that mutant — it did, until a probe aliased to `RULES_TICK` passed here
        // and failed in `tests/probe_runner.rs`.
        RULES_TICK.store(now_secs() - 3600, Ordering::Relaxed);
        CURFEW_TICK.store(now_secs() - 1800, Ordering::Relaxed);
        beat(Enforcer::Probe);
        assert!(age_secs(Enforcer::Probe).is_some_and(|a| a < 5));
        assert!(
            age_secs(Enforcer::Rules).is_some_and(|a| a >= 3600),
            "stamping the probe loop moved the rules cell — they are the same cell"
        );
        assert!(
            age_secs(Enforcer::Curfew).is_some_and(|a| a >= 1800),
            "stamping the probe loop moved the curfew cell — they are the same cell"
        );

        // Worst-of: the rules enforcer, dead an hour, dominates a curfew loop that is merely half
        // an hour behind.
        let worst = worst_age_secs().expect("both have ticked");
        assert!(
            worst >= 3600,
            "a dead enforcer must dominate a healthy one, got {worst}"
        );

        // And the probe cell reaches that number from neither direction. Both halves are asserted
        // because only the pair pins it: a `max` that folded the probe in would pass the first
        // with the cell fresh, and pass the second with it stale.
        assert_eq!(
            worst_age_secs(),
            Some(worst),
            "a probe loop that ticked a second ago must not make 'enforcement alive' read healthy"
        );
        PROBE_TICK.store(now_secs() - 86_400, Ordering::Relaxed);
        RULES_TICK.store(now_secs(), Ordering::Relaxed);
        CURFEW_TICK.store(now_secs(), Ordering::Relaxed);
        assert!(
            worst_age_secs().is_some_and(|a| a < 5),
            "with both enforcers healthy the banner must read healthy, whatever the probe loop \
             is doing"
        );

        // And the probe cell is the one the gate reads. Asked here because this is the one test
        // that may move these cells: `probe::scheduler_stale` could answer `false` for ever — no
        // gate ever lifted for a stopped scheduler — and nothing failed, since every other caller
        // takes the answer as a parameter.
        assert!(
            crate::probe::scheduler_stale(),
            "a probe loop silent for a day has stopped, and the gate must hear it"
        );
        beat(Enforcer::Probe);
        assert!(
            !crate::probe::scheduler_stale(),
            "and one that has just ticked has not"
        );
    }

    /// The point of the wake: a pending shutdown must not outlive the decision to cancel it.
    ///
    /// The interval here is five minutes, so if the wake arm did not exist this test would hang
    /// rather than fail — which is the honest shape for "the loop waited for its timer". Measured
    /// against the wall clock rather than a paused one because `tokio::time::pause` would make the
    /// five-minute interval fire instantly and prove nothing.
    #[tokio::test]
    async fn a_parent_change_ends_the_wait_immediately() {
        let (tx, mut rx) = tokio::sync::watch::channel(0u64);
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(300));
        ticker.tick().await; // `interval` yields its first tick immediately; consume it

        let started = std::time::Instant::now();
        wake(&tx);
        tick(&mut ticker, Enforcer::Curfew, &mut rx).await;
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the wake must end the wait, not the 300s timer: waited {:?}",
            started.elapsed()
        );
    }

    /// The failure mode the `is_err` arm exists for. A closed channel makes `changed()` return
    /// `Err` immediately and forever, so without the fallback the loop would spin at full speed —
    /// burning a core and calling the enforcement path thousands of times a second.
    #[tokio::test]
    async fn a_dropped_waker_falls_back_to_the_timer_rather_than_spinning() {
        let (tx, mut rx) = tokio::sync::watch::channel(0u64);
        drop(tx);
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(80));
        ticker.tick().await;

        let started = std::time::Instant::now();
        tick(&mut ticker, Enforcer::Curfew, &mut rx).await;
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(60),
            "a closed channel must leave the timer in charge, not return instantly: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_backwards_clock_reports_zero_not_a_negative_age() {
        // Stamp the future, as a clock stepping backwards would leave behind.
        CURFEW_TICK.store(now_secs() + 600, Ordering::Relaxed);
        assert_eq!(age_secs(Enforcer::Curfew), Some(0));
    }
}
