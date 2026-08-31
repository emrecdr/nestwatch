//! Guards the guards: a source scanner must not be defeated by the code formatter.
//!
//! # The class this exists for
//!
//! Several tests here defend a property by scanning this crate's own source for a needle —
//! `Command::new("`, `.route("`, `control.shutdown(`. Every one of those needles **spans a
//! syntactic boundary**, and `rustfmt` breaks a call at exactly such a boundary the moment it
//! outgrows the line width. A scan that reads `text.lines()` then matches nothing, and the guard
//! reports success over precisely the code it exists to catch.
//!
//! Three guards were found blind this way on 2026-08-31, by two people working independently, each
//! confirmed by inserting a probe and watching the shipped test pass. They were fixed. A manual
//! sweep then established there was no fourth.
//!
//! **That sweep is the thing worth keeping, and a sweep done once by hand is not kept.** This test
//! is that sweep, run on every push, so a fifth scanner written the old way fails on the day it
//! lands rather than waiting for somebody to independently have the idea again. `O79` records the
//! reasoning; [`nestwatch::srcscan`] is the shared answer this points people at.
//!
//! # What it does not claim
//!
//! It is a lint over a heuristic, not a proof. It finds a needle that spans a boundary in a file
//! that also reads lines, which is the shape all three real instances took. A scanner that
//! constructs its needle at runtime, or splits it across constants, is invisible here — as is one
//! written in a language this does not read. The trade is deliberate: the rule has no false
//! positives across the tree today, and a guard that cries wolf is one somebody deletes.

use std::path::{Path, PathBuf};

use nestwatch::srcscan::{production_source, statements};

/// Files whose needle-shaped literal is **known safe**, each with the reason it is safe.
///
/// Named individually rather than matched by pattern, and that is the point: a pattern-shaped
/// exemption would silently swallow the next real instance that happened to look similar. Adding a
/// row here is a decision someone has to write a sentence about.
///
/// **The exemption is per file, not per needle, and that is a real limitation rather than a
/// simplification.** `tests/spawn_paths.rs` is listed for its `system32(` needles, and that listing
/// also covers its `Command::new("` needle — so if the reflow-tolerance there were ever reverted,
/// this guard would not notice. Per-needle exemptions would close that, at the cost of an
/// exemption list keyed on the very strings it is policing, which rots differently. Listed as the
/// trade it is; the file's own fixture test is what actually holds that one.
const KNOWN_SAFE: [(&str, &str); 3] = [
    (
        "tests/spawn_paths.rs",
        "`system32(` is counted against `system32(\"` per line, so a call the formatter has broken \
         yields one call and no literal, trips the count mismatch, and lands in the bucket that \
         asserts. It fails CLOSED — by construction rather than by care, which is why it is listed \
         rather than trusted.",
    ),
    (
        "src/web.rs",
        "`budgetTone(`/`limitTone(`/`gamePortal(` are matched against the whole markup, not line \
         by line, and every assertion on them requires the needle to be PRESENT. A reflow makes \
         these fail, not pass.",
    ),
    (
        "src/install.rs",
        "`alternate_note(` is matched against whole text with `contains`, and the assertion \
         requires it to be present, so a reflow fails the test rather than silencing it.",
    ),
];

/// Every `.rs` file under `dir`, recursively.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// A string literal of the form `IDENT(` — an identifier immediately followed by an open paren.
///
/// That is the whole vulnerable shape: the only place `rustfmt` can insert a break inside such a
/// needle is between the name and its paren, or between the paren and the first argument. A needle
/// without a `(` cannot be split by the formatter and is not interesting here.
fn holds_a_call_shaped_needle(stmt: &str) -> bool {
    let bytes = stmt.as_bytes();
    let mut i = 0usize;
    while let Some(open) = stmt[i..].find('"') {
        let start = i + open + 1;
        let Some(close_rel) = stmt[start..].find('"') else {
            return false;
        };
        let lit = &stmt[start..start + close_rel];
        // `foo(` or `foo::bar(` or `foo.bar(`, optionally with the opening quote of an argument.
        if let Some(paren) = lit.find('(') {
            let name = &lit[..paren];
            // A leading `.` is allowed, and that is not an edge case: `.route("` is the needle
            // that defeated `server.rs`, and a method call is the single most likely thing for the
            // formatter to break. Requiring an alphabetic first character rejected the real one.
            let bare = name.trim_start_matches(['.', ':']);
            if !bare.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == ':' || c == '.')
                && bare
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic() || c == '_')
            {
                return true;
            }
        }
        i = start + close_rel + 1;
        if i >= bytes.len() {
            break;
        }
    }
    false
}

