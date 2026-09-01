//! Property-based tests for the parsers that read the **child's own** stream.
//!
//! # Why these functions and not others
//!
//! The foreground watcher runs inside the child's session — it has to, because a Session 0 service
//! cannot see their desktop at all — and reports over a pipe. That makes
//! [`foreground::parse_sample`], [`clamp`], [`accrue_capped`] and [`read_bounded_line`] the only
//! parsers in this crate whose input an adversary writes, and `FOREGROUND-TRACKING.md` says so
//! outright: the premise of the module is that the watcher is not honest.
//!
//! # Why properties rather than more examples
//!
//! The existing tests are good, and every one of them is a case somebody thought of. This project's
//! discipline everywhere else is that *a test that passes on first write is not evidence* — that
//! mutation is what tells you an assertion has teeth. Property testing is the same instinct pointed
//! at **inputs** instead of at code: the invariants below are already written out in prose on
//! `clamp` and `bound` ("only one window holds focus at a time, so neither map can sum to more than
//! the interval"), and until now they were asserted at a handful of chosen points.
//!
//! These are deliberately not a rewrite of the example tests. An example says *this input gives
//! that output*, which is what a reader needs; a property says *no input gives a wrong output*,
//! which is what an adversary tests. Both are kept.
//!
//! `proptest` is a **dev-dependency**: it reaches no shipped byte, and `cargo deny check
//! advisories sources licenses bans` was run before and after adding it, reporting ok both times
//! with no widening of the licence allow-list.

use std::collections::BTreeMap;

use nestwatch::foreground::{MAX_PAGES, Sample, accrue_capped, clamp, parse_sample, write_sample};
use proptest::prelude::*;

/// Names as an adversary would send them: mostly short, sometimes empty, sometimes unicode.
///
/// Not `"\\PC \\w+"`-style tidy input. The keys are attacker-chosen, so the generator has to be
/// able to produce the awkward ones — an empty key, a key that is only whitespace, one that is
/// multi-byte — since those are exactly the shapes a hand-written fixture omits.
fn app_name() -> impl Strategy<Value = String> {
    prop_oneof![
        2 => "[a-z]{1,12}\\.exe",
        1 => Just(String::new()),
        1 => Just("   ".to_string()),
        1 => "\\PC{0,20}",
    ]
}

/// Seconds an adversary would claim, weighted toward the boundaries that break arithmetic.
fn secs() -> impl Strategy<Value = u64> {
    prop_oneof![
        3 => 0u64..120,
        1 => Just(0u64),
        1 => Just(u64::MAX),
        1 => (u64::MAX - 1000)..u64::MAX,
        1 => 0u64..u64::MAX,
    ]
}

fn sample() -> impl Strategy<Value = Sample> {
    (
        prop::collection::btree_map(app_name(), secs(), 0..12),
        prop::collection::btree_map(app_name(), secs(), 0..60),
    )
        .prop_map(|(apps, pages)| Sample { apps, pages })
}

fn total(map: &BTreeMap<String, u64>) -> u128 {
    map.values().map(|v| u128::from(*v)).sum()
}

