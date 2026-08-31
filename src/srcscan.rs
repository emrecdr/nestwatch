//! Reflow-tolerant primitives for the guards that read this crate's own source.
//!
//! # Why this module exists at all
//!
//! Several tests here defend a property by scanning the source: no program launched by bare name,
//! no unauthenticated route outside a known list, no child-facing string written in place of a
//! translation. Each looked for a needle naming a call — the bare-name spawn that `spawn_paths.rs`
//! polices, or the route registration that `server.rs` does — a needle that spans
//! a syntactic boundary — and each read the file a line at a time.
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
/// **Deliberately does not call [`production_source`]** — compose them when you want the cut:
/// `statements(production_source(text))`. A guard that polices *other guards* has to see inside
/// test modules, because that is exactly where a source scanner lives; cutting first made the
/// needles in `web.rs` and `install.rs` invisible and left a meta-guard's anti-vacuity check
/// resting on a single file.
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
        let scrubbed = trimmed.replace("'('", "").replace("')'", "");
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape every one of the three blind guards was defeated by.
    #[test]
    fn a_call_the_formatter_has_broken_up_is_read_as_one_statement() {
        let src = "fn f() {\n    let _ = control\n        .shutdown(\n            60,\n            \
                   Some(\"Bedtime.\".to_string()),\n        );\n}\n";
        let joined: Vec<String> = statements(src).into_iter().map(|(_, s)| s).collect();
        let all = joined.join("\n");
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
            let joined = statements(src)
                .into_iter()
                .map(|(_, s)| s)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                joined.contains(needle),
                "`{needle}` was not rebuilt — a scan for it would match nothing:\n{joined}"
            );
        }
    }

    /// …but a binding still keeps its spaces, which the paren fix must not break.
    ///
    /// `let msg =` + `"text"` has to rejoin as `let msg = "text"`. Suppressing the separator before
    /// every quote — the obvious way to fix the paren case — would produce `let msg ="text"` and
    /// blind the translation guard instead. Only an open paren suppresses it.
    #[test]
    fn a_binding_keeps_the_space_the_paren_fix_removes_elsewhere() {
        let src = "fn f() {\n    let msg =\n        \"Screen time is up.\";\n}\n";
        let joined = statements(src)
            .into_iter()
            .map(|(_, s)| s)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("let msg = \""),
            "not rejoined as a binding:\n{joined}"
        );
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
        let joined: Vec<String> = statements(src).into_iter().map(|(_, s)| s).collect();
        assert!(
            joined
                .iter()
                .any(|s| s.contains("let n =") && !s.contains("let m")),
            "the char-literal statement ran on and swallowed what followed it: {joined:?}"
        );
    }

    /// A binding broken after the `=` is the same failure wearing different clothes.
    #[test]
    fn a_binding_split_after_the_equals_is_read_as_one_statement() {
        let src = "fn f() {\n    let msg =\n        \"Screen time is up.\".to_string();\n}\n";
        let all = statements(src)
            .into_iter()
            .map(|(_, s)| s)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all.contains("let msg = \""), "not rejoined:\n{all}");
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
