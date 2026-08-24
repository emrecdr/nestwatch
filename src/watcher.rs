//! Windows-only: the **resident foreground watcher**, run as `nestwatch helper --watch` inside the
//! child's session by [`crate::session`].
//!
//! It answers one question, once every 30 seconds: which apps had focus, and for how long. The
//! accounting itself is [`crate::foreground::Tracker`], which is pure and unit-tested on any
//! platform; everything here is the Win32 shell that feeds it and the loop that reports.
//!
//! # Why this cannot live in the service
//!
//! `SetWinEventHook` receives events "from all processes **on the current desktop**", and a
//! Session-0 service is on `Service-0x0-3e7$\default`, which is not the child's desktop and never
//! becomes it. There is no flag or privilege that changes this — Interactive Service Detection,
//! which used to bridge it, was removed in Windows 10 build 1803. A process resident in the child's
//! session is the only arrangement that works, which is why one exists.
//!
//! It also fails *silently*: a hook registered from the wrong desktop returns a valid handle and
//! then never fires. That is worth knowing before debugging an empty report.
//!
//! # Shape of the loop
//!
//! A hook alone is not enough. Every shipping tracker (ActivityWatch, Cobalt, screenpipe) polls or
//! hybridises, because hooks miss transitions and `GetForegroundWindow` returns `NULL` outright
//! during UAC and at the lock screen. For a tiling window manager a missed event is a cosmetic
//! glitch; for screen-time accounting it is a silent under-count that always favours the child.
//!
//! So: the hook is an **edge trigger** that wakes the loop promptly, and a [`POLL`] timeout is the
//! **reconciliation** that catches whatever the hook missed. Either way the loop re-reads the
//! current foreground window rather than trusting the event, so a missed or duplicated event costs
//! latency and never accuracy.
//!
//! # What the callback may do
//!
//! Almost nothing. Microsoft: *"if a hook function does not process events quickly enough, USER
//! resources are lowered, eventually resulting in a fault or extremely slow response times"* — a
//! slow callback degrades **the whole desktop**, not just this process. It therefore does one
//! non-blocking send and returns. All resolution happens on the worker.

use std::io::Write;
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, EVENT_SYSTEM_FOREGROUND, GetForegroundWindow, GetMessageW, GetWindowTextW,
    GetWindowThreadProcessId, MSG, TranslateMessage, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS,
};
use windows::core::PWSTR;

use crate::foreground::Tracker;

/// How often the loop reconciles against the real foreground window when no event has arrived.
/// Deliberately not the 1-second poll ActivityWatch uses: the hook already provides the low-latency
/// edge, so this only has to catch what the hook missed.
const POLL: Duration = Duration::from_secs(5);

/// How often a sample is written to stdout. Matches the rules enforcer's own tick, so the service
/// folds in roughly one sample per tick and a crash costs at most one interval.
const EMIT: Duration = Duration::from_secs(30);

/// How long without input before the user counts as away. 180s matches ActivityWatch's default:
/// long enough that reading a page or watching a video still counts as use, short enough that a
/// PC abandoned with a game open stops accruing.
const IDLE_AFTER: Duration = Duration::from_secs(180);

/// Wake channel from the hook callback to the worker. A [`OnceLock`] because a `WinEventProc` is a
/// bare `extern "system"` fn with no user-data parameter — there is nowhere else to put it.
static WAKE: OnceLock<SyncSender<()>> = OnceLock::new();

/// Hook callback. Does one non-blocking send and returns; see the module docs on why it must not
/// do more.
///
/// The event is deliberately **ignored**. It is a hint that focus moved, not the value — the worker
/// re-reads `GetForegroundWindow`, so a duplicate or a stale `HWND` costs nothing.
unsafe extern "system" fn on_foreground_change(
    _hook: HWINEVENTHOOK,
    _event: u32,
    _hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // `extern "system"` is a non-unwinding ABI: a panic crossing this boundary aborts the process.
    // This process is the only thing measuring screen time, and it runs unattended on a child's
    // PC, so it must not die because a channel misbehaved. None of komorebi, glazewm, screenpipe
    // or Cobalt guards this; it is cheap and the failure mode is total.
    let _ = std::panic::catch_unwind(|| {
        if let Some(tx) = WAKE.get() {
            // `try_send`, never `send`: the worker may be mid-resolution, and a full channel
            // already means "there is an unprocessed wake-up", which is all a second one would say.
            let _ = tx.try_send(());
        }
    });
}

