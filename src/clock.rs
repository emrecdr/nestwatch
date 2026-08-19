//! Tamper-resistant local time.
//!
//! Everything time-based here — the daily budget reset and the curfew window — keyed off
//! `chrono::Local::now()`, which reads the OS timezone. On Windows the *Change the time zone*
//! right (`SeTimeZonePrivilege`) is granted to the **Users** group by default: a standard user
//! changes it from Settings with no UAC prompt. That made two total bypasses available in four
//! clicks:
//!
//! - Flipping between two zones ≥24h apart changes the local **date** on every flip, and the
//!   usage tally resets whenever the date differs — so the whole day's budget could be reset
//!   every 30 seconds, indefinitely.
//! - The same lever moves wall-clock time out of the curfew window, which makes the enforcer
//!   cancel a pending shutdown.
//!
//! **The fix.** Anchor to UTC — which that privilege cannot move; changing the *clock* needs
//! `SeSystemtimePrivilege`, which standard users don't hold — plus an offset recorded at install.
//!
//! The OS offset is still honoured while it stays within an hour of the anchor, so a genuine DST
//! transition just works. Anything larger is treated as tampering: ignored, and logged once.
//! Crucially the comparison is always against the **stored anchor**, never against the last
//! observed value, so the deviation a child can induce is bounded at one hour and cannot be
//! walked forward step by step.
//!
//! With no anchor recorded (a config from before this existed, or dev runs) it degrades to plain
//! local time — the previous behavior — rather than guessing.

use std::sync::atomic::{AtomicI32, Ordering};

use chrono::{DateTime, FixedOffset, Local, NaiveDate, Offset, Utc};

/// Sentinel for "no anchor recorded" — a real offset is within ±14h of UTC.
const UNSET: i32 = i32::MIN;

/// The trusted UTC offset in minutes. Global because [`today`] is called from handlers, the
/// enforcers, and pure helpers that have no access to `AppState`; threading it through every one
/// would be far more invasive than the problem warrants.
static ANCHOR_MINS: AtomicI32 = AtomicI32::new(UNSET);

/// How far the OS offset may differ from the anchor and still be believed. One hour covers every
/// real DST transition; nothing legitimate moves a machine's offset further while it sits on a
/// desk at home.
const MAX_DRIFT_MINS: i32 = 60;

/// Record the trusted offset (called at startup from the saved config).
pub fn set_anchor(offset_mins: i32) {
    ANCHOR_MINS.store(offset_mins, Ordering::Relaxed);
}

/// The machine's current UTC offset in minutes, for recording an anchor at install time.
pub fn current_offset_mins() -> i32 {
    Local::now().offset().fix().local_minus_utc() / 60
}

/// Whether an anchor is in force (used by diagnostics).
pub fn anchored() -> bool {
    ANCHOR_MINS.load(Ordering::Relaxed) != UNSET
}

/// Local time we're willing to act on.
///
/// Returns the OS local time when it agrees with the anchor to within [`MAX_DRIFT_MINS`], and the
/// anchored time otherwise. Falls back to plain local time when no anchor is set.
pub fn now() -> DateTime<FixedOffset> {
    let anchor = ANCHOR_MINS.load(Ordering::Relaxed);
    let observed = current_offset_mins();
    if anchor == UNSET {
        return Local::now().fixed_offset();
    }
    if (observed - anchor).abs() <= MAX_DRIFT_MINS {
        return Local::now().fixed_offset();
    }
    // Tampering (or a genuinely relocated machine — the parent can re-anchor by reinstalling).
    // Log sparsely: this runs on every 30s tick while the offset stays wrong.
    log_tamper(observed, anchor);
    let tz = FixedOffset::east_opt(anchor * 60).unwrap_or(Utc.fix());
    Utc::now().with_timezone(&tz)
}

/// Today's date in trusted local time. The day key for the budget, grants, and usage history.
pub fn today() -> NaiveDate {
    now().date_naive()
}

