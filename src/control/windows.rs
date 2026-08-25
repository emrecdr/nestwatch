//! The real Windows implementation of [`SystemControl`].
//!
//! Compiled only on Windows (`#[cfg(windows)]` at the module declaration). Uses `xcap`
//! for screen capture, `sysinfo` for process enumeration/termination, and shells out to
//! `shutdown.exe` for power-off (dependency-free, no `unsafe`, no `windows` crate).

use super::{ControlError, ProcessInfo, RunningProcess, SessionState, ShotTier, SystemControl};

pub struct WindowsControl;

impl WindowsControl {
    pub fn new() -> Self {
        Self
    }
}

/// Declare this process per-monitor DPI aware, exactly once, before the first capture.
///
/// Without it the process is DPI *unaware*, which puts it in DPI virtualisation: its desktop device
/// context is sized in logical pixels. `xcap` meanwhile takes the capture rectangle from
/// `EnumDisplaySettingsW` — `dmPosition`, `dmPelsWidth`, `dmPelsHeight` — which are **physical**
/// device pixels and DPI-independent by definition. The two disagree on every scaled display: the
/// code asks for a rectangle larger than the surface it is reading from, and at the 150% Windows
/// picks by default for a 4K laptop panel more than half the requested area lies outside it.
///
/// Called from `screenshot` rather than `new()` so it cannot be skipped by a future caller that
/// constructs the controller differently, and `Once` so repeated captures pay nothing. The result is
/// deliberately ignored: it fails if awareness was already set (by a manifest, or by a second call),
/// and in every one of those cases the desired state already holds. There is nothing to recover
/// from and nothing worth logging on a path that runs before every screenshot.
#[allow(clippy::let_underscore_untyped)]
fn ensure_dpi_aware() {
    use std::sync::Once;
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };

    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: Win32 DPI FFI. `SetProcessDpiAwarenessContext` takes a constant context handle
        // by value, borrows no memory we own, and returns a BOOL. It is process-wide and must run
        // before any window or DC is created in this process — `Once` from inside the capture path
        // is the earliest point every caller shares.
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
    });
}

impl SystemControl for WindowsControl {
    fn screenshot(&self, tier: ShotTier) -> Result<Vec<u8>, ControlError> {
        use xcap::Monitor;

        ensure_dpi_aware();

        let monitors = Monitor::all().map_err(|e| ControlError::Capture(e.to_string()))?;
        // The *primary* monitor, which is what `SystemControl::screenshot` promises — not
        // whichever one enumerated first. `Monitor::all` is backed by `EnumDisplayMonitors`, which
        // returns monitors in display-settings order; the primary is not guaranteed to lead, so on
        // a two-screen setup this used to watch an arbitrary one, possibly forever and with nothing
        // in the UI to say so.
        //
        // Falls back to the first entry rather than erroring: a machine where the flag cannot be
        // read should still yield a picture. `unwrap_or(false)` on each probe for the same reason —
        // one unreadable monitor must not hide the others.
        let monitor = monitors
            .iter()
            .find(|m| m.is_primary().unwrap_or(false))
            .or_else(|| monitors.first())
            .ok_or_else(|| ControlError::Capture("no monitor found".into()))?;

        let captured = monitor
            .capture_image()
            .map_err(|e| ControlError::Capture(e.to_string()))?;

        // Bridge via raw RGBA bytes so we don't couple to xcap's exact `image` version:
        // `into_raw()` yields a plain `Vec<u8>`, which we re-wrap with *our* `image` crate.
        let (width, height) = (captured.width(), captured.height());
        let raw = captured.into_raw();
        let rgba = image::RgbaImage::from_raw(width, height, raw)
            .ok_or_else(|| ControlError::Capture("unexpected frame buffer size".into()))?;

        // Sized and encoded HERE, in the child's session, before the bytes reach the pipe home.
        // See `SystemControl::screenshot` — this is the difference between 47 KiB and 20,641 KiB
        // crossing it.
        super::encode_shot(image::DynamicImage::ImageRgba8(rgba), tier)
    }

    fn list_processes(&self) -> Result<Vec<ProcessInfo>, ControlError> {
        use sysinfo::{ProcessesToUpdate, System};

        let mut sys = System::new();
        sys.refresh_processes(ProcessesToUpdate::All, true);

        let mut out: Vec<ProcessInfo> = sys
            .processes()
            .iter()
            .map(|(pid, proc_)| ProcessInfo {
                pid: pid.as_u32(),
                name: proc_.name().to_string_lossy().into_owned(),
                memory_bytes: proc_.memory(),
            })
            .collect();
        // Heaviest first — the apps a parent most likely wants to see/close.
        out.sort_by_key(|p| std::cmp::Reverse(p.memory_bytes));
        Ok(out)
    }

