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

use nestwatch::srcscan::{production_source, statements};

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

/// Which `pub async fn` in `src/auth.rs` each interesting statement belongs to.
///
/// **Two bugs deep, and both are the same bug.** The first version collected lines starting
/// `session.`, and rustfmt wraps a long call — so `session.insert(SCOPE_KEY, …)` is stored as a
/// bare `session` followed by `.insert(…)`, neither of which matches. It reported `login` as
/// missing a write three lines above the assertion. The second version scanned the body as one
/// string, which fixed that and still read the file with `.lines()` while hunting a needle of the
/// form `IDENT(` — the precise shape `scanner_guards.rs` exists to reject, and it did reject it.
///
/// So this uses [`nestwatch::srcscan::statements`], the reader that joins lines until parentheses
/// balance. The lesson is not about formatting: a guard that can be defeated by the formatter
/// reports success while matching nothing, which is worse than no guard, because a passing test
/// is read as evidence.
fn scope_writes_by_function(auth: &str) -> (Vec<(usize, String)>, Vec<String>) {
    let stmts = statements(production_source(auth));

    // Every `pub async fn` and the line it starts on, so a statement can be attributed to one.
    let functions: Vec<(usize, String)> = stmts
        .iter()
        .filter_map(|(line, stmt)| {
            let rest = stmt.strip_prefix("pub async fn ")?;
            Some((*line, rest.split('(').next().unwrap_or_default().to_owned()))
        })
        .collect();
    let owner = |line: usize| -> Option<String> {
        functions
            .iter()
            .rev()
            .find(|(at, _)| *at <= line)
            .map(|(_, name)| name.clone())
    };

    let authenticates = stmts
        .iter()
        .filter(|(_, stmt)| stmt.contains("insert(AUTH_KEY"))
        .filter_map(|(line, _)| owner(*line).map(|name| (*line, name)))
        .collect();
    let scopes = stmts
        .iter()
        .filter(|(_, stmt)| stmt.contains("insert(SCOPE_KEY"))
        .filter_map(|(line, _)| owner(*line))
        .collect();
    (authenticates, scopes)
}

/// Authenticating without recording an authority must stay impossible to do by accident.
///
/// **This guard replaces its own opposite, and the history is the point.** It used to assert that
/// `pair` and `login` left *identical* session state — four documents said so, nothing held them
/// to it, and the sameness was `O89`: a paired third-party app was indistinguishable from the
/// parent, so it could grant as `source=parent` and skip the registry, the day latch and the
/// daily ceiling together. It was written to go red on the commit that fixed that, and it did.
///
/// What replaces it is the invariant the fix depends on: **every site that writes `AUTH_KEY`
/// writes `SCOPE_KEY` in the same function.** A session carrying authentication but no authority
/// is refused by `require_auth`, so forgetting the second line does not open a hole — it creates
/// a credential that silently never works, which a parent meets as "pairing did nothing".
///
/// Deliberately a source scan and not a behaviour test: the behaviour is covered in
/// `tests/pairing_scope.rs`, and what this adds is *totality* — it fails on a third
/// authentication path nobody wrote a test for, which is the one that would actually get missed.
#[test]
fn nothing_authenticates_a_session_without_also_recording_what_it_may_do() {
    let auth = repo("src/auth.rs");
    let (authenticates, scopes) = scope_writes_by_function(&auth);

    assert_eq!(
        authenticates.len(),
        2,
        "expected exactly `login` and `pair` to authenticate, found {authenticates:?}. A third \
         path is not a failure — add it here once you have checked it records a scope."
    );
    for name in ["login", "pair"] {
        assert!(
            authenticates.iter().any(|(_, f)| f == name),
            "`{name}` no longer writes AUTH_KEY — this guard has gone blind rather than passed"
        );
    }
    for (line, name) in &authenticates {
        assert!(
            scopes.contains(name),
            "`{name}` (src/auth.rs:{line}) authenticates a session without recording its scope. \
             `require_auth` refuses a session with no scope, so this is not a hole — it is a \
             credential that pairs and then cannot do anything, which is worse to diagnose. \
             Write the scope beside AUTH_KEY."
        );
    }
}
