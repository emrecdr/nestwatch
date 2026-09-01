//! What this tool refused — counted where the refusal happens, persisted where the day is kept.
//!
//! # The question this exists to answer
//!
//! The audit log answers *"what did I do?"* precisely: all of its event kinds are the parent's own
//! actions plus authentication. Nothing answered *"has anything been pushed back against?"* — even
//! though this service detects and refuses several things a day and gets every one of them right.
//!
//! Three refusals were reachable only through `tracing::warn!`, which writes to a daily-rotated
//! file inside the ACL-hardened data directory. Reading it needs an Administrator console on the
//! child's PC, which is the one place a parent checking from their phone is not. So the refusals
//! were invisible in exactly the situation that produces them.
//!
//! # Why counters and not events
//!
//! Every one of these is **child-paced and unbounded**. A clock can be moved back and forth on a
//! timer; a cancelled shutdown can be cancelled again on the next thirty-second tick. Appending a
//! row per occurrence would hand the person being limited a way to rotate the history out — which
//! is the hazard `tests/audit_partition.rs` exists to police for the audit log and `O67` records
//! for rotation generally.
//!
//! A counter cannot be made to grow the file. [`Refused`] rides in the daily tally that is already
//! rewritten in place, and into the one rollup row per day that already exists, so a child
//! hammering any of these produces a larger *number* and not a larger *store*. The number is
//! itself the interesting part.
//!
//! # Why the counters are global and drained rather than passed down
//!
//! The two enforcers do not share state. Curfew has the usage *log*; the rules enforcer has the
//! persisted *tally*, and only it knows what day the counts belong to. Threading a handle from one
//! into the other would couple two loops that are deliberately independent — they may both request
//! a shutdown precisely because neither waits on the other.
//!
//! So each site increments a process-global counter and the rules enforcer drains all of them once
//! per tick into the day it is already keeping. [`drain`] uses `swap`, so a count is moved exactly
//! once and cannot be recorded twice by two readers or lost between them.
//!
//! # What is deliberately not counted here
//!
//! Signals whose meaning is ambiguous. An enforcer that stopped ticking might be tampering or
//! might be a Windows update; a machine that genuinely moved countries produces the same clock
//! reading as a child who tried it on. Everything below is a **refusal** — something this tool
//! actively declined to do — which is a fact about the tool's own behaviour and needs no guess
//! about intent. That distinction is the whole reason the card can be shown to the child as well
//! as the parent, and a feed mixing "we blocked this" with "this looked odd" would be the kind of
//! guard that cries wolf and then gets ignored.

use std::sync::atomic::{AtomicU32, Ordering};

use serde::{Deserialize, Serialize};

/// Distinct clock changes refused, day resets refused, shutdown cancellations answered.
static CLOCK_CHANGES: AtomicU32 = AtomicU32::new(0);
static DAY_RESETS: AtomicU32 = AtomicU32::new(0);
static SHUTDOWN_CANCELS: AtomicU32 = AtomicU32::new(0);

/// One day's refusals, as stored and as reported.
///
/// `u32` and saturating throughout. These are counts of deliberate acts by one person at one
/// keyboard, so the real ceiling is a few hundred; the saturation exists so a scripted loop
/// produces a large honest number instead of a wrapped small one, which would read as "nothing
/// happened" at precisely the moment something did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Refused {
    /// Times the system clock was moved to a *different* wrong value and ignored.
    ///
    /// Counted per distinct observed offset rather than per reading: `clock::now` is called many
    /// times a second and every one of them refuses, so counting readings would report the passage
    /// of time rather than the number of attempts.
    #[serde(default)]
    pub clock_changes: u32,
    /// Times a second day-rollover inside the minimum gap (`rules::MIN_RESET_GAP`, 12 h) was
    /// refused, each of which would have wiped the day's screen-time tally.
    ///
    /// Named in prose rather than linked: the constant is private to [`crate::rules`], and
    /// widening it to `pub` so a comment here could point at it would be the doc tail wagging the
    /// API dog.
    #[serde(default)]
    pub day_resets: u32,
    /// Times a pending shutdown was found cancelled and re-issued without a warning countdown.
    ///
    /// A standard user holds `SeShutdownPrivilege` and can run `shutdown /a`, so this is the one
    /// here that requires no settings screen at all — just a command. Counted by both enforcers.
    #[serde(default)]
    pub shutdown_cancels: u32,
}

impl Refused {
    /// Everything that happened, for the one-line summary.
    pub fn total(&self) -> u32 {
        self.clock_changes
            .saturating_add(self.day_resets)
            .saturating_add(self.shutdown_cancels)
    }

    /// Whether there is anything at all to show. The dashboard renders nothing when there is not,
    /// because a card reading "0, 0, 0" every day is a card that stops being read.
    pub fn any(&self) -> bool {
        self.total() > 0
    }

