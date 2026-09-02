//! Guards on prose that states a checkable fact about the code, asserted against the code.
//!
//! `install.rs::the_docs_name_the_binary_this_install_actually_writes` and
//! `remotesetup.rs::plaintext_rules_are_matched_filters_first` already do this for two claims, and
//! the first states the principle outright: *"prose that states a fact about the code needs
//! something holding the two together."* This file applies that principle to the claims nothing was
//! holding, rather than introducing a new idea.
//!
//! Markdown is in the same category `tests/workflow_pins.rs` describes for workflow files: not
//! compiled, not linted by `clippy`, and only "tested" by a reader believing it. The difference is
//! that a wrong workflow fails a release loudly, while wrong prose is read and acted on. All three
//! properties below already hold as of 2026-09-02; each is here because it did **not** hold days
//! earlier and nothing noticed:
//!
//!   * `README.md` said `argon2 0.5` for the several days after the crate moved to `0.6` — a
//!     security-adjacent claim, in the most-read file, wrong in the direction of understating what
//!     is deployed.
//!   * `docs/OPEN-FINDINGS.md`'s "Release state" header said `v0.5.1` after `v0.6.0` shipped. That
//!     paragraph's entire job is to say which released artifact the findings below are open
//!     against, so it is the one line in the file that cannot be allowed to drift.
//!   * `O71` cited four measurements of `assets/app.js` and its markup. All four were stale within
//!     six days, understating the component by 22–43%.
//!
//! **What is deliberately not pinned.** Prose that is a judgement rather than a fact, and numbers
//! whose drift is the point rather than an error. `DECLINED-OPTIONS.md` refuses a coverage
//! percentage gate on the reasoning that a threshold either never fires or fires for the wrong
//! reason; the `O71` bound below is one-sided and generous for exactly that reason — it fires only
//! when the file has grown well past its own citation, and the fix is to re-measure two numbers.

use std::fs;
use std::path::Path;

fn repo(rel: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Every dependency in `Cargo.toml`, as (name, version).
///
/// Hand-parsed rather than via a TOML crate: adding a dependency so a test can read the dependency
/// list is the wrong trade for one table, and `release.yml` already reads this same file with `awk`
/// for the same reason. Only `[*dependencies]` tables are scanned, so `rust-version` and the
/// package's own `version` cannot be mistaken for a crate's.
fn cargo_dependencies() -> Vec<(String, String)> {
    let manifest = repo("Cargo.toml");
    let mut out = Vec::new();
    let mut in_deps = false;

    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_deps = line.contains("dependencies");
            continue;
        }
        if !in_deps || line.starts_with('#') || line.is_empty() {
            continue;
        }
        let Some((name, rest)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            continue;
        }
        // Either `foo = "1.2"` or `foo = { version = "1.2", ... }`.
        let rest = rest.trim();
        let version = match rest.find("version") {
            Some(i) => quoted(&rest[i..]),
            None => quoted(rest),
        };
        if let Some(v) = version {
            out.push((name.to_owned(), v));
        }
    }

    assert!(
        out.len() > 20,
        "parsed only {} dependencies from Cargo.toml; the reader is broken, not the manifest",
        out.len()
    );
    out
}

/// The first `"..."`-quoted run in `s`.
fn quoted(s: &str) -> Option<String> {
    let start = s.find('"')? + 1;
    let end = s[start..].find('"')? + start;
    Some(s[start..end].to_owned())
}

