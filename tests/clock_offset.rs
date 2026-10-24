//! The offset `install` records as the anchor is counted in minutes.
//!
//! Its own binary because it sets `TZ`, which is process-wide. Unix only: chrono reads `TZ` there,
//! while on Windows the zone comes from the system and `TZ` is ignored. The zone is India's, five
//! and a half hours, so seconds, whole hours and minutes all give different answers; on a UTC
//! machine every one of them is zero, which is how the arithmetic survived (`docs/OPEN-FINDINGS.md`
//! O109).
#![cfg(unix)]

#[test]
fn the_anchor_offset_is_counted_in_minutes() {
    // SAFETY: the only test in this binary, and it sets the zone before anything reads it.
    unsafe { std::env::set_var("TZ", "Asia/Kolkata") };
    assert_eq!(nestwatch::clock::current_offset_mins(), 5 * 60 + 30);
}
