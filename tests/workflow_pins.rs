//! Guards on the release pipeline's supply chain, asserted against the workflow files themselves
//! and against the local composite actions they use.
//!
//! Both properties below already hold. They are pinned here because neither is checked by
//! anything else: a workflow is not compiled, not linted by `clippy`, and only "tested" by
//! running a release — which is the one moment you cannot afford to discover the problem.
//!
//! Both tests read text rather than parsed YAML, which is why composite actions need explicit
//! handling: a `uses:` moved into `.github/actions/x/action.yml` runs with identical privileges
//! and disappears from the file these scans read.

use std::fs;
use std::path::{Path, PathBuf};

/// Every `.github/workflows/*.yml`, as (name, contents).
fn workflows() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    let entries =
        fs::read_dir(&dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
    let mut out: Vec<(String, String)> = Vec::new();
    for entry in entries {
        let path: PathBuf = entry.expect("unreadable directory entry").path();
        let is_yaml = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e == "yml" || e == "yaml");
        if !is_yaml {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("workflow filename is not UTF-8")
            .to_owned();
        let body = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        out.push((name, body));
    }
    out.sort();
    assert!(
        out.len() >= 2,
        "found {} workflow files; the reader is broken, not the pipeline",
        out.len()
    );
    out
}

/// Every local composite action, as (name, contents).
///
/// Scanned beside the workflows because a `uses:` inside a composite action runs with exactly the
/// same privileges as one written in the workflow, and neither test below can see it otherwise.
/// A missing directory is not a failure — this repository need not have any.
fn composite_actions() -> Vec<(String, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/actions");
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = Vec::new();
    for entry in entries {
        let path: PathBuf = entry.expect("unreadable directory entry").path();
        for file in ["action.yml", "action.yaml"] {
            let candidate = path.join(file);
            if !candidate.is_file() {
                continue;
            }
            let dir_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .expect("action directory name is not UTF-8");
            let body = fs::read_to_string(&candidate)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", candidate.display()));
            out.push((format!("{dir_name}/{file}"), body));
        }
    }
    out.sort();
    out
}

/// The repository-local action a `uses:` line references, if it is one: `.github/actions/x`.
///
/// `action_ref` cannot answer this: a local reference carries no `@ref`, which is the whole point
/// — it is this tree's own code at this commit, so there is no version to pin.
fn local_action(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let rest = trimmed
        .strip_prefix("- uses:")
        .or_else(|| trimmed.strip_prefix("uses:"))?;
    let spec = rest.trim().split('#').next()?.trim();
    spec.strip_prefix("./")
        .filter(|p| p.starts_with(".github/"))
}

/// The `uses:` reference on a line, if it has one: `("owner/repo", "ref")`.
fn action_ref(line: &str) -> Option<(&str, &str)> {
    let rest = line.trim().strip_prefix("- uses:").or_else(|| {
        line.trim()
            .strip_prefix("uses:")
            .filter(|_| line.trim_start().starts_with("uses:"))
    })?;
    let spec = rest.trim().split('#').next()?.trim();
    spec.split_once('@')
}

fn is_sha(r: &str) -> bool {
    r.len() == 40 && r.chars().all(|c| c.is_ascii_hexdigit())
}

/// Third-party actions are pinned to a commit, everywhere.
///
/// A mutable tag (`@v1`, `@stable`) is a standing grant of arbitrary code execution to whoever
/// can move it — the maintainer, or anyone who takes that account. `actions/*` is GitHub's own
/// org and is left on tags deliberately: trusting it is already implied by running on GitHub at
/// all, and pinning it costs a bump for every security patch upstream ships. Everything else is
/// somebody else's account.
#[test]
fn every_third_party_action_is_pinned_to_a_commit() {
    let mut checked = 0usize;
    let mut floating = Vec::new();
    for (name, body) in workflows().into_iter().chain(composite_actions()) {
        for line in body.lines() {
            let Some((action, reference)) = action_ref(line) else {
                continue;
            };
            let owner = action.split('/').next().unwrap_or("");
            if owner == "actions" {
                continue;
            }
            checked += 1;
            if !is_sha(reference) {
                floating.push(format!("{name}: {action}@{reference}"));
            }
        }
    }
    // A broken parser must not be able to pass this by finding nothing.
    assert!(
        checked >= 4,
        "only matched {checked} third-party actions; the `uses:` parser is broken"
    );
    assert!(
        floating.is_empty(),
        "these run somebody else's code from a moving reference:\n  {}\nPin each to a full \
         commit SHA with a trailing `# vX.Y.Z` comment.",
        floating.join("\n  ")
    );
}