/// Where `text` names `crate <version>`, that version must be a prefix of the manifest's.
///
/// Prefix rather than equality because the README quotes the series a reader cares about — `axum
/// 0.8` for a manifest pin of `0.8.9` is correct and should stay writable. `argon2 0.5` against
/// `0.6` is not a prefix, which is the case this exists to catch.
///
/// A crate name is only considered where a version-shaped token follows it immediately, so ordinary
/// prose using a word that happens to be a crate name (`time`, `image`, `pem`) cannot match.
/// Verified against the real README before this was written: 12 mentions matched, 11 agreed, and
/// the single disagreement was the defect.
#[test]
fn the_readme_names_the_crate_versions_this_build_actually_uses() {
    let readme = repo("README.md");
    let deps = cargo_dependencies();
    let mut checked = 0usize;

    for (name, manifest_version) in &deps {
        let manifest_version = manifest_version.trim_start_matches(['^', '~', '=']);
        let mut from = 0usize;
        while let Some(i) = readme[from..].find(name.as_str()) {
            let at = from + i;
            from = at + name.len();

            // The match must be a whole word, not a substring of a longer name — otherwise
            // `axum` matches inside `axum-server` and compares against the wrong pin.
            let before_ok = at == 0
                || !readme[..at]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_');
            if !before_ok {
                continue;
            }

            // The separator is REQUIRED, not optional. A version claim in prose is always
            // `name 1.2`; nothing writes `name1.2`. Without this, `nestwatch.exe.sha256` parses as
            // the crate `sha2` at version `56` — which is exactly what this test reported the first
            // time it ran, and the reason the space is spelled out here rather than tolerated.
            let Some(rest) = readme[from..].strip_prefix(' ') else {
                continue;
            };
            let rest = rest.strip_prefix('v').unwrap_or(rest);
            let claimed: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            let claimed = claimed.trim_end_matches('.');
            if claimed.is_empty() || !claimed.contains(|c: char| c.is_ascii_digit()) {
                continue;
            }

            checked += 1;
            assert!(
                manifest_version.starts_with(claimed),
                "README.md says `{name} {claimed}`, Cargo.toml says `{manifest_version}`.\n\
                 Update whichever is wrong. This is the check that was missing when the README \
                 said `argon2 0.5` for days after the crate moved to 0.6."
            );
        }
    }

    assert!(
        checked >= 8,
        "only {checked} crate-version claims found in README.md; the scanner stopped matching, \
         which would make this test pass by reading nothing"
    );
}

/// `OPEN-FINDINGS.md` must say which release its findings are open against, and mean this one.
///
/// The file's own preamble calls itself "a task list, not a history" and requires every entry to be
/// true of the tree right now. The release-state header is the one line that scopes all of them, so
/// a stale version there mis-scopes the whole file. It went stale at `v0.6.0` because the release
/// commit touched `CHANGELOG.md`, `Cargo.lock`, `Cargo.toml` and `README.md` and nothing else —
/// this makes the next release commit fail until the header comes with it.
#[test]
fn the_findings_release_state_names_this_version() {
    let findings = repo("docs/OPEN-FINDINGS.md");
    let version = env!("CARGO_PKG_VERSION");

    let start = findings
        .find("## Release state")
        .expect("docs/OPEN-FINDINGS.md has no `## Release state` section");
    // Bound the search to the section, so a version mentioned elsewhere cannot satisfy this.
    let section_end = findings[start + 1..]
        .find("\n## ")
        .map(|i| start + 1 + i)
        .unwrap_or(findings.len());
    let section = &findings[start..section_end];

    assert!(
        section.contains(&format!("`v{version}`")),
        "Cargo.toml is at {version}, but the `## Release state` section of \
         docs/OPEN-FINDINGS.md does not name `v{version}`.\n\
         Bumping the version is part of cutting a release; so is saying which release the open \
         findings are open against."
    );
}