/// Run the watcher until the process is killed. Never returns in normal operation.
///
/// The hook is registered on **this** thread and pumped on **this** thread, because both are
/// required: *"the client thread that calls SetWinEventHook must have a message loop in order to
/// receive events"*, and *"for out-of-context events, the event is delivered on the same thread
/// that called SetWinEventHook"*. Registering here and pumping elsewhere yields a live handle that
/// never fires.
pub fn run() -> Result<()> {
    let (tx, rx) = sync_channel::<()>(1);
    WAKE.set(tx).ok();

    let worker = std::thread::spawn(move || worker(&rx));

    // SAFETY: Win32 hook/message FFI. The hook handle is released on every path out of this
    // function, and the callback is a plain `extern "system"` fn with no captured state.
    unsafe {
        let hook = SetWinEventHook(
            // One narrow range, never EVENT_MIN..EVENT_MAX. Microsoft's guidance is to "register
            // only for the events they need"; a PowerToys engineer traced a 3-5% CPU spike on
            // nothing but cursor movement to a hook registered across the whole range.
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(on_foreground_change),
            0, // every process...
            0, // ...and every thread on this desktop
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
        if hook.0.is_null() {
            anyhow::bail!(
                "SetWinEventHook failed — is this running inside an interactive session? A \
                 Session-0 service cannot hook the user's desktop."
            );
        }

        let mut msg = MSG::default();
        // `> 0` rather than `.as_bool()`: GetMessageW returns -1 on error, which is truthy as a
        // BOOL and would spin this loop forever on a bad message pump.
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        let _ = UnhookWinEvent(hook);
    }

    let _ = worker.join();
    Ok(())
}

/// The measuring loop: wake on a focus event or every [`POLL`], re-read the truth, and report every
/// [`EMIT`].
fn worker(rx: &Receiver<()>) {
    // Two trackers over the same state machine: one keyed by executable, one by browser page
    // title. They are separate rather than one map because the keys are different kinds of thing —
    // `"chrome.exe"` is a program the enforcement tally also knows about, `"Roblox"` is what a tab
    // happened to say — and because the second is unbounded where the first is not.
    let mut apps = Tracker::new();
    let mut pages = Tracker::new();
    let started = Instant::now();
    let mut last_emit = Duration::ZERO;

    loop {
        // Either a focus change woke us or the reconciliation interval expired. Both lead to the
        // same work, which is the point: the hook buys latency, not correctness.
        let _ = rx.recv_timeout(POLL);

        let elapsed = started.elapsed();
        let now_ms = elapsed.as_millis() as u64;

        // Idle first, so the focus updates below bank against the correct active/away state.
        apply_idle(&mut apps, now_ms);
        apply_idle(&mut pages, now_ms);

        apps.focus(foreground_app().as_deref(), now_ms);

        // A page is only credited while a *browser* is in front. `browser_page` returns `None` for
        // every other window, and `Tracker::focus(None, _)` charges those seconds to nobody — so
        // time in Notepad never lands in the page list.
        let page = foreground_title()
            .as_deref()
            .and_then(crate::foreground::browser_page)
            .map(|p| p.page);
        pages.focus(page.as_deref(), now_ms);

        if elapsed.saturating_sub(last_emit) >= EMIT {
            emit(&crate::foreground::Sample {
                apps: apps.drain(now_ms),
                pages: pages.drain(now_ms),
            });
            last_emit = elapsed;
        }
    }
}

/// Tell the tracker whether the user is away, **back-dating the transition to when input actually
/// stopped**.
///
/// This is what makes idle handling exact rather than approximate. Naively flipping a flag when the
/// threshold is crossed credits the whole grace period twice over — once as it elapses, and again
/// on every poll until somebody notices. Because `GetLastInputInfo` reports how long ago the last
/// input was, the moment the user stopped being present is known precisely: it is
/// `last_input + IDLE_AFTER`. Handing the tracker that timestamp credits exactly the grace period
/// and not one second more, and [`Tracker`] never moves its marker backwards, so a late detection
/// cannot retroactively take away time already earned.
fn apply_idle(tracker: &mut Tracker, now_ms: u64) {
    let idle_ms = idle_millis();
    if idle_ms >= IDLE_AFTER.as_millis() as u64 {
        let credited_until = now_ms
            .saturating_sub(idle_ms)
            .saturating_add(IDLE_AFTER.as_millis() as u64);
        tracker.set_idle(true, credited_until);
    } else {
        tracker.set_idle(false, now_ms);
    }
}

/// Milliseconds since the user last touched keyboard or mouse **in this session**.
///
/// `GetLastInputInfo` is explicitly session-scoped — it "does not provide system-wide user input
/// information across all running sessions" — which is a second, independent reason this code lives
/// in the helper rather than the service.
fn idle_millis() -> u64 {
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // SAFETY: `info` is a correctly-sized, fully-initialized LASTINPUTINFO owned by this frame.
    let ok = unsafe { GetLastInputInfo(&mut info) };
    if !ok.as_bool() {
        // Assume present. Failing toward "in use" over-counts slightly; failing the other way
        // would hand out unmetered screen time whenever the call hiccupped.
        return 0;
    }
    // `wrapping_sub` because both are 32-bit tick counts that roll over every ~49.7 days, and the
    // docs warn the value "is not guaranteed to be incremental". A plain subtraction across the
    // rollover yields ~49 days of apparent idleness and silently stops all accounting.
    // SAFETY: `GetTickCount` takes no arguments, touches no memory we own, and cannot fail.
    let now = unsafe { windows::Win32::System::SystemInformation::GetTickCount() };
    u64::from(now.wrapping_sub(info.dwTime))
}

/// The executable name of whatever currently holds focus, lowercased to match how `rules::norm`
/// keys the enforcement tally. `None` when nothing does.
fn foreground_app() -> Option<String> {
    // SAFETY: Win32 window/process FFI. The process handle is closed on every path; `hwnd` is only
    // used as an opaque token and is never dereferenced.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            // Routine, not exceptional: the lock screen, the UAC secure desktop, and the instant
            // after a window closes all report no foreground window. Those seconds belong to no
            // app, and `Tracker::focus(None, _)` is how they get charged to nobody.
            return None;
        }

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }

        // LIMITED, not PROCESS_QUERY_INFORMATION. The wider right FAILS against an elevated
        // process, and ActivityWatch's well-known 5-30% CPU burn on Windows is exactly that
        // failure falling back to a WMI query every second forever. Here it would be worse than
        // slow: a child could evade tracking entirely by running a game as administrator.
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;

        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let name = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .ok()
        .map(|()| String::from_utf16_lossy(&buf[..len as usize]));
        let _ = CloseHandle(handle);

        let path = name?;
        // Just the file name: the enforcement tally is keyed on `"roblox.exe"`, not a full path,
        // and the two must agree for the dashboard to show them side by side.
        Some(
            path.rsplit(['\\', '/'])
                .next()
                .unwrap_or(&path)
                .trim()
                .to_lowercase(),
        )
    }
}

/// The foreground window's title, for browser page attribution. Currently unused by the emitted
/// sample; kept because reading it is one call and `foreground::browser_page` already parses it.
///
/// Safe to call cross-process: `GetWindowTextW` only blocks when asked about a window owned by the
/// *calling* thread, which this never is.
fn foreground_title() -> Option<String> {
    // SAFETY: Win32 window FFI; `buf` is owned by this frame and `hwnd` is an opaque token.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let mut buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut buf);
        (len > 0).then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// Write one sample as a JSON line on stdout, which is the pipe back to the service.
///
/// A sample with nothing in it is still written. Silence is how the service distinguishes a watcher
/// that is running and seeing an idle machine from one that has died — and those two must never
/// look alike, because the second means screen time is not being measured at all.
fn emit(sample: &crate::foreground::Sample) {
    let Ok(line) = serde_json::to_string(sample) else {
        return;
    };
    let mut out = std::io::stdout().lock();
    if writeln!(out, "{line}").is_ok() {
        // Flush every line: the service reads this pipe line-by-line and a buffered sample is a
        // sample it never sees.
        let _ = out.flush();
    }
}

/// Entry point for `helper --watch`.
pub fn main() -> Result<()> {
    run().context("foreground watcher failed")
}
