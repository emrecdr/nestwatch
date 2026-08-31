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

/// The production source as whole statements, each paired with the line it starts on.
///
/// This is the reflow-tolerant reader. Lines are joined until parentheses balance and the text
/// ends a statement, so a call and the arguments handed to it stay in one unit however the
/// formatter has broken them up. The reported line is where the statement *starts*, which is the
/// one a reader wants to be sent to.
///
/// **No separator is inserted before a leading `.`, and that detail is load-bearing.** `rustfmt`
/// breaks a method chain *at* the dot, so joining with a space turns `control` + `.shutdown(` into
/// `control .shutdown(` — which contains `control.shutdown(` no more than the two separate lines
/// did. The first version of this joiner did exactly that: it reassembled the statement and then
/// broke it again with its own separator, and the probe still passed while the code looked fixed.
pub fn statements(text: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let (mut buf, mut start, mut depth) = (String::new(), 0usize, 0i32);

    for (i, raw) in production_source(text).lines().enumerate() {
        let trimmed = raw.trim();
        if trimmed.starts_with("//") || (buf.is_empty() && trimmed.is_empty()) {
            continue;
        }
        if buf.is_empty() {
            start = i + 1;
        } else if !trimmed.starts_with('.') {
            buf.push(' ');
        }
        buf.push_str(trimmed);

        // Quotes are not tracked. A bracket inside a string literal can only *delay* the close,
        // which joins more text than needed; over-joining costs a noisier message, while the
        // under-joining this replaced cost the whole guarantee.
        depth += trimmed.matches('(').count() as i32 - trimmed.matches(')').count() as i32;
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
