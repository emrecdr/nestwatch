//! Advance "your time is nearly up" warnings for the child.
//!
//! Until this existed, the only child-facing notification fired when the limit was *already*
//! spent — `warn_secs` is a grace period **after** exhaustion, not a heads-up before it (see
//! `RulesEnforcer::decide`, where it becomes `budget_deadline = now + warn`). So the first thing
//! the child knew about the budget was the screen locking. That reads as arbitrary, and an
//! enforcement tool the child experiences as arbitrary is one they spend their time defeating.
//!
//! Both enforcers tick every 30s and can say how many minutes are left — screen-time budget
//! remaining, or minutes until the curfew window opens — so the shared part is just *when to
//! speak up*: exactly once per threshold, on the way down, without re-announcing after a lock,
//! a service restart, or a grant. That decision is what lives here. Pure, so every one of those
//! cases is a unit test rather than something you discover on a child's laptop.

/// Remaining-time thresholds (minutes) at which the child gets a heads-up.
///
/// **Descending** — [`Countdown::observe`] walks this in reverse to announce the most urgent
/// threshold a tick crossed. Fixed rather than configurable: a first-time parent has enough to
/// decide already, and 15/5/1 is easy to change here if it turns out wrong in practice.
///
/// **Caller constraint:** whoever feeds a countdown must observe *more often than the smallest
/// gap between thresholds* — one minute, as written. A slower loop steps over a threshold without
/// ever seeing a value at or below it while the previous reading was above it, and that warning
/// silently never fires. Both enforcers tick at 30s, which satisfies this with a factor of two to
/// spare; shrinking the smallest threshold or lengthening a tick interval breaks it.
pub const WARN_AT_MINS: [u32; 3] = [15, 5, 1];

/// The furthest-out threshold — how far ahead a caller needs to be able to see. Callers that
/// measure a *future* event (curfew) only have to look this far ahead.
pub const LOOKAHEAD_MINS: u32 = WARN_AT_MINS[0];

/// Tracks the last observed "minutes remaining" so a threshold is announced on the way down and
/// only once.
#[derive(Debug, Default)]
pub struct Countdown {
    /// Minutes remaining at the previous observation. `None` means unprimed — either the first
    /// observation ever, or after a [`Countdown::reset`].
    last: Option<u32>,
}

impl Countdown {
    /// Feed this tick's minutes remaining; returns the threshold to announce, if this tick
    /// crossed one going **down**.
    ///
    /// The single rule — announce `t` when `previous > t >= remaining` — is what makes every
    /// awkward case fall out for free, with no per-threshold bookkeeping to get wrong:
    ///
    /// * **First observation primes silently.** With no previous value there is no crossing, so
    ///   a service restarted with 4 minutes left doesn't claim "15 minutes"; it stays quiet and
    ///   still announces 1 on the way past.
    /// * **A budget shorter than a threshold never announces it.** A 10-minute day was never
    ///   above 15, so 15 can't be crossed.
    /// * **Granting time re-arms it.** Remaining rises, which is not a crossing; the next descent
    ///   through 15 announces again. No "episode" concept and nothing to reset.
    /// * **Idle ticks are silent.** A locked screen accrues nothing, so remaining doesn't move.
    /// * **One message per tick.** Reverse (ascending) order picks the *most urgent* threshold
    ///   crossed, so a tick that jumps 20 → 3 announces 5 rather than firing three popups or
    ///   announcing the stalest one.
    ///
    /// The announced value is the threshold, not `remaining`. Normally they're equal; they differ
    /// only when a tick skips past a threshold (a long stall, or a parent shrinking the budget
    /// below where the child already is), where the message can overstate by a couple of minutes
    /// once before the enforcer's own at-zero warning takes over.
    pub fn observe(&mut self, remaining: u32) -> Option<u32> {
        let previous = self.last.replace(remaining)?;
        WARN_AT_MINS
            .into_iter()
            .rev()
            .find(|&t| previous > t && remaining <= t)
    }

    /// Feed a reading for a **future** event, where `None` means "further off than
    /// [`LOOKAHEAD_MINS`]" rather than "nothing to count down to".
    ///
    /// Callers measuring something ahead of them (curfew) can only see so far, so they need a
    /// value for "not yet visible" that is nonetheless *above* every threshold — otherwise the
    /// first tick that brings the event into view is a first observation, primes silently, and
    /// the top warning never fires at all. Parking one minute past the horizon makes that tick a
    /// genuine downward crossing.
    ///
    /// That sentinel lives here rather than at the call site so it is pinned by this module's own
    /// tests, beside the threshold list it has to stay above. Reconstructing it from
    /// `LOOKAHEAD_MINS + 1` in a caller works right up until someone changes the thresholds.
    pub fn observe_upcoming(&mut self, mins_until: Option<u32>) -> Option<u32> {
        self.observe(mins_until.unwrap_or(LOOKAHEAD_MINS + 1))
    }