#[test]
fn a_scanner_with_a_call_shaped_needle_reads_statements_not_lines() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    rust_files(&root.join("tests"), &mut files);
    assert!(
        !files.is_empty(),
        "no sources found — this test would pass forever"
    );

    let mut offenders = Vec::new();
    let mut needles_seen = 0usize;

    for path in files {
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(&path).unwrap_or_default();

        // **Cut at the test module, and this is the guard's honest limit rather than an
        // oversight.** A source scanner usually lives inside `#[cfg(test)]`, so cutting there
        // hides some of what this exists to find — `O79` records it. Scanning the whole file was
        // tried and produces false positives the rule cannot adjudicate: a test *fixture* holding
        // a needle string, and a whole-text `split_once` in a file that uses `.lines()` somewhere
        // unrelated, both look identical to a real line-oriented scanner at this resolution.
        // Separating them needs the needle tied to a specific loop, which is a different rule
        // rather than a tweak to this one. A guard with false positives gets deleted; a guard with
        // a written-down blind spot gets improved.
        let stmts = statements(production_source(&text));

        // Adoption is a `use` ITEM, not any occurrence of the name. Matching the bare text let a
        // single COMMENT naming the module switch the guard off for a whole file — demonstrated by
        // adding one to `spawn_paths.rs` and watching it stop being checked. A comment cannot
        // import anything, so requiring the import closes it.
        let adopted = stmts
            .iter()
            .any(|(_, s)| s.starts_with("use ") && s.contains("srcscan"));
        let excused = KNOWN_SAFE.iter().find(|(f, _)| *f == rel);

        for (line, stmt) in &stmts {
            if !holds_a_call_shaped_needle(stmt) {
                continue;
            }
            // Counted BEFORE the skips, so the anti-vacuity check below measures the detector
            // rather than measuring whatever the exemptions happen to leave behind.
            needles_seen += 1;
            // Line-oriented reading is the hazard; a whole-text scan is not one.
            if !text.contains(".lines()") {
                continue;
            }
            if !adopted && excused.is_none() {
                offenders.push(format!(
                    "{rel}:{line}\n    {}\n    ^ a needle of the form `IDENT(` in a file that reads \
                     `.lines()`. rustfmt can break a call at exactly that point, and the scan would \
                     then match nothing while reporting success. Use `nestwatch::srcscan::statements`, \
                     or add the file to KNOWN_SAFE with the reason it fails closed.",
                    stmt.chars().take(120).collect::<String>()
                ));
            }
        }
    }

    // Anti-vacuity: if the detector stops recognising the shape, it finds nothing anywhere and
    // passes forever. The known-safe files guarantee there is always something to find.
    assert!(
        needles_seen > 0,
        "found no call-shaped needle anywhere, including in the files KNOWN_SAFE names — the \
         detector has stopped working and this guard is checking nothing"
    );
    assert!(
        offenders.is_empty(),
        "{} scanner(s) can be defeated by the formatter:\n\n{}\n",
        offenders.len(),
        offenders.join("\n\n")
    );
}

/// The detector recognises the shape, asserted directly rather than via the tree.
///
/// Without this, the guard's non-vacuity depends on the crate happening to contain an example —
/// and the day someone fixes the last one, it starts passing over nothing.
#[test]
fn the_needle_detector_knows_the_shape_from_its_neighbours() {
    for yes in [
        r#"let n = "Command::new(";"#,
        r#"if t.contains("control.shutdown(") {"#,
        r#"const N: &str = "control::notify(";"#,
        r#"let n = ".route(\"";"#,
    ] {
        assert!(
            holds_a_call_shaped_needle(yes),
            "should have matched: {yes}"
        );
    }
    for no in [
        r#"let msg = "Bedtime - this computer is shutting down.";"#,
        r#"let tag = "progress-error";"#,
        r#"let s = "(not a call)";"#,
        r#"let empty = "";"#,
    ] {
        assert!(
            !holds_a_call_shaped_needle(no),
            "should NOT have matched: {no}"
        );
    }
}
