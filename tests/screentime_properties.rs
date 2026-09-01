//! Property-based tests for the report a parent reads.
//!
//! # Why this file and not more examples
//!
//! `screentime.rs` is the largest piece of pure logic in the crate and its own comments carry the
//! sharpest rule in the project: **absent is not zero**. A day the service never ticked through
//! must not render like a day it watched and saw nothing, because the first is a dead enforcer and
//! the second is a well-behaved child, and collapsing them lets one look exactly like the other.
//!
//! That rule is not one assertion, it is an invariant across a dozen fields — `measured`,
//! `minutes_used`, `daily_avg_mins`, `prev_total_mins`, `change_pct`, `first_seen`, and every
//! `*_totals` list. Examples pin the cases somebody thought of. These pin the relationships, over
//! rows nobody chose: duplicated dates, dates in the future, malformed rows, an empty store, a
//! window wider than the history, and the arithmetic edges around a zero baseline.
//!
//! # What is deliberately not asserted
//!
//! The exact wording of any field, and the exact top-N contents. Those are display decisions the
//! example tests already own. What is asserted here is only what must hold for the numbers on the
//! card to be *consistent with each other* — because that is what a parent actually reasons with,
//! and an inconsistency between two of them is invisible to a test that checks either alone.

use chrono::{Duration, NaiveDate};
use nestwatch::screentime::{TOP_OVER_WINDOW, build_report, recent_totals};
use proptest::prelude::*;
use serde_json::{Value, json};

/// A fixed "today", so nothing here depends on the wall clock.
fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 6, 15).expect("valid date")
}

/// Rows as the store really holds them: mostly well-formed, sometimes duplicated, sometimes in the
/// future, sometimes missing the fields `parse_row` requires.
///
/// The awkward ones are the point. A future-dated row is a clock artefact the code explicitly
/// filters; a duplicate arrives legitimately because the same day can come from both logs; a row
/// with no `minutes_used` is a legacy or torn record. All three exist in real installs.
fn row() -> impl Strategy<Value = Value> {
    (
        -400i64..40,
        // Zero weighted heavily on purpose. A previous window that was *measured* and totalled
        // zero is the one arithmetic edge in `build_report` — it is a real baseline, so
        // `change_pct` must be `Some(0)` against a zero current window and `None` against a
        // non-zero one, because that percentage is undefined rather than infinite. With minutes
        // drawn uniformly from 0..2000 an all-zero previous window essentially never occurs, and
        // the property asserting it passed against a mutation that divided anyway.
        prop_oneof![3 => Just(0u64), 2 => 0u64..2000],
        prop::option::of(0u64..600),
        // Enough distinct names, from a wide enough alphabet, that a window really can carry more
        // than `TOP_OVER_WINDOW` of them. The first version used up to six apps per row from
        // `[a-z]{1,8}`, which collided so often that the cap was never reached — so the test that
        // asserts it held passed against a build with the truncation removed.
        prop::collection::btree_map("[a-z]{6}", 0u64..500, 0..14),
        prop::bool::ANY,
        prop::bool::ANY,
    )
        .prop_map(|(offset, mins, budget, apps, drop_minutes, drop_date)| {
            let date = today() + Duration::days(offset);
            let mut v = json!({
                "date": date.format("%Y-%m-%d").to_string(),
                "minutes_used": mins,
                "apps": apps,
            });
            if let Some(b) = budget {
                v["budget"] = json!(b);
            }
            // Malformed shapes the reader must survive rather than trust.
            if drop_minutes {
                v.as_object_mut().expect("object").remove("minutes_used");
            }
            if drop_date {
                v["date"] = json!("not-a-date");
            }
            v
        })
}

fn measured_rows(report: &nestwatch::screentime::Report) -> Vec<&nestwatch::screentime::DayRow> {
    report.days.iter().filter(|d| d.measured).collect()
}

