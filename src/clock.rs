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
//! **What the offset alone could not do.** The first version of this compared the OS *offset*
//! against the anchor and believed anything within an hour, so that a real DST transition still
//! worked. That bound was read — here and in `docs/SECURITY.md` — as "a child can gain at most an
//! hour". It is not what it bounded. True local time also leaves the anchor by an hour every
//! summer, legitimately, and the window was measured from the anchor rather than from the truth,
//! so the two effects add: an install anchored in winter could be pushed **two hours** in summer by
//! choosing UTC, which moves a 21:00 curfew to 23:00 every night from a settings page that raises
//! no prompt. The window could not simply be narrowed, either — an hour of slack is exactly what a
//! real DST transition needs.
//!
//! **What replaced it.** An offset is the *output* of a zone plus a date, and two zones share an
//! output for half the year; that ambiguity was the whole attack. The zone *identity* is the input,
//! and it is what the child actually changes. So the identity is recorded at install and compared
//! each tick:
//!
//! - **Identity matches** — believe the OS clock outright. DST is then exact rather than tolerated,
//!   because Windows is applying the real rules for the recorded zone. No window, no slack.
//! - **Identity differs** — the zone was changed under us. Fall back to UTC plus the highest offset
//!   seen while the identity still matched, which tracks the genuine DST excursion instead of
//!   freezing at the install-time offset.
//! - **Identity unavailable** (non-Windows, or a config written before this) — the offset tolerance
//!   above, unchanged, so nothing regresses.
//!
//! The identity includes the *Adjust for daylight saving time automatically* flag, because
//! unticking it shifts the offset an hour while leaving the zone name alone.
//!
//! With no anchor recorded (a config from before this existed, or dev runs) it degrades to plain
//! local time — the previous behavior — rather than guessing.

use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};

use chrono::{DateTime, FixedOffset, Local, NaiveDate, Offset, Utc};

/// Sentinel for "no anchor recorded" — a real offset is within ±14h of UTC.
const UNSET: i32 = i32::MIN;

/// The trusted UTC offset in minutes. Global because [`today`] is called from handlers, the
/// enforcers, and pure helpers that have no access to `AppState`; threading it through every one
/// would be far more invasive than the problem warrants.
static ANCHOR_MINS: AtomicI32 = AtomicI32::new(UNSET);

/// How far the OS offset may differ from the anchor and still be believed, when the zone
/// *identity* is unavailable (non-Windows, or a config written before it was recorded). One hour
/// covers every real DST transition; nothing legitimate moves a machine's offset further while it
/// sits on a desk at home.
///
/// **This tolerance is a fallback, not the main defence** — see [`decide`] for why it cannot be
/// the main defence. Where the identity is available it is not consulted at all.
const MAX_DRIFT_MINS: i32 = 60;

/// The time-zone identity recorded at install: what zone this machine *is in*, as opposed to what
/// offset it currently reports. `None` when unknown, which selects the [`MAX_DRIFT_MINS`] path.
static ANCHOR_ZONE: Mutex<Option<String>> = Mutex::new(None);

/// The largest offset seen while the zone identity still matched the recorded one.
///
/// This is what makes the tamper *fallback* correct rather than merely bounded. The anchor is a
/// single offset frozen at install; six months later the same zone is legitimately an hour off it,
/// so falling back to the anchor in summer hands back the hour the check just took away. The
/// high-water mark is the honest machine's own reading, so it tracks the real DST excursion — and
/// it only ever moves *up*, which is the direction that increases enforcement. A child cannot use
/// it: raising the offset brings curfew forward, and lowering it requires changing the zone, which
/// is the thing that stops the mark being updated at all.
static HIGH_WATER_MINS: AtomicI32 = AtomicI32::new(UNSET);

/// Record the trusted offset (called at startup from the saved config).
pub fn set_anchor(offset_mins: i32) {
    ANCHOR_MINS.store(offset_mins, Ordering::Relaxed);
    HIGH_WATER_MINS.store(offset_mins, Ordering::Relaxed);
}

/// Record the trusted zone identity (called at startup from the saved config, beside the anchor).
pub fn set_anchor_zone(zone: Option<String>) {
    *ANCHOR_ZONE.lock().unwrap_or_else(|p| p.into_inner()) = zone;
}

