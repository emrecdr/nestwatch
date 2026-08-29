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
#[derive(Clone, Copy)]
pub enum Enforcer {
    Rules,
    Curfew,
}

static RULES_TICK: AtomicI64 = AtomicI64::new(0);
static CURFEW_TICK: AtomicI64 = AtomicI64::new(0);

fn cell(which: Enforcer) -> &'static AtomicI64 {
    match which {
        Enforcer::Rules => &RULES_TICK,
        Enforcer::Curfew => &CURFEW_TICK,
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
pub async fn tick(ticker: &mut tokio::time::Interval, which: Enforcer) {
    ticker.tick().await;
    beat(which);
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

        // Worst-of: pretend the rules enforcer died an hour ago.
        RULES_TICK.store(now_secs() - 3600, Ordering::Relaxed);
        let worst = worst_age_secs().expect("both have ticked");
        assert!(
            worst >= 3600,
            "a dead enforcer must dominate a healthy one, got {worst}"
        );
    }

    #[test]
    fn a_backwards_clock_reports_zero_not_a_negative_age() {
        // Stamp the future, as a clock stepping backwards would leave behind.
        CURFEW_TICK.store(now_secs() + 600, Ordering::Relaxed);
        assert_eq!(age_secs(Enforcer::Curfew), Some(0));
    }
}