    /// Fold another day's counts in, saturating. Used to add a tick's drain to the stored day.
    pub fn merge(&mut self, other: Refused) {
        self.clock_changes = self.clock_changes.saturating_add(other.clock_changes);
        self.day_resets = self.day_resets.saturating_add(other.day_resets);
        self.shutdown_cancels = self.shutdown_cancels.saturating_add(other.shutdown_cancels);
    }
}

/// Saturating increment. `fetch_update` rather than `fetch_add` so the count sticks at the ceiling
/// instead of wrapping to zero.
fn bump(cell: &AtomicU32) {
    let _ = cell.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
        Some(n.saturating_add(1))
    });
}

/// A clock change was observed and ignored. Called from [`crate::clock`].
pub fn clock_change_rejected() {
    bump(&CLOCK_CHANGES);
}

/// A second day rollover inside the minimum gap was refused. Called from [`crate::rules`].
pub fn day_reset_refused() {
    bump(&DAY_RESETS);
}

/// A pending shutdown was found cancelled. Called from **both** enforcers, which is why it lives
/// here rather than on either one's state.
pub fn shutdown_cancel_seen() {
    bump(&SHUTDOWN_CANCELS);
}

/// Take everything counted since the last call, leaving the counters at zero.
///
/// `swap` rather than a read-then-clear: two reads cannot both see the same increment, and nothing
/// counted between the read and the clear is lost. There is exactly one caller in production — the
/// rules enforcer's tick — and that is the property this makes safe to rely on rather than merely
/// true today.
pub fn drain() -> Refused {
    Refused {
        clock_changes: CLOCK_CHANGES.swap(0, Ordering::Relaxed),
        day_resets: DAY_RESETS.swap(0, Ordering::Relaxed),
        shutdown_cancels: SHUTDOWN_CANCELS.swap(0, Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The counters are process-global, so any test touching them must hold this or it will race
    /// the others in the same binary and fail intermittently — which is worse than not testing.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn exclusive() -> std::sync::MutexGuard<'static, ()> {
        let g = LOCK.lock().unwrap_or_else(|p| p.into_inner());
        drain(); // start from a known floor whatever ran before
        g
    }

    #[test]
    fn a_drain_moves_each_count_exactly_once() {
        let _g = exclusive();
        clock_change_rejected();
        clock_change_rejected();
        day_reset_refused();
        shutdown_cancel_seen();

        let first = drain();
        assert_eq!(first.clock_changes, 2);
        assert_eq!(first.day_resets, 1);
        assert_eq!(first.shutdown_cancels, 1);
        assert_eq!(first.total(), 4);

        // The whole point of `swap`: a second reader gets nothing rather than the same counts
        // again, so a tick that runs twice cannot double the day's figure.
        assert_eq!(drain(), Refused::default());
    }

    #[test]
    fn nothing_happening_is_reported_as_nothing_rather_than_as_a_card() {
        let _g = exclusive();
        let quiet = drain();
        assert!(!quiet.any(), "a quiet day must not render a refusals card");
        assert_eq!(quiet.total(), 0);
    }

    /// A scripted loop must produce a big honest number, never a wrapped small one.
    ///
    /// Wrapping is the failure that matters here: `u32::MAX` refusals followed by one more would
    /// read as **zero**, which is indistinguishable from a quiet day — the exact collapse
    /// `screentime.rs` refuses to make between "not measured" and "nothing happened".
    #[test]
    fn counts_saturate_rather_than_wrapping_to_look_quiet() {
        let mut r = Refused {
            clock_changes: u32::MAX,
            day_resets: u32::MAX,
            shutdown_cancels: u32::MAX,
        };
        assert_eq!(r.total(), u32::MAX, "the summary must not wrap either");

        r.merge(Refused {
            clock_changes: 5,
            day_resets: 5,
            shutdown_cancels: 5,
        });
        assert_eq!(r.clock_changes, u32::MAX);
        assert!(r.any());
    }

    #[test]
    fn merging_adds_each_field_to_its_own_counterpart() {
        let mut r = Refused {
            clock_changes: 1,
            day_resets: 2,
            shutdown_cancels: 3,
        };
        r.merge(Refused {
            clock_changes: 10,
            day_resets: 20,
            shutdown_cancels: 30,
        });
        assert_eq!(
            r,
            Refused {
                clock_changes: 11,
                day_resets: 22,
                shutdown_cancels: 33,
            }
        );
        assert_eq!(r.total(), 66);
    }

    /// A stored tally written before this field existed must still parse, and must read as a day
    /// with nothing refused rather than failing the load — a load failure hands the child a zeroed
    /// budget, which is the expensive direction to be wrong in.
    #[test]
    fn a_tally_from_before_this_existed_reads_as_a_quiet_day() {
        let empty: Refused = serde_json::from_str("{}").expect("an absent field is a quiet day");
        assert_eq!(empty, Refused::default());
        let partial: Refused =
            serde_json::from_str(r#"{"clock_changes":3}"#).expect("a partial record parses");
        assert_eq!(partial.clock_changes, 3);
        assert_eq!(partial.day_resets, 0);
    }
}