/// The machine's time-zone **identity** — which zone it is set to, not what offset that implies.
///
/// This is the whole point of the change. An offset is the *output* of a time zone plus the date;
/// two different zones share an offset for half the year, so an offset can never distinguish
/// "Amsterdam in winter" from "London in summer" — which is exactly the substitution a child makes.
/// The identity is the *input*, and changing it is the only way to move the clock without
/// `SeSystemtimePrivilege`. Comparing identities therefore catches the tamper directly instead of
/// inferring it from a number that has an innocent explanation.
///
/// `DynamicDaylightTimeDisabled` is folded in because unticking *Adjust for daylight saving time
/// automatically* moves the offset by an hour while leaving the key name alone — the same attack
/// one checkbox further down the same settings page. It is part of the identity, so it is part of
/// the comparison.
///
/// `None` means "cannot tell", never "unchanged": every caller treats it as absence of evidence
/// and falls back to the offset tolerance.
#[cfg(windows)]
pub fn current_zone_identity() -> Option<String> {
    use windows::Win32::System::Time::{
        DYNAMIC_TIME_ZONE_INFORMATION, GetDynamicTimeZoneInformation,
    };

    /// What `GetDynamicTimeZoneInformation` returns when it cannot answer. Spelled out here
    /// rather than imported so this file states its own failure condition.
    const TIME_ZONE_ID_INVALID: u32 = u32::MAX;

    let mut info = DYNAMIC_TIME_ZONE_INFORMATION::default();
    // SAFETY: the call writes one fully-owned `DYNAMIC_TIME_ZONE_INFORMATION` through the pointer
    // and reads nothing through it. `info` is a live local of exactly that type, so the pointer is
    // valid, correctly aligned and uniquely borrowed for the duration of the call. The struct is
    // plain data — no handles, no allocations — so there is nothing to free on either path.
    let id = unsafe { GetDynamicTimeZoneInformation(&mut info) };
    if id == TIME_ZONE_ID_INVALID {
        return None;
    }

    // A fixed-size UTF-16 buffer, NUL-terminated only when it is shorter than the buffer.
    let name = &info.TimeZoneKeyName;
    let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    let key = String::from_utf16_lossy(&name[..end]);
    if key.is_empty() {
        // Documented as "not always present". Absent is not the same as unchanged, so say so.
        return None;
    }
    Some(if info.DynamicDaylightTimeDisabled {
        format!("{key} (dst-off)")
    } else {
        key
    })
}

/// No identity available off Windows, so dev runs keep exactly the behaviour they had.
#[cfg(not(windows))]
pub fn current_zone_identity() -> Option<String> {
    None
}

/// What to believe this tick. Pure, so the entire decision table is testable on any OS — the
/// Windows-only half is two readings fed in as arguments.
///
/// # Why the offset tolerance cannot be the main defence
///
/// `MAX_DRIFT_MINS` bounds how far the *observed* offset may sit from the *anchor*. It was read as
/// "a child can gain at most an hour". That is not what it bounds. True local time also moves away
/// from the anchor — by an hour, every summer, legitimately — and the tolerance is measured from
/// the anchor rather than from the truth. So the reachable gain is
/// `(true_local - anchor) + MAX_DRIFT_MINS`:
///
/// | install season | season now | anchor | true local | child picks | clock reads | gained |
/// |---|---|---|---|---|---|---|
/// | winter | winter | +60 | +60 | 0 (UTC) | 1h early | **60 min** |
/// | winter | summer | +60 | +120 | 0 (UTC) | 2h early | **120 min** |
///
/// Two hours is enough to make a 21:00 curfew fire at 23:00, every night of the summer, from a
/// settings page that needs no administrator. Comparing identities instead removes the substitution
/// entirely: there is no zone the child can pick that is still the recorded one.
fn decide(
    anchor: i32,
    observed: i32,
    recorded_zone: Option<&str>,
    current_zone: Option<&str>,
    high_water: i32,
) -> Trust {
    match (recorded_zone, current_zone) {
        // The machine is in the zone it was installed in. Believe its clock outright — DST is then
        // exact rather than tolerated, because the OS is applying the real rules for that zone.
        (Some(recorded), Some(current)) if recorded == current => Trust::Os { record: true },
        // The zone was changed under us. Nothing legitimate does this while a PC sits on a desk.
        (Some(_), Some(_)) => Trust::Anchored(high_water.max(anchor)),
        // No identity to compare — the pre-existing behaviour, unchanged.
        _ => {
            if (observed - anchor).abs() <= MAX_DRIFT_MINS {
                Trust::Os { record: false }
            } else {
                Trust::Anchored(anchor)
            }
        }
    }
}

