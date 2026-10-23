//! Test-only helpers for the library's own unit tests.
//!
//! Unrelated things live here, for one shared reason: each must be reachable from every
//! `#[cfg(test)]` module in the crate and from nothing else. [`ScratchDir`], used by nine modules,
//! [`assert_all_lists_every_variant`], shared by `Language::ALL` in `config.rs` and
//! `ShotTier::ALL` in `control/mod.rs`, and [`retry_while_busy`], for the tests in `control` that
//! write a script and then run it.
//!
//! **`ScratchDir` is a deliberate twin of the one in `tests/common/mod.rs`, not an oversight.** The
//! two cannot be one: `tests/common/mod.rs` is compiled into each integration-test binary, and
//! this module is `#[cfg(test)]` inside the library, so neither can see the other. Sharing one
//! copy would mean shipping test code in the release binary behind a feature flag, which costs
//! more than the twenty lines it saves. If you are here to remove the duplication, that is why it
//! is here.

/// A temporary directory that removes itself however the test ends.
///
/// Manual cleanup on the last line of a test body only runs when the test *reaches* that line. An
/// assertion failure is a panic, so the run that most wants its scratch data kept is the only one
/// that leaks it. Seventeen unit tests across eight modules hand-rolled the
/// `temp_dir().join(...)` / `create_dir_all` dance before this existed, and eight of them —
/// every site in `jsonl.rs`, plus `screentime.rs` — never deleted anything at all, on any path.
/// They are why a developer's `$TMPDIR` fills with `nw-jsonl-*` directories.
///
/// **Bind it to a name, never to `_`.** `let _ = ScratchDir::new(..)` drops the value immediately
/// and deletes the directory before the test uses it; `let _dir = ..` keeps it to end of scope.
/// The difference is invisible to the test suite — both spellings pass — because the observable
/// is filesystem state after the process exits, which no in-process assertion can reach. Measured
/// on `password_change_end_to_end`: bare `_` leaves one directory behind, `_dir` leaves none.
pub(crate) struct ScratchDir {
    path: std::path::PathBuf,
}

/// Every directory currently held by a live [`ScratchDir`] in this process.
///
/// Two guards on one path is silent corruption, not a clash: `new` clears the directory it is
/// given, so the second test to start deletes the first test's data mid-run and the first fails
/// somewhere unrelated. That is not hypothetical -- converting `timereq.rs` to this guard, an
/// inline `ScratchDir::new("timereq-cap")` collided with `scratch("cap")`, which builds the same
/// name, and two cap tests failed with `left: 0, right: 5` and an empty queue. Nothing pointed at
/// the directory.
static LIVE: std::sync::Mutex<std::collections::BTreeSet<std::path::PathBuf>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

impl ScratchDir {
    /// `tag` separates tests running concurrently inside one binary; the process id already
    /// separates the binaries from each other.
    pub(crate) fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("nw-{tag}-{}", std::process::id()));
        assert!(
            LIVE.lock().unwrap().insert(path.clone()),
            "two live ScratchDirs share {path:?} -- tag {tag:?} is already in use by another \
             test in this binary. `new` clears the directory, so the second one to start would \
             delete the first one's data mid-run and the failure would surface somewhere else \
             entirely. Give this one a distinct tag."
        );
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    /// A path inside the directory. The file need not exist.
    pub(crate) fn join(&self, name: &str) -> std::path::PathBuf {
        self.path.join(name)
    }

    /// The directory itself, for tests that hand the whole path to something else.
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
        // Released, not leaked: a test that finishes frees its tag for a later one to reuse.
        // Uses `lock()` defensively -- a panicking test poisons the mutex, and a poisoned lock
        // here must not turn one failure into every later test aborting in `drop`.
        if let Ok(mut live) = LIVE.lock() {
            live.remove(&self.path);
        }
    }
}

