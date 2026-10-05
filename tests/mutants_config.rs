//! Guards on `.cargo/mutants.toml`, the configuration both mutation-testing jobs read.
//!
//! The file excludes the modules that are compiled on Windows only, because cargo-mutants does not
//! evaluate `cfg` and every runner that mutates this crate is Linux: a mutant in such a module is
//! never compiled, passes the unchanged tests, and is reported as a survivor nothing can kill. That
//! exclusion is only right while the list matches the code, in both directions — a new
//! Windows-only module left off it floods the weekly run with false survivors again, and a module
//! made cross-platform but left on it is code whose mutants are silently never tested.

use std::fs;
use std::path::{Path, PathBuf};

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Every module declared `#[cfg(windows)] mod name;` anywhere under `src/`, as the file it loads.
///
/// Scans every file rather than the two that hold such declarations today, so the first one
/// written somewhere new is found too. The path follows Rust's own rule: a module declared in
/// `lib.rs`, `main.rs` or a `mod.rs` lives beside it; one declared in `x.rs` lives in `x/`.
fn windows_only_modules() -> Vec<String> {
    let mut files = Vec::new();
    rust_files(&root().join("src"), &mut files);
    let mut found = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            // `mod name;`, with any visibility in front. Split on the keyword rather than matching
            // each spelling of the visibility: a needle like `pub(crate) mod` reads as a call to
            // `tests/scanner_guards.rs`, and a declaration is never broken across lines anyway.
            let Some((visibility, rest)) = line.trim().split_once("mod ") else {
                continue;
            };
            if !(visibility.is_empty() || visibility.starts_with("pub")) {
                continue;
            }
            let Some(name) = rest.strip_suffix(';') else {
                continue;
            };
            let windows = lines[..i]
                .iter()
                .rev()
                .take_while(|above| above.trim().starts_with("#["))
                .any(|above| above.trim() == "#[cfg(windows)]");
            if !windows {
                continue;
            }
            let stem = file.file_stem().unwrap().to_str().unwrap();
            let dir = if ["lib", "main", "mod"].contains(&stem) {
                file.parent().unwrap().to_path_buf()
            } else {
                file.with_extension("")
            };
            let flat = dir.join(format!("{name}.rs"));
            let module = if flat.exists() {
                flat
            } else {
                dir.join(name).join("mod.rs")
            };
            assert!(
                module.exists(),
                "cannot find the file for `mod {name}` declared in {}",
                file.display()
            );
            found.push(
                module
                    .strip_prefix(root())
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .replace('\\', "/"),
            );
        }
    }
    found.sort();
    found
}

/// The `exclude_globs` array in `.cargo/mutants.toml`, read by hand: one key, a list of quoted
/// strings, and no reason to add a TOML parser to the dev-dependencies to read it.
fn excluded_globs() -> Vec<String> {
    let text = fs::read_to_string(root().join(".cargo/mutants.toml")).unwrap();
    let start = text
        .find("exclude_globs")
        .expect(".cargo/mutants.toml has no exclude_globs");
    let open = start + text[start..].find('[').unwrap();
    let close = open + text[open..].find(']').unwrap();
    let mut globs: Vec<String> = text[open + 1..close]
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect();
    globs.sort();
    globs
}

#[test]
fn the_mutation_runs_exclude_exactly_the_windows_only_modules() {
    let modules = windows_only_modules();
    assert!(
        modules.len() >= 4,
        "found only {modules:?}; the scan is broken, not the code"
    );
    assert_eq!(
        excluded_globs(),
        modules,
        "`.cargo/mutants.toml` must exclude exactly the modules compiled on Windows only. One \
         missing floods the weekly run with survivors nothing on Linux can kill; one extra is \
         cross-platform code whose mutants are silently never tested."
    );
}
