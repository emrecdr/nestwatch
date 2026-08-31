//! Reflow-tolerant primitives for the guards that read this crate's own source.
//!
//! # Why this module exists at all
//!
//! Several tests here defend a property by scanning the source: no program launched by bare name,
//! no unauthenticated route outside a known list, no child-facing string written in place of a
//! translation. Each looked for a needle naming a call: the bare-name spawn that `spawn_paths.rs`
//! polices, or the route registration that `server.rs` does. Every one of those needles spans a
//! syntactic boundary, and every one of those scans read the file a line at a time.
//!
//! `rustfmt` breaks a call the moment it outgrows the line width. The needle then straddles the
//! break and the scan matches nothing, so the guard reports success over exactly the code it
//! exists to catch. **Three guards were found blind this way on 2026-08-31**, by two people
//! working independently, each confirmed by inserting a probe and watching the shipped test pass.
//!
//! The most instructive part is in `spawn_paths.rs`, which contains one scanner that failed open
//! and a second that survives the same reflow *by construction rather than by care* — it counts
//! `system32(` against `system32("`, so a broken call yields a mismatch and lands in a bucket that
//! asserts. Same file, same hand, opposite failure directions. Nobody reasoned about reflow in
//! either, which is the argument for this module: not that hand-rolling it is hard, but that
//! whether a hand-roll fails open or closed has so far been luck.
//!
//! # Why it is not behind `#[cfg(test)]`
//!
//! `src/testutil.rs` is `#[cfg(test)]`, so an integration-test binary cannot see it — and the
//! scanners are split across both worlds: `server.rs`'s lives in the library's own test module
//! while `translated_strings.rs` and `spawn_paths.rs` are integration binaries. A `cfg(test)`
//! helper would have to be duplicated to serve both, which is the situation this replaces.
//!
//! So it is an ordinary `pub` module. The cost is a few pure string functions compiled into the
//! library that production never calls; nothing in the binary references them.

use std::borrow::Cow;

/// The part of a source file above its unit-test module.
///
/// Test code writes `let msg = "expected wording"` legitimately and constantly, so a scan that
/// included it would be noisy enough to get switched off — which is the usual way a guard like
/// this dies.
///
/// **Matches the whole `#[cfg(test)] mod tests` opening, not merely the attribute.** Cutting on the
/// first `#[cfg(test)]` anywhere in the text silently truncated three files in this crate at
/// something that was not a test module at all: a `//!` doc comment that quoted the attribute
/// (`testutil.rs`, 174 of 177 lines skipped), an indented `#[cfg(test)]` on a single helper method
/// (`foreground.rs`, ~895), and `#[cfg(test)] mod testutil;`, a module declaration (`lib.rs`, 567
/// of 623). The guard still passed, while reading a third of the crate.
pub fn production_source(text: &str) -> &str {
    match text.split_once("\n#[cfg(test)]\nmod tests") {
        Some((before, _)) => before,
        None => text,
    }
}