    /// Nothing to count down to right now — an unlimited day, curfew switched off, or a limit
    /// already spent. Re-primes, so turning a limit back on can't announce a threshold the child
    /// never actually crossed on the way down.
    pub fn reset(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive a countdown over a sequence of observations, collecting what it announced.
    fn run(remaining: &[u32]) -> Vec<u32> {
        let mut c = Countdown::default();
        remaining.iter().filter_map(|&m| c.observe(m)).collect()
    }

    #[test]
    fn first_observation_primes_without_announcing() {
        let mut c = Countdown::default();
        assert_eq!(c.observe(3), None, "nothing to compare against yet");
    }

    #[test]
    fn announces_each_threshold_once_on_the_way_down() {
        let ticks: Vec<u32> = (0..=20).rev().collect(); // 20, 19, ... 1, 0
        assert_eq!(run(&ticks), vec![15, 5, 1]);
    }

    #[test]
    fn repeated_observations_at_a_threshold_do_not_re_announce() {
        // The machine sits idle at 5 minutes remaining: nothing accrues, so nothing is announced
        // after the first crossing.
        assert_eq!(run(&[6, 5, 5, 5, 5]), vec![5]);
    }

    /// A budget smaller than a threshold must never announce it — the child was never above it.
    #[test]
    fn a_short_budget_skips_thresholds_above_it() {
        // A 10-minute day: 15 is never crossed, 5 and 1 are.
        assert_eq!(run(&[10, 9, 8, 7, 6, 5, 4, 3, 2, 1]), vec![5, 1]);
    }

    /// Restarting the service mid-day must not replay warnings the child already passed.
    #[test]
    fn a_restart_partway_down_skips_the_thresholds_already_passed() {
        // The service comes back with 4 minutes left: 15 and 5 are behind us, 1 still ahead.
        assert_eq!(run(&[4, 3, 2, 1, 0]), vec![1]);
    }

    /// Granting more time must re-arm the whole ladder, with no explicit reset anywhere.
    #[test]
    fn granting_time_re_arms_the_thresholds() {
        let mut c = Countdown::default();
        let mut announced = Vec::new();
        // Count down to 3 minutes...
        for m in (3..=20).rev() {
            announced.extend(c.observe(m));
        }
        assert_eq!(announced, vec![15, 5], "warned on the way down");
        // ...then the parent grants 30 more minutes.
        announced.clear();
        for m in (0..=33).rev() {
            announced.extend(c.observe(m));
        }
        assert_eq!(
            announced,
            vec![15, 5, 1],
            "a grant lifts the child back above the thresholds, so they must warn again"
        );
    }

    /// A tick that skips several thresholds at once announces the most urgent one, not the
    /// stalest — and only one message, so the child never gets a burst of popups.
    #[test]
    fn a_jump_past_several_thresholds_announces_only_the_most_urgent() {
        assert_eq!(
            run(&[20, 3]),
            vec![5],
            "3 minutes left is not a 15-minute warning"
        );
        assert_eq!(run(&[20, 0]), vec![1]);
    }

    /// `reset` must re-prime, not merely clear: after it, the next observation is a first
    /// observation. This is what stops "unlimited today, then a 10-minute budget" from
    /// announcing a 15-minute warning the child never crossed.
    #[test]
    fn reset_re_primes() {
        let mut c = Countdown::default();
        assert_eq!(c.observe(100), None);
        c.reset();
        assert_eq!(
            c.observe(10),
            None,
            "post-reset is a fresh first observation"
        );
        assert_eq!(
            c.observe(9),
            None,
            "10 → 9 crosses nothing: 15 was never above us"
        );
        assert_eq!(c.observe(5), Some(5));
    }

    /// Crossing *upward* is never an announcement — only descent counts.
    #[test]
    fn rising_remaining_never_announces() {
        assert_eq!(run(&[1, 5, 15, 20]), Vec::<u32>::new());
    }

    /// A future event that is out of sight must still announce the top threshold the moment it
    /// comes into view.
    ///
    /// This is the invariant `observe_upcoming` exists to own. If "beyond the horizon" re-primed
    /// (or parked *at* the top threshold rather than above it), the first visible tick would be a
    /// first observation, cross nothing, and the 15-minute warning would never fire at all — with
    /// no error and no failing test anywhere else.
    #[test]
    fn a_future_event_beyond_the_horizon_announces_on_arrival() {
        let mut c = Countdown::default();
        for _ in 0..5 {
            assert_eq!(c.observe_upcoming(None), None, "still out of sight");
        }
        assert_eq!(
            c.observe_upcoming(Some(LOOKAHEAD_MINS)),
            Some(LOOKAHEAD_MINS),
            "coming into view must be a real downward crossing of the top threshold"
        );
    }

    /// A reading inside the horizon behaves exactly like [`Countdown::observe`] — `observe_upcoming`
    /// only reinterprets `None`.
    #[test]
    fn observe_upcoming_passes_visible_readings_straight_through() {
        let mut c = Countdown::default();
        c.observe_upcoming(Some(7));
        assert_eq!(c.observe_upcoming(Some(5)), Some(5));
    }

    /// `LOOKAHEAD_MINS` must stay pinned to the furthest-out threshold: curfew only looks that
    /// far ahead, so if the two drifted apart its 15-minute warning would silently stop firing.
    #[test]
    fn lookahead_covers_every_threshold() {
        assert_eq!(LOOKAHEAD_MINS, *WARN_AT_MINS.iter().max().unwrap());
        assert!(
            WARN_AT_MINS.windows(2).all(|w| w[0] > w[1]),
            "WARN_AT_MINS must stay strictly descending — `observe` reverses it to find the \
             most urgent crossing"
        );
    }
}