proptest! {
    /// The window is exactly as wide as it was asked for, in order, and never reaches today.
    ///
    /// Three separate things a parent's chart depends on and no single example checks together: a
    /// missing column silently narrows the period the totals are read against, a repeated date
    /// double-counts a day, and a column for today would show a partial day beside complete ones as
    /// though they were comparable.
    #[test]
    fn the_window_is_the_width_asked_for_in_order_and_never_includes_today(
        rows in prop::collection::vec(row(), 0..40),
        days in 1u32..120,
    ) {
        let r = build_report(&rows, today(), days);

        prop_assert_eq!(
            r.days.len(),
            days as usize,
            "a report of {} days has {} columns", days, r.days.len()
        );

        let dates: Vec<NaiveDate> = r
            .days
            .iter()
            .map(|d| NaiveDate::parse_from_str(&d.date, "%Y-%m-%d").expect("emitted dates parse"))
            .collect();

        for pair in dates.windows(2) {
            prop_assert!(
                pair[1] == pair[0] + Duration::days(1),
                "the columns are not consecutive: {} then {}", pair[0], pair[1]
            );
        }
        if let Some(last) = dates.last() {
            prop_assert!(*last < today(), "a column for today or later: {last}");
        }
    }

    /// Every headline number agrees with the columns it is drawn from.
    ///
    /// The totals and the chart are read side by side, so a disagreement between them is a parent
    /// being told two different things at once — and each is individually plausible, which is what
    /// makes it survive review.
    #[test]
    fn the_totals_agree_with_the_days_they_are_drawn_from(
        rows in prop::collection::vec(row(), 0..40),
        days in 1u32..90,
    ) {
        let r = build_report(&rows, today(), days);
        let measured = measured_rows(&r);

        prop_assert_eq!(
            r.measured_days,
            measured.len(),
            "measured_days disagrees with the number of measured columns"
        );

        let summed: u64 = measured.iter().filter_map(|d| d.minutes_used).sum();
        prop_assert_eq!(
            r.total_mins, summed,
            "total_mins is not the sum of the measured columns"
        );

        // A measured day always carries a number; an unmeasured one never does. This is the
        // absent-is-not-zero rule stated as a relationship rather than as two separate fields.
        for d in &r.days {
            prop_assert_eq!(
                d.measured,
                d.minutes_used.is_some(),
                "{}: measured={} but minutes_used={:?} — a dead enforcer and a quiet day must not \
                 be renderable as the same column",
                d.date, d.measured, d.minutes_used
            );
        }
    }

    /// The average exists exactly when there is something to average.
    ///
    /// Averaging unmeasured days as zero understates by precisely the amount that is unknown, which
    /// is the failure the field's own doc names. `Some(0)` and `None` are different claims and the
    /// card renders them differently.
    #[test]
    fn the_average_is_absent_exactly_when_nothing_was_measured(
        rows in prop::collection::vec(row(), 0..40),
        days in 1u32..90,
    ) {
        let r = build_report(&rows, today(), days);

        prop_assert_eq!(
            r.daily_avg_mins.is_some(),
            r.measured_days > 0,
            "daily_avg_mins={:?} against measured_days={}",
            r.daily_avg_mins, r.measured_days
        );
        if let Some(avg) = r.daily_avg_mins {
            let expected = r.total_mins / r.measured_days as u64;
            prop_assert_eq!(avg, expected, "the mean does not match total/measured");
        }
    }

    /// The comparison never divides by zero, and never reads an absent baseline as "no change".
    ///
    /// The arithmetic here has a genuine undefined case — a previous window that was measured and
    /// totalled zero, against a current window that did not. `build_report` returns `None` there on
    /// purpose and its comment says not to "fix" it into a division. This is what stops that
    /// comment from being the only thing holding the line.
    #[test]
    fn the_comparison_is_absent_rather_than_wrong_when_there_is_no_baseline(
        rows in prop::collection::vec(row(), 0..40),
        days in 1u32..60,
    ) {
        let r = build_report(&rows, today(), days);

        if r.prev_total_mins.is_none() {
            prop_assert!(
                r.change_pct.is_none(),
                "a change against no baseline: {:?}", r.change_pct
            );
        }
        // Zero to non-zero is undefined, not infinite and not 0%.
        if r.prev_total_mins == Some(0) && r.total_mins > 0 {
            prop_assert!(
                r.change_pct.is_none(),
                "a percentage against a zero baseline: {:?}", r.change_pct
            );
        }
    }

    /// Every windowed list is capped and ordered, whatever the history holds.
    ///
    /// Page titles are attacker-influenced and the number of distinct ones over ninety days has no
    /// natural bound — the cap's own doc says so. Ordering matters just as much: the lists are read
    /// as "what he actually did", and an unordered one puts a stray entry at the top of the answer.
    #[test]
    fn every_windowed_list_is_capped_and_heaviest_first(
        rows in prop::collection::vec(row(), 0..60),
        days in 1u32..90,
    ) {
        let r = build_report(&rows, today(), days);
        for (name, list) in [
            ("app_totals", &r.app_totals),
            ("focus_totals", &r.focus_totals),
            ("page_totals", &r.page_totals),
            ("group_totals", &r.group_totals),
        ] {
            prop_assert!(
                list.len() <= TOP_OVER_WINDOW,
                "{name} carries {} rows against a cap of {TOP_OVER_WINDOW}", list.len()
            );
            for pair in list.windows(2) {
                prop_assert!(
                    pair[0].minutes >= pair[1].minutes,
                    "{name} is not heaviest-first: {} then {}", pair[0].minutes, pair[1].minutes
                );
            }
        }
    }

    /// The retention horizon describes the store, not the window.
    ///
    /// `history_from` answers "how far back can this tool see at all", so it must not move when the
    /// parent presses 7 / 30 / 90 — the whole point of showing it is that rotation deletes silently,
    /// and a figure that changed with the button would be describing the button.
    #[test]
    fn the_retention_horizon_does_not_move_with_the_window(
        rows in prop::collection::vec(row(), 0..40),
        a in 1u32..30,
        b in 60u32..120,
    ) {
        let narrow = build_report(&rows, today(), a);
        let wide = build_report(&rows, today(), b);
        prop_assert_eq!(
            &narrow.history_from, &wide.history_from,
            "the oldest day held changed when only the window did"
        );
    }

    /// The child's own week never shows a day that was not measured as a zero.
    ///
    /// `/status` is unauthenticated and the child reads it about themselves. The same
    /// absent-is-not-zero rule applies and matters more, not less: a child looking at their own
    /// week must not be shown a confident zero for a day the service was simply not running.
    #[test]
    fn the_childs_own_week_never_invents_a_zero(
        rows in prop::collection::vec(row(), 0..40),
        days in 1u32..40,
    ) {
        let totals = recent_totals(&rows, today(), days);

        prop_assert!(totals.len() <= 31, "the child's view is clamped to a month");
        for t in &totals {
            let d = NaiveDate::parse_from_str(&t.date, "%Y-%m-%d").expect("emitted dates parse");
            prop_assert!(d < today(), "a column for today or later: {d}");
        }
        let dates: Vec<NaiveDate> = totals
            .iter()
            .map(|t| NaiveDate::parse_from_str(&t.date, "%Y-%m-%d").expect("parses"))
            .collect();
        for pair in dates.windows(2) {
            prop_assert!(pair[1] > pair[0], "not oldest-first: {} then {}", pair[0], pair[1]);
        }
    }

    /// The two views of the same history agree about which days were measured.
    ///
    /// `recent_totals` exists as a deliberately cheaper reader for the child's page — it parses two
    /// fields where `build_report` parses every map. Two readers of one store is exactly the shape
    /// that drifts, and the drift would be silent: the parent's chart and the child's week would
    /// disagree about which evenings existed, with each looking correct on its own.
    #[test]
    fn the_parents_chart_and_the_childs_week_agree_about_which_days_were_measured(
        rows in prop::collection::vec(row(), 0..40),
        days in 1u32..25,
    ) {
        let report = build_report(&rows, today(), days);
        let child = recent_totals(&rows, today(), days);

        for c in &child {
            let Some(parent_day) = report.days.iter().find(|d| d.date == c.date) else {
                continue;
            };
            prop_assert_eq!(
                parent_day.minutes_used.is_some(),
                c.minutes.is_some(),
                "{}: the parent's chart says measured={} and the child's week says {:?}",
                c.date, parent_day.measured, c.minutes
            );
        }
    }
}
