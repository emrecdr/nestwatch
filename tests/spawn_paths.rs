//! Guards the invariant that `src/syspath.rs` exists to establish.
//!
//! That module resolves every Windows system tool to an absolute path, because Rust resolves a
//! bare program name by searching the *current executable's own directory* before `System32` —
//! and `install`/`doctor` run elevated from wherever the parent left `nestwatch.exe`.
//!
//! On its own that's only a convention: nothing stops the next `Command::new("shutdown")` from
//! reintroducing it, and the failure would be invisible (the command still works, on the wrong
//! binary). This test is the enforcement. It reads the crate's own sources, so it runs on
//! **every** CI job rather than only the Windows one — the guard is cross-platform even though
//! the thing it guards is not.

use std::path::Path;

use nestwatch::srcscan::{find_tokens, line_of};

mod common;
use common::crate_sources;

/// The string literal opening the call matched at `at`, which the caller has already established
/// begins with a quote.
///
/// An unterminated literal cannot compile, so it only arrives by someone editing a scan. It is
/// still reported rather than dropped: losing a hit because the name could not be read would be
/// these guards failing open one level below the failure they exist to close.
fn literal_after(text: &str, at: usize) -> &str {
    let open = at + text[at..].find('"').expect("caller matched a quote token");
    let rest = &text[open + 1..];
    rest.find('"')
        .map_or("<unterminated literal>", |end| &rest[..end])
}

/// Every place `text` spawns a program by a **bare name** — `Command::new("…")` with a literal
/// rather than a resolved path — as the line the call is on and the program it names.
///
/// Returning the program, not just the position, is what lets the failure identify the offending
/// binary. It used to return offsets and the caller printed the starting line's text, which for a
/// call the formatter had split is `Command::new(` and nothing else — so the one report whose job
/// is to name the binary did not contain it, in exactly the case this scan exists to catch.
///
/// **Scans the whole text, not line by line, and that is the entire point.** This guard read
/// `text.lines()` looking for `Command::new("` until 2026-08-31, when the same blindness was found
/// in two sibling scanners on the same day: `server.rs`'s unauthenticated-route scan and
/// `translated_strings.rs`'s child-string scan. `rustfmt` breaks a call the moment it outgrows the
/// width, and
///
/// ```ignore
/// std::process::Command::new(
///     "a-bare-name-long-enough-to-push-this-call-past-one-hundred-columns.exe",
/// )
/// ```
///
/// contains no line holding `Command::new("` at all. Verified by inserting exactly that into
/// `src/syspath.rs` against the previous version of this test and watching it report success.
///
/// The trigger is not exotic: a call acquires that shape automatically once its argument is long,
/// and a long argument is precisely what an unqualified program name looks like when someone
/// spells out an executable rather than reaching for `syspath`.
///
/// Nor is it hypothetical. `install.rs::configure_recovery` carries exactly that shape today —
/// `Command::new(` on one line and its argument on the next, put there by the formatter rather than
/// by anyone's choice. That one is `syspath`-resolved, so it is correctly not a hit; what it establishes is
/// that the reflowed shape is ordinary code in this tree, so the tolerance below is exercised
/// against the real sources and not only against the fixtures.
///
/// Its neighbour [`scan_call_sites`] had the same weakness and survived it by luck rather than by
/// care: comparing `system32(` against `system32("` per line meant a broken call produced a count
/// mismatch and landed in the bucket that asserts, so it failed **closed** while this one failed
/// **open**. Same file, same blind spot, opposite consequences — which is the argument for both
/// reading tokens now instead of each carrying its own idea of what a call looks like.
///
/// # Why this reads tokens rather than text
///
/// `find_tokens` matches its tokens in order separated only by whitespace, which is the whole of
/// what reflow tolerance requires: whitespace is the only thing `rustfmt` can insert between the
/// pieces of a call. It reports the offset of the match itself, so the failure below names the line
/// the call is on rather than the line some enclosing construct started on, and it skips comment
/// lines already.
///
/// This scan hand-rolled all three of those until `f76ba07` landed the primitive. The hand-roll is
/// gone rather than defended: keeping a private copy of a shared reflow rule is what let three
/// guards in this crate drift into failing open on the same day.
fn bare_name_spawns(text: &str) -> Vec<(usize, &str)> {
    find_tokens(text, &["Command::new", "(", "\""])
        .into_iter()
        .map(|at| (line_of(text, at), literal_after(text, at)))
        .collect()
}