/// One warning per distinct observed offset, so a tampered clock doesn't spam the service log
/// twice a minute forever.
fn log_tamper(observed: i32, anchor: i32) {
    static LAST_LOGGED: AtomicI32 = AtomicI32::new(UNSET);
    if LAST_LOGGED.swap(observed, Ordering::Relaxed) != observed {
        tracing::warn!(
            "system timezone offset is {observed} min but this install is anchored to {anchor} \
             min — ignoring the change and using the anchored time (screen-time limits and \
             curfew are unaffected)"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes the tests in this module, and restores the anchor afterwards.
    ///
    /// Both halves are needed and only the second used to be here. `ANCHOR_MINS` is
    /// process-global and the test harness runs these on parallel threads, so restoring on drop
    /// only undoes leakage *after* a test — it provides no exclusion *during* one. Two tests could
    /// interleave between one's `set_anchor` and its `now()`, and the reader would see the other's
    /// anchor. That raced at about 1 run in 20: green locally, and eventually red in CI on a
    /// commit that had nothing to do with the clock.
    ///
    /// (`heartbeat.rs` has the same hazard and solves it the other way — by keeping all its
    /// global-touching assertions inside a single test.)
    struct Restore {
        anchor: i32,
        _gate: std::sync::MutexGuard<'static, ()>,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            ANCHOR_MINS.store(self.anchor, Ordering::Relaxed);
        }
    }
    fn guard() -> Restore {
        static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
        // A panicking test poisons the mutex; take it anyway, or one failure cascades into
        // every other test in this module reporting a poisoned lock instead of its own result.
        let _gate = GATE.lock().unwrap_or_else(|p| p.into_inner());
        Restore {
            anchor: ANCHOR_MINS.load(Ordering::Relaxed),
            _gate,
        }
    }

    #[test]
    fn without_an_anchor_it_is_plain_local_time() {
        let _g = guard();
        ANCHOR_MINS.store(UNSET, Ordering::Relaxed);
        assert!(!anchored());
        assert_eq!(now().date_naive(), Local::now().date_naive());
    }

    #[test]
    fn an_agreeing_offset_is_believed() {
        let _g = guard();
        set_anchor(current_offset_mins());
        assert!(anchored());
        assert_eq!(now().date_naive(), Local::now().date_naive());
    }

    /// A DST shift (±60 min) must still be honoured — the machine really did change offset.
    #[test]
    fn a_dst_sized_shift_is_still_believed() {
        let _g = guard();
        for shift in [-60, 60] {
            set_anchor(current_offset_mins() + shift);
            assert_eq!(
                now().offset().local_minus_utc() / 60,
                current_offset_mins(),
                "a {shift}-minute difference is DST-sized and must be trusted"
            );
        }
    }

    /// The attack: a large offset jump must be ignored, and the anchored offset used instead.
    #[test]
    fn a_large_offset_jump_is_ignored() {
        let _g = guard();
        // Pretend the machine was installed 12 hours away from where it now claims to be.
        let anchor = current_offset_mins() + 12 * 60;
        set_anchor(anchor);
        assert_eq!(
            now().offset().local_minus_utc() / 60,
            anchor,
            "a 12-hour jump is tampering; the anchored offset must win"
        );
    }

    /// Bounded, not cumulative: whatever the OS claims, the result is always within one hour of
    /// the anchor — so repeated shifts can't walk the clock across a day boundary.
    #[test]
    fn drift_is_bounded_by_the_anchor_not_the_previous_reading() {
        let _g = guard();
        let os = current_offset_mins();
        for jump in [2 * 60, 6 * 60, 13 * 60, -13 * 60] {
            set_anchor(os - jump); // anchor far from the OS's claim
            let effective = now().offset().local_minus_utc() / 60;
            let anchor = os - jump;
            assert!(
                (effective - anchor).abs() <= MAX_DRIFT_MINS,
                "effective offset {effective} strayed more than an hour from anchor {anchor}"
            );
        }
    }
}