/// `O71` cites the size of the dashboard component. Those numbers must not understate it badly.
///
/// One-sided and generous on purpose. Growth is the failure mode — the entry is an argument about
/// size, so a citation smaller than reality weakens it while a larger one cannot occur by drift.
/// 15% is wide enough that ordinary edits never fire this and narrow enough that the 22% drift
/// which prompted it would have.
///
/// When this fires: re-count, edit the numbers in `O71`, and update the date beside them. That is
/// the whole fix — it is a prompt to re-measure, not a budget to stay under.
#[test]
fn o71_line_counts_have_not_drifted_past_its_citation() {
    let findings = repo("docs/OPEN-FINDINGS.md");
    let start = findings
        .find("### O71 ")
        .expect("docs/OPEN-FINDINGS.md has no O71 entry");
    let end = findings[start + 1..]
        .find("\n### ")
        .map(|i| start + 1 + i)
        .unwrap_or(findings.len());
    let entry = &findings[start..end];

    // (what the entry describes, the file it describes, the phrase the number precedes)
    let claims = [
        ("assets/app.js", "assets/app.js", "lines** registering"),
        ("its markup", "assets/index.html", "lines of markup"),
        (
            "web/test/app.test.js",
            "web/test/app.test.js",
            "lines** exercising",
        ),
    ];

    for (what, path, anchor) in claims {
        let cited = cited_number_before(entry, anchor).unwrap_or_else(|| {
            panic!(
                "O71 no longer states a line count before \"{anchor}\" — if the entry was \
                    rewritten, update this test with it"
            )
        });
        let actual = repo(path).lines().count();
        let ceiling = cited + cited * 15 / 100;
        assert!(
            actual <= ceiling,
            "O71 says {what} is {cited} lines; it is now {actual} ({}% more).\n\
             Re-measure the numbers in O71 and update the date beside them.",
            (actual - cited) * 100 / cited.max(1)
        );
    }
}

/// The number immediately before `anchor`, written `**2,501 ` or `2,501 `.
fn cited_number_before(entry: &str, anchor: &str) -> Option<usize> {
    let at = entry.find(anchor)?;
    let head = &entry[..at];
    let digits: String = head
        .chars()
        .rev()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .filter(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.chars().rev().collect::<String>().parse().ok()
}

/// The session mutations inside `src/auth.rs`'s `pub async fn name`, in source order.
///
/// Bounded by the first line that is exactly `}`, which is the function's own closing brace —
/// every line of a body is indented, so nothing inside can end the scan early.
fn session_writes(auth: &str, name: &str) -> Vec<String> {
    let head = format!("pub async fn {name}(");
    let start = auth
        .find(&head)
        .unwrap_or_else(|| panic!("`{name}` is no longer a `pub async fn` in src/auth.rs"));
    let body = &auth[start..];
    let end = body
        .find("\n}")
        .unwrap_or_else(|| panic!("`{name}` has no closing brace at column 0"));
    body[..end]
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("session."))
        .map(|line| line.split(".await").next().unwrap_or(line).to_owned())
        .collect()
}

/// Four documents say a paired device holds exactly the parent's authority. This holds that
/// sentence to `src/auth.rs` instead of to four people's memories.
///
/// `pair`'s own comment calls itself "the same privilege transition as a password login", and
/// `O89` is filed on the consequence: the phone app that pushes earned time stores the cookie
/// pairing minted, so the principal that pushes and the principal that configures the registry
/// are one. That claim is now restated at code-symbol precision in `re_anchor`'s doc comment,
/// `docs/SECURITY.md`, `docs/PLUGIN-SYSTEM.md` and `O89` — four hand-maintained copies of a fact
/// about two functions, which is the exact shape this file exists to stop drifting.
///
/// **It is written to fire when `O89` is fixed, and that is the point.** Marking a paired session
/// — `O89`'s candidate 2 — means writing something in `pair` that `login` does not, so this test
/// goes red on the commit that repairs the finding rather than on some later one. Red here is not
/// a defect; it is the signal that four documents describing the old behaviour are now stale, and
/// the message names them.
#[test]
fn a_paired_session_still_carries_exactly_what_a_password_login_carries() {
    let auth = repo("src/auth.rs");
    let login = session_writes(&auth, "login");
    let pair = session_writes(&auth, "pair");

    assert!(
        !login.is_empty(),
        "no session writes found in `login` — this guard has gone blind rather than passed"
    );
    assert_eq!(
        pair, login,
        "`pair` and `login` no longer leave the same session state, so a paired device and a \
         password login are no longer the same principal.\n\nIf this is `O89` being fixed, this \
         is the commit that has to update the four places stating they are identical: \
         `api::re_anchor`'s doc comment, the paragraph under `docs/SECURITY.md`'s blast-radius \
         table, the correction in `docs/PLUGIN-SYSTEM.md`, and `O89` itself."
    );
}