/// `text` as whole statements, each paired with the line it starts on.
///
/// **Prefer [`find_tokens`]. This is the weaker primitive and it has one consumer left.** It joins
/// lines until parentheses balance, which is approximate in ways that are not fixable by patching:
/// a binding inside a closure is swallowed into the enclosing statement, an `#[allow(...)]` line
/// prepends itself to the statement below it, and one trailing comment holding an unbalanced `(`
/// corrupts the depth for the rest of the file. Measured: 115 of 1,716 statements across six
/// production files span ten or more lines, the worst 56.
///
/// Whether that matters depends entirely on what the consumer asserts. `translated_strings.rs`
/// anchored its rule to a statement's *start*, so over-joining cost it the whole guarantee — three
/// ordinary shapes silently switched the guard off, and it has been rewritten onto `find_tokens`.
/// `scanner_guards.rs` asserts on content found anywhere *within* a statement, so over-joining
/// costs it a wrong line number in a failure message and nothing else.
///
/// **Takes `text` as given; it does not cut at the test module.** Compose when you want the cut:
/// `statements(production_source(text))`. Both callers do — the cut is a policy each guard states
/// for itself rather than something this reader decides for them.
///
/// The justification here previously claimed the opposite: that a guard policing *other* guards
/// needs uncut text, because that is where scanners live. That is true of the ambition and false
/// of the code — scanning whole files was tried and reverted, and `tests/scanner_guards.rs` now
/// carries nine lines explaining why it must cut. A reader acting on the old paragraph would have
/// removed that call and reintroduced the false positives it describes.
///
/// This is the reflow-tolerant reader. Lines are joined until parentheses balance and the text
/// ends a statement, so a call and the arguments handed to it stay in one unit however the
/// formatter has broken them up. The reported line is where the statement *starts*, which is the
/// one a reader wants to be sent to.
///
/// **Two places take no separator, and both are load-bearing.** `rustfmt` breaks a call at exactly
/// two points, and a space inserted at either one re-breaks the needle the join was meant to
/// repair:
///
/// - before a leading `.`, because the formatter breaks a method chain *at* the dot — joining with
///   a space turns `control` + `.shutdown(` into `control .shutdown(`, which contains
///   `control.shutdown(` no more than the two separate lines did;
/// - directly after an open `(`, because the formatter also breaks between a call's paren and its
///   first argument — `Command::new(` + `"shutdown.exe"` must rejoin as `Command::new("…`, not
///   `Command::new( "…`.
///
/// Each was got wrong once. The first version of this joiner inserted a space before the `.`: it
/// reassembled the statement and then broke it again with its own separator, and the probe still
/// passed while the code looked fixed. The second version fixed the dot and left the paren, which
/// is **the shape that defeated two of the three original guards** — a scan for `Command::new("` or
/// `.route("` still matched nothing. Found by the other session compiling this function and running
/// the three real needles through it rather than reading it. Both are pinned by tests below.
///
/// A separator IS still inserted after `=`, deliberately: `let msg =` + `"text"` must rejoin as
/// `let msg = "text"`, which is the shape the translation guard looks for.
pub fn statements(text: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let (mut buf, mut start, mut depth) = (String::new(), 0usize, 0i32);

    for (i, raw) in text.lines().enumerate() {
        let trimmed = raw.trim();
        if trimmed.starts_with("//") || (buf.is_empty() && trimmed.is_empty()) {
            continue;
        }
        if buf.is_empty() {
            start = i + 1;
        } else if !trimmed.starts_with('.') && !buf.ends_with('(') {
            buf.push(' ');
        }
        buf.push_str(trimmed);

        // Char literals holding a bracket are scrubbed first. `s.matches('(')` is ordinary Rust
        // and it used to increment the depth without ever closing, so the statement ran on and
        // swallowed everything after it — which a scanner then read as one enormous statement and
        // reported needles at the wrong place. This module's own body contains that exact call,
        // so it mis-parsed itself.
        //
        // String literals are still not tracked, and that stays a deliberate asymmetry: a bracket
        // inside a `"…"` can only *delay* the close, which over-joins and costs a noisier message.
        // The char-literal case was different in kind because it could never be balanced at all.
        // Only allocate when there is something to scrub. The unconditional form built two
        // `String`s for every line of every scanned file, and the overwhelming majority contain no
        // char literal at all.
        let scrubbed = if trimmed.contains("'(") || trimmed.contains("')") {
            Cow::Owned(trimmed.replace("'('", "").replace("')'", ""))
        } else {
            Cow::Borrowed(trimmed)
        };
        depth += scrubbed.matches('(').count() as i32 - scrubbed.matches(')').count() as i32;
        let ends = trimmed.ends_with(';') || trimmed.ends_with('{') || trimmed.ends_with('}');
        if depth <= 0 && ends {
            out.push((start, std::mem::take(&mut buf)));
            depth = 0;
        }
    }
    if !buf.is_empty() {
        out.push((start, buf));
    }
    out
}

/// 1-based line number containing byte offset `at`.
pub fn line_of(text: &str, at: usize) -> usize {
    text[..at].matches('\n').count() + 1
}

/// Whether the line containing `at` is a comment line.
///
/// Judged by the line the match *starts* on, the same rule `spawn_paths.rs` uses — parsing comments
/// properly is `O63` and not needed here. A doc comment quoting a call shape is not a call.
fn in_a_comment_line(text: &str, at: usize) -> bool {
    let line_start = text[..at].rfind('\n').map_or(0, |n| n + 1);
    text[line_start..at].trim_start().starts_with("//")
}

/// Byte offsets where `tokens` appear **in order, separated only by whitespace**.
///
/// This is the reflow-proof primitive, and it is deliberately not a parser. `rustfmt` can only ever
/// put whitespace between the pieces of a call, so skipping whitespace between tokens is the whole
/// of what tolerance requires: `find_tokens(text, &["Command::new", "(", "\""])` matches the call
/// however the formatter has broken it, on one line or five.
///
/// **It replaced a statement-reconstructing reader, and the reason is worth keeping.** That reader
/// joined lines until parentheses balanced, which made every scan built on it fail open on ordinary
/// Rust: a binding inside a closure was swallowed into the enclosing statement, an
/// `#[allow(...)]` line above a binding prepended itself to it, and a single trailing comment
/// containing an unbalanced `(` corrupted the depth for the rest of the file. Measured, not
/// feared — three of the three shapes tried were missed, and 115 of 1,716 "statements" across six
/// production files spanned ten or more lines, the worst 56. Matching tokens never reconstructs a
/// statement, so none of those shapes exists to be got wrong.
///
/// Matches beginning on a comment line are skipped; see [`in_a_comment_line`].
pub fn find_tokens(text: &str, tokens: &[&str]) -> Vec<usize> {
    let Some((first, rest)) = tokens.split_first() else {
        return Vec::new();
    };
    text.match_indices(first)
        .filter(|(at, _)| !in_a_comment_line(text, *at))
        .filter_map(|(at, needle)| {
            let mut cursor = at + needle.len();
            for tok in rest {
                let after = text[cursor..].trim_start();
                if !after.starts_with(tok) {
                    return None;
                }
                // `trim_start` moved the cursor by however much whitespace it removed.
                cursor = text.len() - after.len() + tok.len();
            }
            Some(at)
        })
        .collect()
}

