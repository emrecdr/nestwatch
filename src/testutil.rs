//! Test-only helpers for the library's own unit tests.
//!
//! **This is a deliberate twin of `ScratchDir` in `tests/common/mod.rs`, not an oversight.** The
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

#[cfg(test)]
mod tests {
    use super::ScratchDir;

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