/// The scan must see both shapes — otherwise the guard below is whatever `rustfmt` last decided.
///
/// Fixtures rather than a probe in `src/`: this asserts the *detector* directly, so it keeps
/// holding after the production tree stops happening to contain an example of either form. A
/// guard whose non-vacuity depends on the code it guards is one refactor from testing nothing.
#[test]
fn the_bare_name_scan_sees_a_call_the_formatter_has_broken_up() {
    let one_line = r#"let s = Command::new("shutdown").status();"#;
    assert_eq!(
        bare_name_spawns(one_line),
        vec![(1, "shutdown")],
        "one-line form missed"
    );

    let reflowed = "let s = std::process::Command::new(\n    \"shutdown.exe\",\n)\n.status();";
    assert_eq!(
        bare_name_spawns(reflowed),
        vec![(1, "shutdown.exe")],
        "a call the formatter split across lines was either missed, or found without the program \
         it names — the first is the defect this scan was rewritten to close, the second leaves \
         the failure printing `Command::new(` and nothing else, which is the same blindness one \
         level down"
    );

    // A resolved path is the whole point of `syspath`, and must not be reported.
    let resolved = "let s = Command::new(syspath::system32(\"shutdown.exe\")).status();";
    assert!(
        bare_name_spawns(resolved).is_empty(),
        "a `syspath`-resolved call was reported as a bare name"
    );

    // A doc example is prose, not a call site.
    let commented = "/// nothing stops the next Command::new(\"shutdown\") from returning";
    assert!(
        bare_name_spawns(commented).is_empty(),
        "a doc comment was reported as a call site"
    );

    // An unterminated literal cannot compile, so this shape only ever arrives by someone editing
    // the scan itself. It must still be reported: dropping the hit because the name could not be
    // read would be this guard failing open again, one level below the failure it was rewritten
    // to close. Pinned because the branch is otherwise unreachable from the tree.
    let unterminated = "let s = Command::new(\"never-closed";
    assert_eq!(
        bare_name_spawns(unterminated),
        vec![(1, "<unterminated literal>")],
        "a spawn whose literal never closes was dropped instead of reported"
    );
}

#[test]
fn no_source_file_spawns_a_program_by_bare_name() {
    let mut offenders = Vec::new();
    for (file, text) in crate_sources(&["src"]) {
        for (line, program) in bare_name_spawns(&text) {
            offenders.push(format!("  {}:{line} — spawns `{program}`", file.display()));
        }
    }

    assert!(
        offenders.is_empty(),
        "a process is spawned by bare name, which Rust resolves against the running \
         executable's own directory before System32:\n{}\n\nUse `crate::syspath::system32(\"…\")` \
         (or `syspath::powershell()`) so there is nothing to search. See src/syspath.rs.",
        offenders.join("\n")
    );
}

/// Every name the crate passes to `syspath::system32("…")`, with the call site it came from.
///
/// Derived from the call sites rather than hand-listed. This started life as a hardcoded array
/// inside `syspath.rs`'s own tests, which was very nearly a tautology: it proved that five
/// well-known files exist in `System32` — true on every Windows machine, whatever this crate
/// does — while claiming to check "the paths we hand to `Command`". A typo at a call site left
/// that array untouched, so CI stayed green and the shell-out failed silently on the child's PC.
/// To say anything about *our* code, the list has to come from the code.
fn requested_system_binaries() -> Vec<(String, String)> {
    let (found, unreadable) = scan_call_sites();
    assert!(
        unreadable.is_empty(),
        "a `system32(…)` argument is not a string literal, so this test cannot check it exists \
         and would skip it silently:\n{}\n\nPass a literal, or extend this scan to follow the \
         indirection — an unchecked call site is how a typo reaches the child's PC.",
        unreadable.join("\n")
    );
    found
}