/// Assert that a hand-written `ALL` array really does list every variant of its enum.
///
/// `src` is the module's own source via `include_str!`, `decl` the enum's declaration line up to
/// and including the brace — `"pub enum Language {"`. The variants are counted out of that source
/// rather than compared against a second hand-written list, for the reason `tests/spawn_paths.rs`
/// gives: a fixture that mirrors a list in the code passes forever once the two drift, and the
/// drift is silent.
///
/// Two enums need this and their exposure is *not* the same, which is why the check lives here
/// rather than beside either of them. `Language` is caught by the compiler at every `match`, so
/// `ALL` is the only place a new variant can slip past. `ShotTier` is the worse case: `as_arg` is
/// `match self` and does fail to compile, but `from_arg` matches on the wire *string* with a `_`
/// arm, so a third tier would parse as `Full` with no error raised anywhere. Its round-trip test
/// is named `every_tier_survives_the_wire_spelling`; this assertion is the only thing that keeps
/// that name true.
pub(crate) fn assert_all_lists_every_variant(src: &str, decl: &str, all_len: usize) {
    let body = src
        .split_once(decl)
        .unwrap_or_else(|| panic!("no enum declared as `{decl}` in this source"))
        .1
        .split_once("\n}")
        .expect("unterminated enum body")
        .0;
    let variants: Vec<&str> = body
        .lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("//")
                && !l.starts_with("#[")
                && l.ends_with(',')
                && !l.contains(' ')
        })
        .collect();
    // A broken extractor must not make this vacuous — the same guard `spawn_paths` carries.
    assert!(
        variants.len() >= 2,
        "extracted {variants:?} from the body of `{decl}`; the parser is broken, not the code"
    );
    assert_eq!(
        variants.len(),
        all_len,
        "`{decl}` declares {variants:?} but its `ALL` has {all_len} entries"
    );
}

/// Assert that every language produced its own wording rather than a shared one.
///
/// The failure it exists for is quiet: a `match lang` whose arms were filled in by copying the
/// English one leaves a Dutch install reading English, and every other assertion in such a test
/// still passes.
///
/// **Not sufficient on its own.** Three distinct strings can still include an empty one, so every
/// caller pairs this with an absolute assertion per language — `!msg.trim().is_empty()`, or the
/// number the sentence has to state. A migration that drops that pairing makes a test weaker, not
/// tidier: `control::child_notice_title` was covered only by an `assert_ne!` against the parent's
/// heading, which `""` satisfies, and cargo-mutants found it on the 0.9.0 push.
///
/// **The migration to this helper is outstanding, not finished, and this comment says so on
/// purpose.** Four call sites use it — two in `probe.rs`, two in `control/mod.rs`, the first of
/// which was the `BTreeSet` copy that prompted the extraction. Six hand-rolled copies remain, in
/// two idioms: a `clone/sort/dedup` in
/// `rules::tests::every_reason_an_app_was_closed_is_explained_in_every_language`, and an index
/// loop with an inner `assert_ne!` — three in
/// `rules::tests::every_language_has_its_own_lock_and_limit_wording`, one each in
/// `curfew::tests::every_language_has_its_own_bedtime_title` and
/// `curfew::tests::every_language_has_its_own_bedtime_shutdown_notice`. An earlier version of this
/// comment read as though those had been converted. They had not, and a comment that describes
/// work as done is worse than no comment: it stops the next reader looking. Named by symbol rather
/// than line: every line number this comment used to carry had drifted by the time anyone checked.
pub(crate) fn assert_each_language_differs<T: Ord + std::fmt::Debug>(said: &[T]) {
    let unique: std::collections::BTreeSet<&T> = said.iter().collect();
    assert_eq!(
        unique.len(),
        said.len(),
        "each language needs its own wording, not a shared one: {said:?}"
    );
}

/// Run `attempt` again while it fails with "Text file busy", for about a second at most.
///
/// Linux refuses to execute a file that any process holds open for writing (`ETXTBSY`). A test
/// that writes a script closes its own handle before running it, but if another test thread starts
/// a process while that handle is open, the child inherits a copy and keeps it until it reaches
/// `exec` (rust-lang/rust#114554). For that moment the script is busy through no fault of the code
/// under test. The weekly mutation run caught two probe tests failing this way on unmutated code,
/// and each time an unrelated mutant was counted as killed. The window is one fork long, so a short
/// retry closes it. Any other result, success or failure, comes back from the first attempt that
/// produces it. macOS never reports the error, so there it always does.
pub(crate) fn retry_while_busy<T, E: std::fmt::Display>(
    mut attempt: impl FnMut() -> Result<T, E>,
) -> Result<T, E> {
    for _ in 0..BUSY_RETRIES {
        match attempt() {
            Err(e) if e.to_string().contains("Text file busy") => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            other => return other,
        }
    }
    attempt()
}