    fn running_processes(&self) -> Result<Vec<RunningProcess>, ControlError> {
        use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

        let mut sys = System::new();
        // `nothing()`, not the `refresh_processes` default this used to share with
        // `list_processes`. That default expands to
        // `memory + cpu + disk_usage + exe(OnlyIfNotSet) + tasks`, and on Windows each is a syscall
        // per process: `GetProcessTimes` twice (start time, then CPU), `GetSystemTimes`,
        // `GetProcessIoCounters`, `GetProcessMemoryInfo`, `GetModuleFileNameExW`. Several hundred
        // processes, every thirty seconds, forever — for two fields.
        //
        // `pid` and `name` survive `nothing()` because both come straight out of the
        // `CreateToolhelp32Snapshot` walk, before any refresh kind is consulted. Neither needs a
        // process handle.
        //
        // A fresh `System` per call is kept deliberately, though reusing one would skip more still.
        // sysinfo holds a `PROCESS_QUERY_INFORMATION | PROCESS_VM_READ` handle open for every live
        // process for as long as the `System` lives, and a SYSTEM service permanently holding a few
        // hundred read handles to everything on the machine is a well-known EDR heuristic. Being
        // quarantined by antivirus costs a family more than these syscalls do.
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );

        Ok(sys
            .processes()
            .iter()
            .map(|(pid, proc_)| RunningProcess {
                pid: pid.as_u32(),
                name: proc_.name().to_string_lossy().into_owned(),
            })
            .collect())
    }

    fn kill_process(&self, pid: u32) -> Result<(), ControlError> {
        use sysinfo::{Pid, ProcessesToUpdate, System};

        // Refresh only the target PID rather than walking the whole process table.
        let target = Pid::from_u32(pid);
        let mut sys = System::new();
        sys.refresh_processes(ProcessesToUpdate::Some(&[target]), true);

        let Some(proc_) = sys.process(target) else {
            return Err(ControlError::ProcessNotFound(pid));
        };
        if proc_.kill() {
            Ok(())
        } else {
            Err(ControlError::Op(format!("failed to kill pid {pid}")))
        }
    }

    fn lock_workstation(&self) -> Result<(), ControlError> {
        // Shell out (dependency-free, no FFI) — this locks the session of the *calling*
        // process. When invoked directly it locks the current desktop; under the SYSTEM
        // service it is launched inside the user's session by the helper (see
        // `service_control` + `session::lock_active_session`).
        let status = std::process::Command::new(crate::syspath::system32("rundll32.exe"))
            .arg("user32.dll,LockWorkStation")
            .status()
            .map_err(|e| ControlError::Op(e.to_string()))?;
        if status.success() {
            Ok(())
        } else {
            Err(ControlError::Op(format!("lock exited with {status}")))
        }
    }

    fn shutdown(&self, delay_secs: u32, message: Option<String>) -> Result<(), ControlError> {
        // `/t N` gives Windows' own countdown; `/c "msg"` shows the user a reason.
        let delay = delay_secs.to_string();
        let mut cmd = std::process::Command::new(crate::syspath::system32("shutdown.exe"));
        cmd.args(["/s", "/t", &delay]);
        if let Some(msg) = message.as_deref() {
            // Windows truncates the comment at 512 chars.
            cmd.args(["/c", &msg.chars().take(512).collect::<String>()]);
        }
        let status = cmd.status().map_err(|e| ControlError::Op(e.to_string()))?;
        if status.success() {
            Ok(())
        } else {
            Err(ControlError::Op(format!("shutdown exited with {status}")))
        }
    }

    fn abort_shutdown(&self) -> Result<(), ControlError> {
        // `shutdown /a` cancels a pending shutdown; it exits non-zero ("no shutdown in
        // progress", 1116) when there is nothing to cancel — which is fine, so best-effort.
        std::process::Command::new(crate::syspath::system32("shutdown.exe"))
            .arg("/a")
            .output()
            .map_err(|e| ControlError::Op(e.to_string()))?;
        Ok(())
    }

    fn session_state(&self) -> Result<SessionState, ControlError> {
        // Queries the active console session via WTS. Works whether we're the interactive
        // process (dev `run`) or the SYSTEM service — the same call is used by both.
        crate::session::active_session_state()
    }

    fn notify_user(&self, title: String, body: String) -> Result<(), ControlError> {
        crate::session::notify_active_session(&title, &body)
    }
}