/// The text between the parentheses of the call whose `(` is the first one at or after `from`.
///
/// Bounded and local: it balances parentheses from one known opening, rather than reconstructing
/// statements across a file. A call's arguments are where a hard-coded string hides when it is not
/// the first argument — `control.shutdown(60, Some("…"))` — so a token sequence alone cannot see
/// it, and this is the smallest thing that can.
///
/// `None` when there is no `(` after `from`, or when it is never closed.
pub fn call_arguments(text: &str, from: usize) -> Option<&str> {
    let open = from + text[from..].find('(')?;
    let mut depth = 0i32;
    for (i, c) in text[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[open + 1..open + i]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The statements of `src`, one per line — what every test here asserts against.
    ///
    /// Written out five times before this existed, in two spellings, which is the shape that makes
    /// a sixth test a sixth variant.
    fn joined(src: &str) -> String {
        parts(src).join("\n")
    }

    /// The statements of `src`, for the two tests that need them apart rather than run together.
    fn parts(src: &str) -> Vec<String> {
        statements(src).into_iter().map(|(_, s)| s).collect()
    }

    /// The four shapes that defeated the statement reader this replaced.
    ///
    /// Every one of them made a scan miss a binding it was written to catch, and every one fails
    /// *open* — which is the defect class the module exists to close, reproduced inside it. They
    /// are fixtures rather than a probe against the tree so they keep holding after the tree stops
    /// happening to contain an example.
    #[test]
    fn token_matching_survives_what_statement_joining_did_not() {
        for (label, src) in [
            ("plain", "fn f() {\n    let msg = \"Bedtime.\";\n}\n"),
            (
                "inside a closure",
                "fn f() {\n    x.map(|y| {\n        let msg = \"Bedtime.\";\n    });\n}\n",
            ),
            (
                "under an attribute",
                "fn f() {\n    #[allow(dead_code)]\n    let msg = \"Bedtime.\";\n}\n",
            ),
            (
                "after a comment holding an unbalanced paren",
                "fn f() {\n    let a = 1; // see notify_child(\n    let msg = \"Bedtime.\";\n}\n",
            ),
            (
                "broken across lines by the formatter",
                "fn f() {\n    let msg =\n        \"Bedtime.\";\n}\n",
            ),
        ] {
            assert_eq!(
                find_tokens(src, &["let msg", "=", "\""]).len(),
                1,
                "missed the binding {label}"
            );
        }
    }

    /// A comment naming the shape is not the shape.
    #[test]
    fn a_call_written_only_in_a_comment_is_not_a_hit() {
        let src = "fn f() {\n    // let msg = \"Bedtime.\";\n    let x = 1;\n}\n";
        assert!(find_tokens(src, &["let msg", "=", "\""]).is_empty());
    }

    /// Tokens must be consecutive-modulo-whitespace, or the matcher would agree with anything.
    #[test]
    fn tokens_separated_by_anything_but_whitespace_do_not_match() {
        let src = "fn f() {\n    let msg = compute(\"x\");\n}\n";
        assert!(
            find_tokens(src, &["let msg", "=", "\""]).is_empty(),
            "`let msg = compute(\"x\")` binds a call result, not a literal"
        );
    }

    /// A literal that is not the first argument still has to be found.
    #[test]
    fn call_arguments_reaches_a_literal_that_is_not_the_first_argument() {
        let src = "fn f() {\n    control\n        .shutdown(\n            60,\n            Some(\"Bedtime.\".into()),\n        );\n}\n";
        let at = *find_tokens(src, &["control", ".shutdown", "("])
            .first()
            .expect("the reflowed sink must be found");
        let args = call_arguments(src, at).expect("the call must close");
        assert!(
            args.contains('"'),
            "the literal in the second argument was not reachable: {args}"
        );
    }

    /// The shape every one of the three blind guards was defeated by.
    #[test]
    fn a_call_the_formatter_has_broken_up_is_read_as_one_statement() {
        let src = "fn f() {\n    let _ = control\n        .shutdown(\n            60,\n            \
                   Some(\"Bedtime.\".to_string()),\n        );\n}\n";
        let all = joined(src);
        assert!(
            all.contains("control.shutdown("),
            "the method chain was not rejoined — this is the whole point of the module:\n{all}"
        );
        assert!(
            all.contains("control.shutdown(") && all.contains('"'),
            "the sink and the literal handed to it must land in the SAME statement, or a scan can \
             see one without the other and pass"
        );
    }

    /// The paren shape — the one that defeated two of the three original guards.
    ///
    /// `rustfmt` breaks between a call's paren and its first argument at least as readily as at a
    /// dot, and these are the two real needles it hid. This test exists because the joiner shipped
    /// handling the dot and not the paren, which made the module doc's promise false for exactly
    /// the cases it named.
    #[test]
    fn a_call_split_between_its_paren_and_first_argument_is_rejoined() {
        for (needle, src) in [
            (
                "Command::new(\"",
                "fn f() {\n    let c = Command::new(\n        \"shutdown.exe\",\n    );\n}\n",
            ),
            (
                ".route(\"",
                "fn f() {\n    r = r\n        .route(\n            \"/ask\",\n            get(h),\n        );\n}\n",
            ),
        ] {
            let all = joined(src);
            assert!(
                all.contains(needle),
                "`{needle}` was not rebuilt — a scan for it would match nothing:\n{all}"
            );
        }
    }

    /// A bracket in a char literal must not open a statement that never closes.
    ///
    /// `s.matches('(')` is unremarkable Rust, and an unbalanced depth made every following line
    /// join the same statement — so a scan over it reported needles at whatever line the run-on
    /// had started, and a meta-guard built on this flagged three files that were fine. Caught
    /// because this module's own body contains that call and therefore mis-parsed itself.
    #[test]
    fn a_bracket_in_a_char_literal_does_not_run_the_statement_on() {
        let src = "fn f() {\n    let n = s.matches('(').count();\n    let m = 1;\n}\n";
        let stmts = parts(src);
        assert!(
            stmts
                .iter()
                .any(|s| s.contains("let n =") && !s.contains("let m")),
            "the char-literal statement ran on and swallowed what followed it: {stmts:?}"
        );
    }

    /// A binding broken after the `=` is the same failure wearing different clothes — **and it is
    /// what stops the paren fix being written too broadly.**
    ///
    /// `let msg =` + `"text"` must rejoin as `let msg = "text"`. The obvious way to make
    /// `Command::new(` + `"…"` rejoin is to suppress the separator before every `"`, which would
    /// produce `let msg ="text"` here and blind the translation guard instead. Only an open paren
    /// suppresses it, and this test is what fails if someone widens that rule.
    ///
    /// A second fixture was added for that trade and then removed: it differed only by a trailing
    /// `.to_string()`, drove the identical branch, and asserted the identical string, so it failed
    /// and passed in lockstep with this one. Two tests moving together are one test and one
    /// liability.
    #[test]
    fn a_binding_split_after_the_equals_is_read_as_one_statement() {
        let src = "fn f() {\n    let msg =\n        \"Screen time is up.\".to_string();\n}\n";
        assert!(joined(src).contains("let msg = \""), "not rejoined");
    }

    /// The reported line is the statement's first, not its last.
    #[test]
    fn a_statement_is_reported_at_the_line_it_starts_on() {
        let src = "fn f() {\n    let _ = a\n        .b(\n            1,\n        );\n}\n";
        let (line, stmt) = statements(src)
            .into_iter()
            .find(|(_, s)| s.contains("a.b("))
            .expect("the joined statement must be present");
        assert_eq!(line, 2, "reported at the wrong end of `{stmt}`");
    }

    /// The three shapes that are **not** a test module, each of which truncated a scan once.
    #[test]
    fn production_source_cuts_only_at_a_real_test_module() {
        for (label, text) in [
            (
                "a doc comment naming the attribute",
                "//! `#[cfg(test)]`\nfn real() {}\n",
            ),
            (
                "an indented test-only helper",
                "fn a() {}\n    #[cfg(test)]\n    fn h() {}\n",
            ),
            (
                "a cfg(test) module that is not `mod tests`",
                "#[cfg(test)]\nmod testutil;\nfn a() {}\n",
            ),
        ] {
            assert_eq!(production_source(text), text, "cut early at {label}");
        }
        let real = "fn keep() {}\n#[cfg(test)]\nmod tests {\n    let msg = \"x\";\n}\n";
        assert_eq!(production_source(real), "fn keep() {}");
    }
}
