//! Absolute paths to the Windows system executables this tool shells out to.
//!
//! Everything here exists to avoid passing a **bare program name** to [`std::process::Command`].
//! Rust resolves a bare name (`"shutdown"`) by searching, in order:
//!
//! 1. an explicit `PATH` set on the `Command` (we never set one),
//! 2. **the directory holding the current executable**,
//! 3. `GetSystemDirectory()` — `C:\Windows\System32`,
//! 4. `GetWindowsDirectory()` — `C:\Windows`,
//! 5. the inherited `PATH`.
//!
//! The current working directory is deliberately *not* searched by Rust, so the classic
//! CWD-planting attack doesn't apply. Step 2 is the one that matters: **the application's own
//! directory outranks `System32`.**
//!
//! For the installed service that directory is `C:\Program Files\HostHealth\`, which `install`
//! ACL-hardens to SYSTEM+Administrators (Users get read+execute only) — so a standard user can't
//! plant anything there and the running service was never actually exposed. The real window is
//! the *other* two callers: `install` and `doctor` run **elevated**, from wherever the parent
//! left `nestwatch.exe`. A `netsh.exe` or `icacls.exe` sitting next to it in a directory the
//! child can write to — `C:\Users\Public\Downloads`, a shared folder, a USB stick — would run
//! with full administrator rights.
//!
//! That's a narrow, conditional path rather than a live hole, and the documented install flow
//! (`cd $env:USERPROFILE\Downloads`) is not child-writable. But the whole class disappears for
//! the cost of one `GetSystemDirectoryW` call, and this binary's job is to resist someone who
//! has an account on the machine and time to experiment.
//!
//! Windows-only: every caller is already behind `#[cfg(windows)]`.

use std::path::PathBuf;

use windows::Win32::System::SystemInformation::GetSystemDirectoryW;

/// Where Windows says its system directory is.
///
/// Deliberately uncached: this runs immediately before spawning a process, a handful of times a
/// day, so the syscall is lost in the noise of `CreateProcess` and a `OnceLock` would be pure
/// ceremony on a cold path.
fn system_dir() -> PathBuf {
    // `GetSystemDirectoryW` returns the character count written (excluding the NUL), or the
    // required buffer size if it didn't fit, or 0 on failure. MAX_PATH is generous here:
    // the answer is `C:\Windows\System32` on every real system.
    let mut buf = [0u16; 260];
    // SAFETY: `buf` is a live, correctly-sized slice for the duration of the call, and the
    // return value is validated against its length before being used as an index.
    let len = unsafe { GetSystemDirectoryW(Some(&mut buf)) } as usize;
    if len == 0 || len > buf.len() {
        // Falling back to a hardcoded guess is still strictly better than a bare name: it is
        // at least an absolute path that skips the application-directory search.
        return PathBuf::from(r"C:\Windows\System32");
    }
    PathBuf::from(String::from_utf16_lossy(&buf[..len]))
}

/// Absolute path to a `System32` executable — e.g. `system32("shutdown.exe")`.
///
/// Pass the full file name including `.exe`, so what's on disk is what's named here.
pub fn system32(exe: &str) -> PathBuf {
    system_dir().join(exe)
}

/// Absolute path to Windows PowerShell.
///
/// It lives in a subdirectory of `System32` rather than in `System32` itself, which is why it
/// can't go through [`system32`]. This is deliberately *Windows* PowerShell (the one guaranteed
/// present since Windows 7), not PowerShell 7+ — `doctor` only runs built-in cmdlets.
pub fn powershell() -> PathBuf {
    system_dir().join(r"WindowsPowerShell\v1.0\powershell.exe")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These run for real on the `windows-latest` CI job (`cargo test --all-targets`), so they
    /// assert what a cross-compile can't: that these paths resolve to files on a real Windows
    /// machine.
    ///
    /// The equivalent check for [`system32`] deliberately lives in `tests/spawn_paths.rs`
    /// instead, because it has to iterate the names the *call sites* actually pass. A list
    /// written out here would only prove that some well-known files exist in `System32` — true
    /// on every Windows machine regardless of what this crate asks for.
    #[test]
    fn powershell_resolves_to_a_real_file() {
        let path = powershell();
        assert!(path.is_absolute(), "{} is not absolute", path.display());
        assert!(
            path.exists(),
            "{} does not exist — `doctor`'s network and administrator checks would report \
             'unknown' on every run",
            path.display()
        );
    }

    /// The system directory must not be a relative path, or joining onto it reintroduces exactly
    /// the search this module exists to avoid.
    #[test]
    fn the_system_directory_itself_is_absolute() {
        assert!(system_dir().is_absolute(), "{:?}", system_dir());
    }
}
