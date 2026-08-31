//! Guards the invariant that every string the **child** reads is translated.
//!
//! The convention this enforces already existed and was already followed almost everywhere: a
//! child-facing string is built by a small `*_message(lang)` function, and a unit test beside that
//! function asserts each language gets its own wording. `lock_warning_message`,
//! `limit_reached_message`, `budget_countdown_message`, `bedtime_message` and `bedtime_title` all
//! work that way.
//!
//! **The two that did not were invisible for exactly that reason.** `rules.rs` and `curfew.rs`
//! each built their shutdown notice as a bare literal instead of a function, so a Dutch install
//! showed a Dutch countdown, a Dutch lock warning, and then an English "shutting down" — at the
//! most stressful moment the child gets. No per-function guard could ever have caught it: the
//! missing function was both the reason the string was never translated *and* the reason nothing
//! noticed. A test can only check the functions that exist.
//!
//! So this guard is deliberately aimed at the **class** rather than at those two strings — the
//! standard advice for keeping hard-coded UI text from creeping back is a scan over the sources,
//! not another per-string assertion. It reads the crate's own text, like `spawn_paths.rs` does for
//! system binaries, which also means it runs on every CI job rather than only the Windows one.
//!
//! It is a lint, not a proof: it catches a literal *bound for* the child, not every conceivable
//! way one could reach them. That is the trade for having no false positives — see
//! `MESSAGE_BINDINGS`.

use std::path::{Path, PathBuf};

use nestwatch::srcscan::{production_source, statements};

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
        "no sources found under {} — this test would silently pass forever",
        src.display()
    );
    files
        .into_iter()
        .map(|p| {
            let text = std::fs::read_to_string(&p)
                .unwrap_or_else(|e| panic!("reading {}: {e}", p.display()));
            (p, text)
        })
        .collect()
}

/// The names a message takes in the moment before it is handed to the child.
///
/// Narrow on purpose. A broad "no string literals in these files" rule would flag log lines, JSON
/// keys and audit fields — none of which the child ever sees — and a guard that cries wolf gets
/// deleted. These three names are what the enforcers actually call the text they are about to
/// display, so the rule has no false positives across the whole crate while still catching the
/// real defect. Widen it if a fourth name appears; do not widen it to "any literal".
const MESSAGE_BINDINGS: [&str; 3] = ["msg", "body", "title"];

/// The calls that put text in front of the child.
///
/// `control::notify(` earns its place separately from `notify_child(`: the latter is `rules.rs`'s
/// own wrapper, but `curfew.rs` calls the underlying helper directly for the bedtime countdown, so
/// listing only the wrapper left the child's *other* notification path unwatched.
const CHILD_FACING_SINKS: [&str; 3] = ["control.shutdown(", "notify_child(", "control::notify("];

/// Whether `trimmed` binds `binding` to a string written in place rather than translated.
///
/// Covers `format!` as well as a plain literal. A bare `"…"` was how both real defects were
/// written, but the moment one of these messages needs to interpolate a countdown the natural
/// thing to reach for is `let msg = format!("… {secs} …")` — still English, still hard-coded, and
/// invisible to a check that only looked for an opening quote.
fn binds_a_literal(trimmed: &str, binding: &str) -> bool {
    let Some(rhs) = trimmed.strip_prefix(&format!("let {binding} = ")) else {
        return false;
    };
    rhs.starts_with('"') || rhs.starts_with("format!(\"")
}

#[test]
fn no_child_facing_string_is_written_in_place_of_a_translation() {
    let mut offenders = Vec::new();
    // Anti-tautology: if the sinks are ever renamed, this scan would find nothing to check and
    // pass forever while the guarantee quietly lapsed. Counting them makes that failure loud.
    let mut sinks_seen = 0usize;

    for (path, text) in sources() {
        for (n, stmt) in statements(&text) {
            let trimmed = stmt.as_str();
            let at = format!("{}:{n}", path.display());

            if MESSAGE_BINDINGS.iter().any(|b| binds_a_literal(trimmed, b)) {
                offenders.push(format!(
                    "{at}\n    {trimmed}\n    ^ a literal the child will read. Build it in a \
                     `*_message(lang)` fn beside the others and call that instead."
                ));
            }

            if CHILD_FACING_SINKS.iter().any(|s| trimmed.contains(s)) {
                sinks_seen += 1;
                if trimmed.contains('"') {
                    offenders.push(format!(
                        "{at}\n    {trimmed}\n    ^ a literal passed straight to the child. Same \
                         fix: a `*_message(lang)` fn."
                    ));
                }
            }
        }
    }

    assert!(
        sinks_seen > 0,
        "found none of {CHILD_FACING_SINKS:?} anywhere in src/ — the sinks were renamed and this \
         guard is no longer checking anything"
    );
    assert!(
        offenders.is_empty(),
        "{} child-facing string(s) bypass translation:\n\n{}\n",
        offenders.len(),
        offenders.join("\n\n")
    );
}

/// Every file is read down to its own test module, and nothing stops earlier.
///
/// The direct statement of what [`production_source`] is for, checked against the real tree rather
/// than against fixtures — so a *new* file introducing a fourth truncating shape fails here on the
/// day it lands, instead of quietly shrinking the guard above.
/// Asserted on the *remainder* rather than by recomputing the cut, deliberately. Deriving the
/// expected length from the same `split_once` the function uses would be a tautology — it would
/// agree with any bug, including the one this exists because of. What the leftover text begins
/// with is an independent fact: if the scan stopped anywhere other than a test module, the
/// remainder starts with something else and says so.
///
/// Cutting *late* is not checked here, and does not need to be: it can only pull test code into
/// the scan, which shows up as a loud false positive rather than as silent blindness.
#[test]
fn every_file_is_scanned_down_to_its_own_tests() {
    let mut cut = 0usize;
    for (path, text) in sources() {
        let rest = &text[production_source(&text).len()..];
        assert!(
            rest.is_empty() || rest.starts_with("\n#[cfg(test)]\nmod tests"),
            "{} stops being scanned at something that is not its test module — every \
             child-facing string below that point is unguarded. The scan resumes at:\n{}",
            path.display(),
            rest.lines().take(3).collect::<Vec<_>>().join("\n")
        );
        cut += usize::from(!rest.is_empty());
    }
    // Anti-tautology: if `production_source` ever stopped cutting at all, every assertion above
    // would pass on an empty remainder while the scan silently filled with test code.
    assert!(
        cut > 20,
        "only {cut} files were cut at a test module; this crate has ~29, so the cut has stopped \
         working and the scan is now reading unit tests as if they were production code"
    );
}