proptest! {
    /// **The invariant `clamp`'s own doc states, against inputs nobody chose.**
    ///
    /// "Only one window holds focus at a time, so neither the apps nor the pages recorded for an
    /// interval can sum to more than the interval." If this can be violated, a forged report
    /// inflates a child's reported focus time without bound — and because the figure is only ever
    /// *reported*, never enforced on, the wrongness would surface as a parent reading a number that
    /// is simply false rather than as anything failing.
    #[test]
    fn clamp_never_lets_a_report_claim_more_time_than_actually_elapsed(
        s in sample(),
        elapsed in 0u64..600,
    ) {
        let out = clamp(s, elapsed);
        let cap = u128::from(elapsed);

        prop_assert!(
            total(&out.apps) <= cap,
            "apps summed to {} against {elapsed}s elapsed: {:?}",
            total(&out.apps), out.apps
        );
        prop_assert!(
            total(&out.pages) <= cap,
            "pages summed to {} against {elapsed}s elapsed: {:?}",
            total(&out.pages), out.pages
        );
        for (name, v) in out.apps.iter().chain(out.pages.iter()) {
            prop_assert!(
                u128::from(*v) <= cap,
                "{name:?} alone claimed {v}s of a {elapsed}s interval"
            );
        }
    }

    /// Zero-valued entries never survive, whatever the input.
    ///
    /// Stated on `clamp` as a bound on the *keyspace* rather than on the values: "they carry no
    /// information and would otherwise let a forged report pad the map with thousands of app
    /// names." A zero that survives is a free key, and keys are what the count caps exist for.
    #[test]
    fn clamp_keeps_no_zero_entries(s in sample(), elapsed in 0u64..600) {
        let out = clamp(s, elapsed);
        for (name, v) in out.apps.iter().chain(out.pages.iter()) {
            prop_assert!(*v > 0, "{name:?} survived with 0s");
        }
    }

    /// A zero-length interval measures nothing at all.
    ///
    /// The degenerate case, and the one where a division would sit if the implementation had not
    /// returned early: nothing can have been in front for any of no time.
    #[test]
    fn a_zero_length_interval_yields_nothing(s in sample()) {
        let out = clamp(s, 0);
        prop_assert!(out.apps.is_empty() && out.pages.is_empty(), "{out:?}");
    }

    /// The page cap holds however many titles are claimed.
    ///
    /// Page titles are the highest-cardinality thing recorded and come from a process running as
    /// the child — "every tab, every video, every renamed document is a new key". This is the bound
    /// that stops a retitling loop growing `usage_state.json` without limit.
    #[test]
    fn the_page_cap_holds_however_many_titles_are_claimed(
        pages in prop::collection::btree_map(app_name(), 1u64..50, 0..(MAX_PAGES * 3)),
        elapsed in 1u64..600,
    ) {
        let out = clamp(Sample { apps: BTreeMap::new(), pages }, elapsed);
        prop_assert!(
            out.pages.len() <= MAX_PAGES,
            "{} page titles survived a cap of {MAX_PAGES}",
            out.pages.len()
        );
    }

    /// Accrual respects its key cap across repeated ticks.
    ///
    /// `clamp` bounds what the numbers may *say*; this bounds how many of them there may *be*, and
    /// `accrue_capped`'s doc is explicit that the second does not follow from the first — "a report
    /// of ten thousand one-second entries passes every value check ever written." Driven over many
    /// ticks because the map that is persisted is the one that *accumulates*, which is where the
    /// two were once allowed to disagree.
    #[test]
    fn accrual_never_outgrows_its_key_cap_however_many_ticks_arrive(
        ticks in prop::collection::vec(
            prop::collection::btree_map(app_name(), 1u64..30, 0..8),
            1..25,
        ),
        cap in 1usize..12,
    ) {
        let mut running = BTreeMap::new();
        for t in ticks {
            accrue_capped(&mut running, t, cap);
            prop_assert!(
                running.len() <= cap,
                "the accumulator holds {} keys against a cap of {cap}",
                running.len()
            );
        }
    }

    /// Writing a sample and reading it back gives the same sample.
    ///
    /// The pipe format is the seam between two processes that ship in one binary, so a field that
    /// serializes and does not deserialize is invisible to any test living on one side of it —
    /// which is the same reason `tests/golden.rs` exists for the HTTP seam.
    #[test]
    fn a_written_sample_reads_back_unchanged(s in sample()) {
        let mut out = Vec::new();
        write_sample(&mut out, &s).expect("writing to a Vec cannot fail");
        let text = String::from_utf8(out).expect("serde_json emits UTF-8");
        let line = text.strip_suffix('\n').expect("write_sample terminates its line");

        let round_tripped = parse_sample(line);
        prop_assert_eq!(
            round_tripped.as_ref(),
            Some(&s),
            "the pipe format lost or changed something: {}", line
        );
    }

    /// **A malformed line is never fatal, for any bytes at all.**
    ///
    /// `parse_sample`'s contract is that "a corrupt or truncated line must never be fatal: the
    /// watcher writes to a pipe that can be cut mid-line by a session ending or the child killing
    /// the process". The child chooses these bytes, and the reader is the SYSTEM service — so the
    /// property that matters is not *what* it returns but that it always returns.
    #[test]
    fn no_bytes_can_make_the_parser_panic(raw in "\\PC{0,200}") {
        let _ = parse_sample(&raw);
    }

    /// Truncating a valid line anywhere never panics, and never yields a *different* sample.
    ///
    /// The torn-write case specifically, rather than random noise: a prefix of real JSON is far
    /// more likely to parse into something than arbitrary text is, and "parses into something
    /// wrong" is worse than "does not parse".
    #[test]
    fn a_torn_line_is_either_rejected_or_identical(s in sample(), cut in 0usize..400) {
        let mut out = Vec::new();
        write_sample(&mut out, &s).expect("writing to a Vec cannot fail");
        let text = String::from_utf8(out).expect("serde_json emits UTF-8");
        let line = text.trim_end_matches('\n');

        let at = cut.min(line.len());
        // Truncate on a char boundary — a byte-sliced multi-byte name is a different (and
        // uninteresting) failure from a torn record.
        let at = (0..=at).rev().find(|i| line.is_char_boundary(*i)).unwrap_or(0);

        if let Some(parsed) = parse_sample(&line[..at]) {
            prop_assert_eq!(
                &parsed, &s,
                "a truncated line parsed into a DIFFERENT sample, which would be recorded as \
                 measurement: {:?}", &line[..at]
            );
        }
    }
}