/// The outcome of [`decide`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trust {
    /// Believe the OS clock. `record` is set only when the zone identity vouched for it, and is
    /// what may advance [`HIGH_WATER_MINS`] — a reading admitted by the offset tolerance alone has
    /// not been vouched for by anything and must never move the mark.
    Os { record: bool },
    /// Express `Utc::now()` at this offset instead.
    Anchored(i32),
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
    if anchor == UNSET {
        return Local::now().fixed_offset();
    }
    let observed = current_offset_mins();
    let current = current_zone_identity();
    let recorded = ANCHOR_ZONE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    let high_water = HIGH_WATER_MINS.load(Ordering::Relaxed);

    match decide(
        anchor,
        observed,
        recorded.as_deref(),
        current.as_deref(),
        high_water,
    ) {
        Trust::Os { record } => {
            if record {
                HIGH_WATER_MINS.fetch_max(observed, Ordering::Relaxed);
            }
            Local::now().fixed_offset()
        }
        Trust::Anchored(offset) => {
            // Tampering (or a genuinely relocated machine — the parent can re-anchor by
            // reinstalling). Log sparsely: this runs on every 30s tick while the offset stays wrong.
            log_tamper(observed, offset);
            let tz = FixedOffset::east_opt(offset * 60).unwrap_or(Utc.fix());
            Utc::now().with_timezone(&tz)
        }
    }
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
        /// Restored alongside the anchor. `set_anchor` writes both, so a test that leaves this
        /// behind changes what the *next* test's tamper fallback returns — the same cross-test
        /// leak the gate below exists to prevent, one static further along.
        high_water: i32,
        zone: Option<String>,
        _gate: std::sync::MutexGuard<'static, ()>,
    }
    impl Drop for Restore {
        fn drop(&mut self) {
            ANCHOR_MINS.store(self.anchor, Ordering::Relaxed);
            HIGH_WATER_MINS.store(self.high_water, Ordering::Relaxed);
            *ANCHOR_ZONE.lock().unwrap_or_else(|p| p.into_inner()) = self.zone.take();
        }
    }
    fn guard() -> Restore {
        static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
        // A panicking test poisons the mutex; take it anyway, or one failure cascades into
        // every other test in this module reporting a poisoned lock instead of its own result.
        let _gate = GATE.lock().unwrap_or_else(|p| p.into_inner());
        Restore {
            anchor: ANCHOR_MINS.load(Ordering::Relaxed),
            high_water: HIGH_WATER_MINS.load(Ordering::Relaxed),
            zone: ANCHOR_ZONE
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone(),
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

    // ---------------------------------------------------------------------------------------
    // Zone identity. These drive `decide` directly rather than `now()`: the identity is only
    // readable on Windows, so `now()` on a dev machine always takes the fallback arm and could
    // never exercise the arms that matter. Feeding the two readings in as arguments is what makes
    // the whole decision table testable on any OS — the untestable part is then one Win32 call
    // that returns a string.
    // ---------------------------------------------------------------------------------------

    /// Amsterdam. Winter is UTC+1, summer UTC+2.
    const WINTER: i32 = 60;
    const SUMMER: i32 = 120;
    const HOME: &str = "W. Europe Standard Time";

    /// How far the clock can be pushed *earlier* than the truth, in minutes. Earlier is the only
    /// direction worth anything to a child: it delays curfew.
    ///
    /// Mirrors what `now()` does with each outcome, so the number is the one the enforcers would
    /// actually see rather than a restatement of `decide`'s return value.
    fn minutes_gained(
        anchor: i32,
        true_local: i32,
        picked: i32,
        recorded: Option<&str>,
        current: Option<&str>,
        high_water: i32,
    ) -> i32 {
        let effective = match decide(anchor, picked, recorded, current, high_water) {
            Trust::Os { .. } => picked,
            Trust::Anchored(offset) => offset,
        };
        true_local - effective
    }

    /// The bug this mechanism exists for, kept as an executable statement of it.
    ///
    /// Anchored in winter, exploited in summer, by selecting plain UTC: the offset check saw a
    /// 60-minute deviation, which was inside its tolerance, and handed back two hours.
    #[test]
    fn choosing_utc_in_summer_used_to_buy_two_hours_and_now_buys_none() {
        // What the offset-only check did. Reproduced through the fallback arm, which still
        // implements exactly that rule — so this is the real predicate, not a model of it.
        let old = minutes_gained(WINTER, SUMMER, 0, None, None, WINTER);
        assert_eq!(
            old, 120,
            "the offset-only rule is what it always was; if this changed, the fallback arm moved"
        );

        // With the identity recorded, picking any other zone is simply not the recorded zone.
        let now = minutes_gained(WINTER, SUMMER, 0, Some(HOME), Some("UTC"), SUMMER);
        assert_eq!(now, 0, "a changed zone must gain the child nothing");
    }

    /// The same lever in winter, which is the milder version of the same bug.
    #[test]
    fn choosing_utc_in_winter_gains_nothing_either() {
        assert_eq!(minutes_gained(WINTER, WINTER, 0, None, None, WINTER), 60);
        assert_eq!(
            minutes_gained(WINTER, WINTER, 0, Some(HOME), Some("UTC"), WINTER),
            0
        );
    }

    /// The reason the offset could never carry this on its own: a substituted zone can present an
    /// offset that is indistinguishable from an honest one. Only the identity separates them.
    #[test]
    fn a_changed_zone_is_caught_even_when_its_offset_looks_innocent() {
        // London in summer is +60 — numerically identical to Amsterdam in winter, and well inside
        // MAX_DRIFT_MINS of the anchor. The offset rule cannot see anything wrong here.
        assert_eq!(
            decide(WINTER, WINTER, None, None, WINTER),
            Trust::Os { record: false },
            "precondition: the offset rule finds this innocent"
        );
        assert_eq!(
            decide(
                WINTER,
                WINTER,
                Some(HOME),
                Some("GMT Standard Time"),
                WINTER
            ),
            Trust::Anchored(WINTER),
            "the identity must catch what the offset cannot"
        );
    }

    /// DST must keep working, and with the identity present it works *exactly* — the hour of slack
    /// is not consulted at all, because Windows is applying the recorded zone's own rules.
    #[test]
    fn a_matching_zone_is_believed_straight_through_a_dst_transition() {
        for observed in [WINTER, SUMMER] {
            assert_eq!(
                decide(WINTER, observed, Some(HOME), Some(HOME), WINTER),
                Trust::Os { record: true },
                "the machine is in the zone it was installed in; its clock is the authority"
            );
        }
        // And an offset far outside the old tolerance is still believed when the zone agrees,
        // which is what makes this exact rather than tolerant. (Lord Howe shifts 30; a machine
        // legitimately re-homed by the parent shifts more.)
        assert_eq!(
            decide(WINTER, WINTER + 8 * 60, Some(HOME), Some(HOME), WINTER),
            Trust::Os { record: true }
        );
    }

    /// Unticking "adjust for daylight saving time automatically" is the same attack one checkbox
    /// further down the page: the zone name does not move, the offset does.
    #[test]
    fn disabling_automatic_dst_counts_as_a_different_zone() {
        assert_eq!(
            decide(
                WINTER,
                WINTER,
                Some(HOME),
                Some("W. Europe Standard Time (dst-off)"),
                SUMMER
            ),
            Trust::Anchored(SUMMER),
            "the DST flag is part of the identity, so flipping it is a zone change"
        );
    }

    /// Why the fallback uses the high-water mark and not the anchor: the anchor is frozen at
    /// install, so in summer it is itself an hour behind the truth and would refund the hour the
    /// identity check just took away.
    #[test]
    fn the_tamper_fallback_follows_dst_instead_of_freezing_at_install() {
        let with_mark = minutes_gained(WINTER, SUMMER, 0, Some(HOME), Some("UTC"), SUMMER);
        let without = minutes_gained(WINTER, SUMMER, 0, Some(HOME), Some("UTC"), WINTER);
        assert_eq!(with_mark, 0);
        assert_eq!(
            without, 60,
            "anchoring to the install-time offset would still leak an hour in summer"
        );
    }

    /// The mark must never be advanced by a reading the identity did not vouch for — otherwise the
    /// fallback arm could be walked forward by the very tampering it exists to answer.
    #[test]
    fn only_a_vouched_reading_may_move_the_high_water_mark() {
        assert_eq!(
            decide(WINTER, SUMMER, None, None, WINTER),
            Trust::Os { record: false },
            "the offset tolerance admits this reading but vouches for nothing"
        );
        assert_eq!(
            decide(WINTER, SUMMER, Some(HOME), Some(HOME), WINTER),
            Trust::Os { record: true }
        );
    }

    /// A config written before the identity existed must behave exactly as it did before.
    #[test]
    fn a_half_known_identity_falls_back_rather_than_guessing() {
        for pair in [(Some(HOME), None), (None, Some(HOME)), (None, None)] {
            assert_eq!(
                decide(WINTER, WINTER, pair.0, pair.1, WINTER),
                Trust::Os { record: false },
                "one-sided evidence is no evidence; the old tolerance decides"
            );
            assert_eq!(
                decide(WINTER, WINTER + 13 * 60, pair.0, pair.1, WINTER),
                Trust::Anchored(WINTER),
                "and it still rejects a jump outside the tolerance"
            );
        }
    }

    #[test]
    fn recording_an_anchor_seeds_the_high_water_mark() {
        let _g = guard();
        set_anchor(WINTER);
        assert_eq!(HIGH_WATER_MINS.load(Ordering::Relaxed), WINTER);
    }
}