/// How many busy answers [`retry_while_busy`] waits out before handing the last one back.
const BUSY_RETRIES: usize = 50;

#[cfg(test)]
mod tests {
    use super::{BUSY_RETRIES, ScratchDir, retry_while_busy};

    /// A busy answer is waited out, and the first real answer is what comes back.
    #[test]
    fn a_busy_script_is_tried_again_until_it_answers() {
        let mut calls = 0;
        let got = retry_while_busy(|| {
            calls += 1;
            if calls < 3 {
                Err("could not start probe /tmp/x: Text file busy (os error 26)")
            } else {
                Ok("answer")
            }
        });
        assert_eq!(got, Ok("answer"));
        assert_eq!(calls, 3);
    }

    /// Any other failure is the test's to see, on the first attempt: retrying it would hide the
    /// very errors the probe tests exist to check.
    #[test]
    fn any_other_failure_comes_back_at_once() {
        let mut calls = 0;
        let got: Result<(), _> = retry_while_busy(|| {
            calls += 1;
            Err("probe exited with status 3")
        });
        assert_eq!(got, Err("probe exited with status 3"));
        assert_eq!(calls, 1);
    }

    /// A file that stays busy is reported busy after the last retry, not waited on for ever.
    #[test]
    fn a_script_that_stays_busy_is_given_up_on() {
        let mut calls = 0;
        let got: Result<(), _> = retry_while_busy(|| {
            calls += 1;
            Err("Text file busy (os error 26)")
        });
        assert_eq!(got, Err("Text file busy (os error 26)"));
        assert_eq!(calls, BUSY_RETRIES + 1);
    }

    /// The directory exists while the guard is held and is gone once its scope closes.
    ///
    /// This pins the contract the rest of the suite depends on and cannot see. Every other test
    /// using a `ScratchDir` would pass whether or not `Drop` fired -- they assert about the data
    /// they wrote, not about the directory afterwards -- so a `Drop` that silently stopped
    /// working would leave the whole suite green while `$TMPDIR` filled up again. That is how the
    /// seventeen hand-rolled sites this replaced went unnoticed for the life of the project.
    ///
    /// The inner scope is what makes it observable in-process: the guard is dropped at the closing
    /// brace, so the second assertion runs after cleanup without needing a separate process.
    ///
    /// **It does not catch a caller that writes `let _ = ScratchDir::new(..)`.** That drops the
    /// guard immediately rather than at scope end, and no assertion inside the caller's own body
    /// can observe the difference -- the app recreates the directory on first write, so the test
    /// passes and only the leftover directory afterwards betrays it. The doc on `ScratchDir` is
    /// the whole guard there.
    #[test]
    fn the_directory_lives_exactly_as_long_as_the_guard() {
        let path = {
            let dir = ScratchDir::new("scope-probe");
            let p = dir.path().to_path_buf();
            assert!(
                p.exists(),
                "the directory must exist while the guard is held"
            );
            std::fs::write(p.join("marker"), b"x").unwrap();
            p
        };
        assert!(
            !path.exists(),
            "the directory must be gone once the guard's scope closes, and {path:?} is still there"
        );
    }

    /// A tag freed by one test can be taken by the next.
    ///
    /// The collision guard would otherwise be a one-shot: `Drop` has to release the name, or the
    /// second of two *sequential* tests sharing a tag panics on a clash that never existed.
    #[test]
    fn a_tag_is_released_when_its_guard_drops() {
        drop(ScratchDir::new("reuse-probe"));
        drop(ScratchDir::new("reuse-probe"));
    }
}