/// A forged report cannot spend the wire budget on a few enormous keys.
///
/// # The gap this closes
///
/// Every other bound in this module is on a **count** or a **value**: `MAX_PAGES` and `MAX_APPS`
/// cap how many keys there may be, `clamp` caps what the seconds may say. Nothing capped how *long*
/// a key may be. The 512-unit limit that exists lives in `watcher::window_title`'s buffer — inside
/// the helper, which runs as the child, so it binds an honest watcher and nothing else. On the
/// service side the only remaining ceiling was `MAX_LINE`, and its own doc records that one MiB
/// leaves "about six times over" above the honest worst case of 170,170 bytes. That headroom is
/// exactly the room a forged writer has.
///
/// # Why it matters more than it looks
///
/// These keys are not transient. They are accrued into `usage_state.json`, rewritten (and
/// `fsync`ed) every thirty seconds, and folded into the daily rollup row that lands in
/// `screentime.jsonl` — the log `O67` calls the one holding "the irreplaceable rows", capped at two
/// generations of 2 MiB. Long keys therefore buy an attacker the same thing the audit partition
/// already had to take away on the other log: **history eviction paced by whoever is knocking**.
///
/// The assertion is on the persisted size rather than on the cap directly, because the size is the
/// harm and the cap is only one way to bound it.
#[test]
fn a_forged_report_cannot_fill_the_history_log_with_enormous_keys() {
    use nestwatch::foreground::{MAX_LINE, MAX_PAGES};

    // One second per title, against a thirty-second tick.
    //
    // Load-bearing, and the first version of this test got it wrong and passed for the wrong
    // reason. Claiming thirty seconds for each of forty titles makes the total 1,200s against a
    // 30s interval, so `bound` scales every entry to zero and drops the lot — the map comes out
    // empty and the assertion below sails through. A forged report that wants its keys *kept* has
    // to be arithmetically honest: the values must already fit, so nothing is scaled and nothing
    // is dropped. That is the state an attacker would actually send.
    // The tick the enforcer actually uses, and the key count that can survive it: one second each,
    // summing to no more than the interval, so `bound` scales nothing away.
    const TICK: u64 = 30;
    let keys = (TICK as usize - 1).min(MAX_PAGES);
    let per_key = (MAX_LINE as usize / (keys + 4)) - 64;
    let mut pages = BTreeMap::new();
    for i in 0..keys {
        pages.insert(format!("{i:04}{}", "T".repeat(per_key)), 1u64);
    }
    let apps = BTreeMap::from([("game.exe".to_string(), 1u64)]);

    let wire = serde_json::to_string(&Sample {
        apps: apps.clone(),
        pages: pages.clone(),
    })
    .expect("a sample serializes");
    assert!(
        wire.len() as u64 <= MAX_LINE,
        "the probe must itself respect the wire limit, or it is testing an unreachable state: {} \
         bytes against {MAX_LINE}",
        wire.len()
    );

    let bounded = clamp(Sample { apps, pages }, TICK);
    let persisted = serde_json::to_string(&bounded).expect("a sample serializes");

    // A generous ceiling rather than a tight one: the point is that a single tick cannot approach
    // the size of the history log it will be folded into, not that it hits a particular number.
    const ROLLUP_CEILING: usize = 64 * 1024;
    assert!(
        persisted.len() <= ROLLUP_CEILING,
        "one forged tick persists {} bytes. That is folded into the daily rollup row in \
         screentime.jsonl, which keeps two generations of 2 MiB — so a child who can write to the \
         watcher pipe evicts the parent's whole screen-time history in a few days, which is the \
         eviction class the audit partition already had to close on the other log. Longest key: \
         {} bytes",
        persisted.len(),
        bounded.pages.keys().map(String::len).max().unwrap_or(0)
    );
}

