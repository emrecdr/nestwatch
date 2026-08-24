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

use std::path::{Path, PathBuf};

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

/// Every `.rs` file under `src/`, paired with its contents.
fn sources() -> Vec<(PathBuf, String)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    assert!(
        !files.is_empty(),
        "no sources found under {} — these tests would silently pass forever",
        src.display()
    );
    files
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
            (path, text)
        })
        .collect()
}

#[test]
fn no_source_file_spawns_a_program_by_bare_name() {
    let mut offenders = Vec::new();
    for (file, text) in sources() {
        for (n, line) in text.lines().enumerate() {
            if line.contains("Command::new(\"") {
                offenders.push(format!("  {}:{} — {}", file.display(), n + 1, line.trim()));
            }
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
fn scan_call_sites() -> (Vec<(String, String)>, Vec<String>) {
    let (mut found, mut unreadable) = (Vec::new(), Vec::new());
    for (path, text) in sources() {
        for (n, line) in text.lines().enumerate() {
            // Doc examples mention the call shape without being call sites.
            if line.trim_start().starts_with("//") {
                continue;
            }
            let at = format!("{}:{}", path.display(), n + 1);
            // `fn system32(` is the definition, not a call.
            let calls = line.matches("system32(").count() - line.matches("fn system32(").count();
            let literals = line.matches("system32(\"").count();
            if calls > literals {
                unreadable.push(format!("  {at} — {}", line.trim()));
            }
            for tail in line.split("system32(\"").skip(1) {
                if let Some((name, _)) = tail.split_once('"') {
                    found.push((name.to_string(), at.clone()));
                }
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
    let text = std::fs::read_to_string(&preflight)
        .unwrap_or_else(|e| panic!("reading {}: {e}", preflight.display()));

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