/// Returns (literal arguments with their call site, call sites whose argument isn't a literal).
///
/// Counting both matters: extracting only literals would quietly ignore `system32(SOME_CONST)`,
/// leaving that call site unverified while the test still passed — the same silent-gap shape
/// this whole check exists to remove.
///
/// **Every call, then the subset opening with a literal.** The difference is what cannot be
/// checked. Reading it as two token searches rather than as counts-per-line is what makes it
/// reflow-proof: the old form compared `system32(` against `system32("` on a single line, which
/// happened to fail closed when the formatter split a call — a broken call yielded a count mismatch
/// and landed in `unreadable`, so it complained rather than went quiet. That was luck, not design,
/// and it was the last line-oriented scan in this file.
fn scan_call_sites() -> (Vec<(String, String)>, Vec<String>) {
    let (mut found, mut unreadable) = (Vec::new(), Vec::new());
    for (path, text) in crate_sources(&["src"]) {
        let opens_with_literal = find_tokens(&text, &["system32", "(", "\""]);
        for at in find_tokens(&text, &["system32", "("]) {
            // `fn system32(` is the definition, not a call.
            if text[..at].trim_end().ends_with("fn") {
                continue;
            }
            let site = format!("{}:{}", path.display(), line_of(&text, at));
            if opens_with_literal.contains(&at) {
                found.push((literal_after(&text, at).to_string(), site));
            } else {
                unreadable.push(format!("  {site} — first argument is not a string literal"));
            }
        }
    }
    (found, unreadable)
}

/// The existence check can only run on Windows, but the extraction runs everywhere — so a broken
/// extractor (which would make the Windows check vacuous) fails on every platform.
#[test]
fn every_system_binary_the_code_asks_for_is_a_real_file() {
    let requested = requested_system_binaries();
    assert!(
        !requested.is_empty(),
        "found no `system32(\"…\")` call sites — the extractor is broken, so this test would \
         pass no matter what the code asked Windows for"
    );

    #[cfg(windows)]
    {
        for (name, at) in &requested {
            let path = nestwatch::syspath::system32(name);
            assert!(
                path.exists(),
                "{at} asks for `{name}`, which resolves to {} — no such file. A typo here is \
                 invisible until enforcement silently stops working on the child's PC.",
                path.display()
            );
        }
    }
}

/// Pre-flight must know about every tool the crate shells out to.
///
/// `check_system_tools` reports missing Windows tools before install touches anything, from a
/// list written by hand. A hand-written list of what the code does is the exact shape this file
/// exists to distrust — and it had already fallen behind: `shutdown.exe` and `rundll32.exe`,
/// the two the curfew needs to lock or shut the PC down, were absent from it. Nothing checked
/// them anywhere, so a stripped image would have installed cleanly and then done nothing at
/// bedtime.
///
/// Derived from the call sites for the same reason as the scan above.
#[test]
fn preflight_knows_about_every_tool_the_crate_shells_out_to() {
    let preflight = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/preflight.rs");
    // Normalised, because this scan looks for raw "\n}\n" rather than going line by line.
    // Windows checkouts have CRLF (the runner sets core.autocrlf), so the unnormalised form
    // matched nothing and the test failed on CI having passed everywhere else. The other scans
    // in this file use `str::lines`, which strips the `\r` for them.
    let text = std::fs::read_to_string(&preflight)
        .unwrap_or_else(|e| panic!("reading {}: {e}", preflight.display()))
        .replace("\r\n", "\n");

    let start = text
        .find("fn check_system_tools")
        .expect("src/preflight.rs no longer defines check_system_tools — this test is stale");
    let body = &text[start..start + text[start..].find("\n}\n").expect("unterminated fn")];

    let mut names: Vec<String> = requested_system_binaries()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    names.sort();
    names.dedup();
    assert!(
        names.len() >= 4,
        "only {} distinct tools found — the extractor is broken and this proves nothing",
        names.len()
    );

    let unchecked: Vec<&String> = names
        .iter()
        .filter(|n| !body.contains(&format!("system32(\"{n}\")")))
        .collect();
    assert!(
        unchecked.is_empty(),
        "these tools are invoked somewhere in the crate but never pre-checked, so a machine \
         missing one installs cleanly and fails later, with no error pointing at the cause: \
         {unchecked:?}\n\nAdd them to `check_system_tools` in src/preflight.rs — as a blocker if \
         install needs them, as a caution if only enforcement does."
    );
}