/// `read_bounded_line` holds its ceiling and stays in step with the stream.
///
/// Outside `proptest!` because it drives a reader over many lines and asserts two things at once:
/// that no single read exceeds `max` (the memory bound — `BufRead::lines` would let a writer that
/// never sends a newline take the reader's memory), and that an over-long line costs *one* line
/// rather than desynchronising everything after it.
#[test]
fn a_bounded_read_holds_its_ceiling_and_resynchronises() {
    use nestwatch::foreground::read_bounded_line;
    use std::io::BufReader;

    proptest!(|(
        lens in prop::collection::vec(0usize..40, 1..12),
        max in 4u64..24,
    )| {
        // Distinguishable lines, so "which line came back" is answerable.
        let mut stream = String::new();
        for (i, len) in lens.iter().enumerate() {
            stream.push_str(&format!("{i}"));
            stream.push_str(&"x".repeat(*len));
            stream.push('\n');
        }

        let mut reader = BufReader::new(stream.as_bytes());
        let mut buf = Vec::new();
        let mut returned = Vec::new();
        // Bounded so a bug that never advances the reader fails the test instead of hanging it.
        for _ in 0..(lens.len() * 4 + 8) {
            match read_bounded_line(&mut reader, &mut buf, max) {
                Ok(true) => {
                    prop_assert!(
                        buf.len() as u64 <= max,
                        "a single read returned {} bytes against a ceiling of {max} — the whole \
                         point is that the limit is on the READ, not on an inspection afterwards",
                        buf.len()
                    );
                    returned.push(String::from_utf8_lossy(&buf).to_string());
                }
                Ok(false) => break,
                Err(e) => prop_assert!(false, "unexpected error: {e}"),
            }
        }

        // **An over-long line comes back as an EMPTY one, not as nothing.** That is the documented
        // contract rather than a quirk, and `session.rs` relies on it in as many words: "An
        // over-long line arrives here as an empty one, which fails to parse and is skipped by the
        // same path." `Ok(true)` means "the stream continues", never "here is a record" — the
        // first version of this test read it as the latter and failed against correct code.
        //
        // So the property is about ORDER and COUNT, which is what "one bad line costs one line"
        // actually means: every short line comes back intact and in sequence, and each over-long
        // one costs exactly one empty slot rather than swallowing its neighbour.
        let expected: Vec<String> = lens
            .iter()
            .enumerate()
            .map(|(i, len)| {
                let line = format!("{i}{}", "x".repeat(*len));
                // `+ 1` for the newline, and it is not an off-by-one in the test.
                // `take(max).read_until(b'\n', ..)` reads at most `max` bytes *including* the
                // terminator, so a record of exactly `max` characters cannot have its newline
                // read and is treated as over-long. `MAX_LINE` therefore bounds the record plus
                // its terminator, and the usable payload is `MAX_LINE - 1`.
                let with_terminator = line.len() as u64 + 1;
                if with_terminator <= max { line } else { String::new() }
            })
            .collect();
        prop_assert_eq!(
            &returned, &expected,
            "an over-long line desynchronised the stream: one bad line must cost one line, and \
             every line short enough to fit must arrive intact and in order"
        );
    });
}