/// The job that can sign does not build.
///
/// This is the separation that makes the provenance mean something. An attestation proves which
/// workflow produced a file; it cannot prove that the workflow's earlier steps left the source
/// alone. If the signing job also compiles, then every build input — three third-party actions,
/// every `build.rs` in the dependency graph — executes inside a job holding `id-token: write`,
/// and can patch the tree before `cargo build` reads it. The attestation then signs the tampered
/// binary truthfully, and `gh attestation verify` passes. Keeping the two in separate jobs is
/// what SLSA calls Build L3, and it is the reason `release.yml` has a `build` job at all.
#[test]
fn the_signing_job_runs_no_build_step() {
    let (name, body) = workflows()
        .into_iter()
        .find(|(n, _)| n == "release.yml")
        .expect("release.yml must exist");

    // Jobs sit at exactly two spaces under `jobs:`; split the file on those headers.
    let mut jobs: Vec<(String, String)> = Vec::new();
    for line in body.lines() {
        let is_header = line.starts_with("  ")
            && !line.starts_with("   ")
            && line.trim_end().ends_with(':')
            && !line.trim().starts_with('#')
            && line.trim().split(':').next().is_some_and(|n| {
                !n.is_empty()
                    && n.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            });
        if is_header {
            jobs.push((line.trim().trim_end_matches(':').to_owned(), String::new()));
        } else if let Some(last) = jobs.last_mut() {
            // Comments are dropped, and that is load-bearing twice over. A comment block sits
            // *above* the job it describes, so at this point it has already been attributed to
            // the previous job — and both markers this test hunts for ("id-token: write",
            // "cargo build") are discussed in the prose of `release.yml` precisely because the
            // separation is worth explaining. Scanning the prose reports the explanation as the
            // violation. Found by this test failing on its own first run.
            if !line.trim_start().starts_with('#') {
                last.1.push_str(line);
                last.1.push('\n');
            }
        }
    }
    assert!(
        jobs.len() >= 3,
        "parsed {} jobs out of {name}; the splitter is broken, not the workflow",
        jobs.len()
    );

    let signing: Vec<&(String, String)> = jobs
        .iter()
        .filter(|(_, b)| b.contains("id-token: write"))
        .collect();
    assert_eq!(
        signing.len(),
        1,
        "expected exactly one job to hold the signing identity, found {}: {:?}",
        signing.len(),
        signing.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );

    let (job, steps) = signing[0];

    // A composite action runs build steps whose names never appear in this file, which is exactly
    // what the scan below reads. So expand every local action the signing job uses: moving
    // `npm ci` behind `uses: ./.github/actions/frontend-css` must not quietly satisfy this test.
    // A reference that resolves to no `action.yml` fails rather than expanding to nothing — a
    // renamed directory is a hole in this guard, not a pass.
    let mut steps = steps.clone();
    for line in steps.clone().lines() {
        let Some(rel) = local_action(line) else {
            continue;
        };
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
        let file = ["action.yml", "action.yaml"]
            .iter()
            .map(|f| dir.join(f))
            .find(|p| p.is_file())
            .unwrap_or_else(|| {
                panic!(
                    "job `{job}` uses `./{rel}`, which holds no action.yml — this guard cannot \
                     read what that step runs, so it cannot say the job does not build"
                )
            });
        steps.push_str(
            &fs::read_to_string(&file)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", file.display())),
        );
    }

    // Anything that turns source into the artifact. `cargo build` is the one that matters; the
    // toolchain and Node setups are listed because a build step cannot appear without them, so
    // they fail earlier and point at the cause more directly.
    for marker in [
        "cargo build",
        "npm ci",
        "npm run build",
        "rust-toolchain",
        "setup-node",
        "setup-nasm",
    ] {
        assert!(
            !steps.contains(marker),
            "job `{job}` holds `id-token: write` and also runs `{marker}`. Building beside the \
             signing identity is what the provenance cannot detect — move the build into the \
             `build` job and pass the artifact across."
        );
    }
}
