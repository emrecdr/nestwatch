# Open findings

**Open work only.** Every entry here describes something that is still true of the tree right now —
found by a review pass, judged real, and deliberately left unfixed, with the reasoning recorded so it
does not have to be re-derived and so nobody re-raises what was already weighed.

## How to keep this file honest

This file is a **task list, not a history**. It answers exactly one question: *what is still wrong?*

- **When a finding is fixed, delete its entry.** Do not strike it through, do not move it to a
  "Fixed" section, do not leave it in place with a note saying it was done. `CHANGELOG.md` records
  what shipped and why, and `git log` holds the rest — this file does not need to say it twice. A
  closed entry left here is not harmless: a reader budgets attention against the length of the list,
  and every dead entry spends some of it.
- **When a finding is partly fixed, rewrite it so it describes only the part that is still true.**
  A half-stale entry is worse than a missing one, because it gets checked, found wrong, and then the
  whole file is trusted less.
- **When a finding is withdrawn, refuted, or declined, move it to
  [DECLINED-OPTIONS.md](DECLINED-OPTIONS.md).** Stopping an idea from being re-proposed is a real
  job, but it is a different job from tracking open work, and mixing the two is what made this file
  2,045 lines long.
- **Cite symbols, not line numbers.** `foreground.rs::BROWSERS` survives an edit; `foreground.rs:307`
  was wrong within a week, and nothing in CI will ever tell you. Where a line number is genuinely the
  only handle, expect to re-check it.
- **Verify before writing, and say how.** Mark the measured claims as measured, with the number and
  the date. On this codebase a confident claim has repeatedly survived review and died on contact
  with the code.

## Writing across the two repos

`nestwatch-mobile` keeps the same file — `docs/OPEN-FINDINGS.md`, same rules — and the two now cite
each other. `O72` was the first pair, closed on 2026-11-18. That makes this a channel, and a channel
needs an address rather than a sentence.

**Cite a counterpart as `repo#ID`** — `nestwatch-mobile#M6`, `nestwatch#O72` — anywhere in the entry's
prose. On an entry that crosses the boundary, open it with one line, carrying only the parts that
apply and only when they are not the default:

```
> **Cross-repo** · filed by `nestwatch-mobile` · blocked on `nestwatch-mobile#M6`
```

| | |
|---|---|
| `filed by <repo>` | omit when this repo wrote it — say it when the other side did, because prose lands under whoever commits and `git blame` will name the wrong author |
| `blocked on <repo>#<ID>` | this entry cannot start until that one is done |
| `pairs with <repo>#<ID>` | same subject, both sides have work, neither waits |

**The addresses exist so a script can follow them.** `nestwatch-mobile/tool/check_findings.sh` reads
both files and resolves every reference in both directions. It matters because of the rule at the top
of this file: an entry is **deleted** when it is fixed. So a reference that resolved yesterday and
dangles today is not sloppiness — it is *the other side shipping something*, and for a `blocked on`
entry that is precisely when the wait ends and the work starts. The checker says so rather than
reporting an error, and its third outcome is the usual one: without the sibling checkout on the
machine it compares nothing and exits 2 saying so, instead of reporting a clean run.

Scoped to entries below `## Open` on both sides, so the examples in this section are not mistaken for
citations.

**Provenance.** Findings come from several independent review passes over this codebase (2026-08),
across four angles — reuse, simplification, efficiency, altitude — plus a security analysis and a
research review of per-app and web-page tracking against primary sources. Entries are not all equally
solid, and each says which it is: some are read directly off the tree and are facts about code that
exists; others rest on a primary source plus a mechanism, and name the one on-device observation that
would confirm or kill them.

Last **full** audit against the tree: **2026-08-31**. Entries that did not survive it were removed
or rewritten rather than annotated, per the rules above. Individual entries have been re-measured
since and carry their own later dates — `O87` was re-measured on 2026-10-22 and the release state
below on 2026-10-23, and `O100` was deleted on 2026-09-09 once the markup it described had been
converted. So read the date on an entry before the date on this line: this one bounds only how long
ago every entry was last looked at *together*.

## Release state

**`v0.12.1`, published 2026-11-18.** Everything below is open against a release that is on the
download page, not against unreleased work — which is what makes the list worth keeping honest
rather than tidy.

What that release was verified by: unit and integration tests, `cargo test --all-targets --locked`
and `clippy -D warnings` on Linux and on a `windows-latest` runner, cross-compilation to
`x86_64-pc-windows-gnu`, `cargo deny` against the advisory database, and a published SBOM plus
binary attestation.

What it was **not** verified by: running on the machine it is for. Section H of
[WINDOWS-TESTING.md](WINDOWS-TESTING.md) holds **66 items across §H1–§H12**, 0.11.0's words on
app groups brought seven more in §D2b, and none of them has executed on Windows (re-measured
2026-11-18). Which features they cover and what each release added is told once, in that section's
preamble, rather than repeated here. Worth stating exactly: **no commit in this repository's history
has ever recorded a ticked item in that file**, in any section, so all 244 boxes it carries are
still open. The three gates that were green
when it shipped are the same three that were green when `install` failed on real hardware and again
when `remove_file` turned out not to be exclusive. That is not an argument for distrusting them; it
is the reason the section below exists and the reason the checklist is the only method here with a
track record.

**0.12.1 adds no Windows-only code.** Measured with `git diff v0.12.0..HEAD`: under `src/` it
changes the dashboard's style policy in `security.rs`, adds its guard in `web.rs`, and rewords
comments; the rest is `tests/golden/limits.json` gaining `renew_warn_days` and documentation.
Everything below about 0.12.0 holds for 0.12.1.

**0.12.0 adds no Windows-only code, and drives 0.11.0's more often.** Measured 2026-11-18 with `git
diff v0.11.1..HEAD`: of 3,384 lines added under `src/`, none is in a module that compiles only on
Windows or in a `#[cfg(windows)]` item of a shared file. The owner snapshot 0.11.0 added
(`session::console_user_processes`) is now taken on every enforcer tick while somebody is at the
machine, not only when a group has words: it is how the service keeps each of the child's programs'
running time for the day, which a limit or group set later starts from, and it decides what a `*` in
the blocklist or a per-app limit may count or close. §H12 names what to run first.

**0.11.1 changes no code under `src/`.** Measured 2026-10-23 with `git diff v0.11.0..HEAD`: the
release moves `ctutils` from 0.4.2 to 0.4.5 for RUSTSEC-2026-0385, and `chacha20`, a test-only
dependency, off a withdrawn version. The one path in this build that reaches `ctutils` is `argon2`'s
password check, through `phc`'s constant-time comparison of the two hashes; in 0.4.2 that
comparison already went through `cmov`'s assembly rather than the helpers the advisory names.
Everything below about 0.11.0 holds for 0.11.1 unchanged.

**0.11.0 adds Windows-only code to the running service, the first since 0.8.0's probe launch.**
Measured with `git diff v0.10.0..v0.11.0`: of 2,962 lines added under `src/`, 132 are in
modules that compile only on Windows, and none is in a `#[cfg(windows)]` block of a shared file.
`session.rs` gained `console_user_processes` (+94): one `WTSEnumerateProcessesExW` snapshot whose
owner SIDs are compared with the signed-in console user's token. Both Windows controls answer
`SystemControl::child_processes` with it (+6 each), and `watcher.rs` (+26) now sends the page in
front as a visit that names its browser, so the service knows what to close. The snapshot is taken
on every enforcer tick where a group has words, and it decides what a word may count or close: the
safety half of the feature. §D2b names what to run first.

**0.10.0 touches two lines of Windows-only code, both at install time.** Measured 2026-10-04 with
`git diff v0.9.0..HEAD`: of 3,417 lines added under `src/`, two are inside a `#[cfg(windows)]`
block — `install::deploy` gained the hook that reads and, on `--revoke-sessions`, empties the
sessions file while the service is stopped — and `session.rs`, `control/windows.rs` and
`control/service_control.rs` are untouched. The running service gained no platform code; it gained
three more sentences on the child's screen through `WTSSendMessageW`, a clock mark written beside
the tally and read back at startup, a span around every request, and the gate the integration
owns. The unverified surface grew again; §H2, §H7, §H8 and §H10 name what to run first.

**0.9.0 adds no Windows-only code, which inverts what 0.8.0 had to say and moves the risk rather
than removing it.** Measured 2026-09-11 with `git diff v0.8.0..HEAD`: of 1,057 lines added under
`src/`, **zero** are inside a `#[cfg(windows)]` block, and `session.rs`, `control/windows.rs` and
`control/service_control.rs` are untouched. What 0.9.0 adds is code that *drives* an existing
Windows path — `control::notify_child`, and through it `WTSSendMessageW`, which the countdown
warnings have used since 0.2.0. So the platform surface did not grow, and the unverified surface
did: two new things now appear on the child's screen on a call whose real return value only Windows
decides, and `notify_child` treats that return value as load-bearing — a notice the OS refused is
deliberately *not* recorded as said.

**0.8.0's entry, kept because its probe launch is still unrun.** It was
the first release since `v0.5.1` to add any: the probe runs the check program *as the child*, which
on Windows means launching it into the interactive session the way the screenshot helper already
does. Measured 2026-09-09 with `git diff v0.7.0..HEAD`: the `#[cfg(windows)]` module `session.rs`
gained 129 lines, and both Windows implementations of `SystemControl::run_probe` were new —
`control/windows.rs` (+5) and `control/service_control.rs` (+7). That code is in 0.11.1 unchanged
and still unrun (§H8).

**And neither 0.6.0's nor 0.7.0's headline features are in the checklist at all** — which is worse
than being in it unrun, because an unrun item is at least counted. Tracked as `O87`.

**Re-measured 2026-09-09, and the keyword test this entry leaned on twice now needs reading with
care.** Three of its eight terms match at last — `integration` (1), `provider` (2), `StudyGo` (1) —
but every one of those matches is inside **§H8, which covers the probe that shipped in `v0.8.0`**,
not any 0.6.0 or 0.7.0 feature. The five terms that actually track the gap are unmoved at **zero**:
`earned`, `idempot`, `revoke`, `signed-in`, `masquerad`. Nothing exercises the absolute session cap,
and the sole Routines item (§E2) still exercises the *manual* save-and-apply path that predates both
releases. So re-running the measurement as it was written would report progress where there is none.
A keyword count is a proxy for coverage, and this is the proxy coming apart from the thing it
proxies — recorded here rather than quietly re-run.

**For 0.7.0 that gap sits on the authentication surface, which is the part worth saying out loud.**
Scoped pairing, per-device revocation and the absolute cap all decide whether a person gets in, they
fail closed by design, and the way back from a mistake is an elevated console on the child's PC.
0.7.0 also carried the one breaking change this project has shipped — every existing session was
refused, so every device had to be paired again — and that upgrade path has itself never run on
Windows. Nothing here says the code is wrong; it says the gates that were green when `install` failed
on real hardware are the same three that are green now.

§D2b is the one to run first on a PC where any group has words. Its owner check is the newest
Windows-only code in the download and the only part of it that decides what a word may close, and
nothing off Windows can exercise it.

The enforcer wake comes next, and first where no group has words. Its entire value is a timing
property — an abort arriving in well under a second where it previously took up to 30 — measured
once, on macOS, where `shutdown` is a no-op. On Windows it is a real `shutdown.exe` with a real
pending timer, and whether the abort beats a 60-second countdown there is unknown.

§H8 after that. The probe's Windows path shares `CreateProcessAsUserW` with the screenshot helper,
which has itself never run here either, so a failure there would take both features down and
neither has a green run to distinguish them.

---

## Open

### O4 · A wedged enforcer is reported but never recovered

`heartbeat.rs` calls a silently dead enforcer "the worst failure this product can have", and then
only *displays* the staleness — `api::usage_today`, which feeds the dashboard's Today card, is its
only consumer. `doctor` runs as a process of its own, in which no enforcer stamps anything, so it
says it cannot see one rather than reading it. It was `rules::today_summary` until the read moved
out to the edge; the mechanism is unchanged, only where it is called from.

There is no `process::abort` anywhere in `src/`, and nothing restarts the loop.
(`std::process::exit` *does* appear — argument errors in `lib.rs`, a failing `doctor` — but those
are exits on the way out, not recovery.) The signal is pull-based:
it helps only if the parent happens to look.

**What is and isn't already covered.** A *panic* is handled: `panic = "abort"` kills the process and
the SCM restarts it. The uncovered case is a **hang** — a tick that enters
`spawn_blocking` and never returns (a wedged `shutdown.exe`, a stuck WTS call). The loop stalls,
the heartbeat goes stale, and enforcement is off indefinitely with the dashboard still serving
normally.

**Two things checked, because both would have changed the answer:**
- The restart budget is *not* exhaustible. `configure_recovery` sets three `restart/5000` actions
  with `reset= 86400`, which reads like "three restarts then give up for a day". Per
  [`SERVICE_FAILURE_ACTIONS`](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/ns-winsvc-service_failure_actionsw),
  when the failure count exceeds the array length **the last action repeats**. So the SCM restarts
  indefinitely, and a watchdog that aborts cannot strand the machine unenforced for 24 hours.
- A hang is *plausible but undemonstrated*. Every blocking call in the loops has a bounded
  implementation; none has been observed to wedge.

**Fix, revised after validating it against current practice (2026-08-19).** A supervisor reading
`worst_age_secs()` that `process::abort()`s past a generous threshold (≥5× `CHECK_INTERVAL`), so
the SCM restarts a wedged service. Keep the decision a pure, unit-tested function and the abort at
the edge, matching how the enforcers are already split.

One correction to the original sketch: it said "a supervisor **task**", i.e. a `tokio::spawn`.
The established pattern for this — a heartbeat recorded on the runtime and *checked from a plain
`std::thread` outside it* — exists precisely because a supervisor living on the runtime cannot
detect the failure mode where the runtime itself is starved. That is not hypothetical here: this
service builds a multi-threaded runtime (`Runtime::new()` in `lib.rs`) and the tick loop has
five await points, two of them `spawn_blocking`. If blocking work ever saturated the pool, an
on-runtime supervisor would be queued behind exactly the stall it was meant to report.

The codebase already has this shape and can be followed rather than invented: `session.rs`
spawns a `std::thread` watchdog that kills the screenshot helper if it outruns its timeout. The
enforcer supervisor is the same idea applied to the tick loops.

Practitioner reports on this pattern note no false positives in production, and that an extra
thread waking once a second is negligible — but the caution below is unchanged, and is about this
machine rather than the pattern.

**Why it isn't done.** The abort→SCM-restart leg cannot be exercised by `cargo test`, by clippy, or
by the Windows cross-check — only on the target machine. Shipping a self-abort mechanism into a
service whose runtime behaviour hasn't been observed since an early build risks converting a hypothetical
hang into a real restart loop. **Trigger:** do this on-device, with `WINDOWS-TESTING.md` in hand.

### O5 · An unanswered question about enforcement coverage — tracked privately

There is an open question about whether enforcement holds under conditions this repository
deliberately does not enumerate. It is the highest-value unanswered item about this system.

The detail is deliberately **not** in this repository. It describes how enforcement can be
avoided rather than how it works, and — unlike every other finding here — it cannot be
re-derived by reading `src/`, because it is a property of the operating system's behaviour
rather than of this code. That asymmetry is the whole reason for the split: redacting what
the source already shows would buy nothing, so nothing else here is redacted. The repository
is public, and published, this one would be a how-to reachable from a name visible on the
managed PC. It lives in
`docs/private/OPERATIONAL-FINDINGS.md`, which is git-ignored, alongside the fix and the
condition for applying it.

### O16 · UWP windows resolve to `ApplicationFrameHost.exe`, not to the app

The watcher identifies an app by taking `GetForegroundWindow`, asking it for a pid, and reading that
process's image name (`watcher.rs::process_name`). For a **packaged UWP app** that chain returns
`applicationframehost.exe`. The OS hosts UWP windows in a frame process; the app's own
`Windows.UI.Core.CoreWindow` is a *child* window owned by a different process.

Why this one matters more than it looks: [WINDOWS-TESTING.md](WINDOWS-TESTING.md) §237 asks the
tester to confirm Roblox is attributed under **both** the direct download (`RobloxPlayerBeta.exe`)
and the Microsoft Store build (`Windows10Universal.exe`), calling switching between them "the obvious
dodge". [FOREGROUND-TRACKING.md](FOREGROUND-TRACKING.md) repeats the claim, and `assets/app.js`'s `appLabel`
maps `windows10universal.exe` to the label "Roblox". If the Store build is a CoreWindow UWP then
**that key can never arrive** — the watcher reports `applicationframehost.exe` — the label mapping is
dead code, and the dodge the checklist names is the dodge that works. Every Store app would also pile
into one meaningless row.

Two sessions checked this independently. `applicationframehost|uwp|winui|store app` returns **zero
hits** across `src/`, `docs/` and `assets/`: not handled, and not recorded as a known gap either.
ActivityWatch has carried the same defect for years, so it is not an exotic case.

**Deliberately not asserted:** whether Roblox's current Store build is a CoreWindow UWP or an
MSIX-packaged Win32 app. Only the first is affected. That is one glance at Task Manager on the target
PC, and it decides whether this entry is urgent or moot — which is why the checklist step comes first.

**Fix.** When the resolved name is `applicationframehost.exe`, `EnumChildWindows` the frame window
and take the first child whose pid differs. Known limit: a *minimised* UWP app's frame holds no such
child — irrelevant here, because a minimised window is never the foreground window.

**Trigger.** §D2, before any further doc claims the Store build is covered.

### O17 · Gamepad play reads as idle, so console-style sessions stop accruing

Idle is decided solely by `GetLastInputInfo` (`watcher.rs`, via `foreground::idle_state`), and `Tracker::bank` credits
**zero** while idle (`foreground.rs::Tracker::bank`). `GetLastInputInfo` reports keyboard and mouse only. A
Valve engineer states that Steam Input and gamepad-emulating overlays "don't generate events that
`GetLastInputInfo` would read".

So a child playing with an Xbox controller goes idle after `IDLE_AFTER` (180s) and accrues **nothing**
for the rest of the session. The failure direction is the one that matters: a silent under-count that
favours the child, on exactly the long uninterrupted session a budget exists to bound.
[FOREGROUND-TRACKING.md](FOREGROUND-TRACKING.md) rejected a pure event-driven design in as many words
for producing "a silent under-count that always favours the child" — that reasoning was never carried
across to idle.

It also surfaces as a visible contradiction, because enforcement counts *running* time and is
unaffected: a controller session renders as "Apps running 3 h / Time in front 20 min", with the
smaller and more wrong number being the one labelled as what the child actually did.

**Fix.** Register for Raw Input (`RegisterRawInputDevices`, HID usage page `0x01`, usages `0x04`/`0x05`)
with `RIDEV_INPUTSINK`, and feed `WM_INPUT` arrivals into the same last-input timestamp. Event-driven,
so it costs nothing when nobody is playing, and the watcher **already runs a message pump**. It needs
one message-only window, because `RIDEV_INPUTSINK` requires an `hwndTarget` and the pump currently has
no window.

**Not `XInputGetState` polling.** Microsoft's own guidance is not to call it for empty user slots every
frame, and 10–15% CPU losses have been measured doing exactly that. The cheap-looking fix is the
expensive one.

**Trigger.** Alongside O18 — same file, same measurement, and §D2 can then test both in one sitting
with a controller on the desk.

### O18 · Idle time is discarded rather than reported, so passive viewing vanishes

Same mechanism as O17, different victim. Because `bank` credits zero while idle, **any** use that
generates no keyboard or mouse input disappears after 180 seconds: a 40-minute YouTube video counts as
about three minutes. For a product whose report answers "what has he been doing all evening", video is
not an edge case.

The deeper problem is not the threshold, it is that the seconds are **thrown away**. This codebase
draws a careful line everywhere else between "measured zero" and "not measured" — `measured` on
`DayRow`, `focus_missing` on the today card, `null` rather than `0` for an absent helper — and states
the reason: collapsing them "would let a dead enforcer render exactly like a well-behaved child".
Idle time is the one place that distinction is silently dropped.

**Preferred fix, and it needs no new Win32 at all:** bank idle seconds into a third bucket and report
them, so the card reads "2 h active · 40 min unattended (app open, no input)". That converts a silent
under-count into an honest visible number, costs one map, and is consistent with the pattern the rest
of the codebase already follows. A parent can then interpret it; today they cannot, because they
cannot see it.

**Considered and not recommended:** detecting playback via `IAudioSessionManager2` +
`IAudioMeterInformation` peak values, checked only once input-idle has tripped (so zero cost on the
active path). It misattributes in the case that matters most — Chrome's audio session belongs to a
separate utility process, not the process owning the foreground window — and it misses muted video.
Worth revisiting only if the third bucket proves insufficient in practice.

**The general form, which is worth more than this entry.** On this codebase, **"no input" and
"nothing happening" are different facts**, and treating them as one is a recurring mistake rather
than a one-off. This entry and O17 are two instances; a third was caught in the capture path, where
skipping a screenshot while `GetLastInputInfo` reports idle looked like free savings and would have
frozen the live view during exactly the activity a parent most wants to see — a child watching a
video generates no input for an hour while the screen changes continuously.

The enforcement path already knows this and is the model to copy: `session_state()` distinguishes
`Locked` from `NoUser` rather than collapsing both into "away". Any future optimisation keyed on idle
should be read against that.

**Trigger.** With O17.

### O19 · An unrecognised browser silently yields no page data

`BROWSERS` (`foreground.rs`) lists four executables: `chrome.exe`, `msedge.exe`, `firefox.exe`,
`brave.exe`. `is_browser` gates the title read, so anything else — Opera, **Opera GX**, Vivaldi, Arc,
any Chromium fork — produces no page attribution at all.

The list being short is deliberate and the reasoning is sound (an entry that never matches is
indistinguishable from a child who never opened it). The finding is the **failure mode**, not the
length: the dashboard shows `opera.exe` under "Apps running" and nothing under "In the browser", which
is pixel-identical to an evening of not browsing. So it is an evasion route that requires no
privilege, no scripting and no admin — install Opera GX, which is marketed squarely at exactly this
product's demographic, and web visibility silently drops to zero.

**Fix.** Not "add more browsers" — that loses the same way one version later. Either (a) detect that
the foreground app is a browser the watcher cannot parse and surface it as *unrecognised browser —
page titles unavailable*, so absence is visible, or (b) fall back to the raw window title with the
browser suffix left on when the exe is unknown-but-browser-shaped. (a) is honest and cheap; (b) is
more useful and needs a rule for what counts as browser-shaped.

**Researched 2026-11-18, and it now bears on the `*` patterns.** A `*roblox*` limit or blocklist
entry reaches pages only in these four browsers, so Roblox in Opera GX, which is made for gamers, is
outside it. Vivaldi runs as `vivaldi.exe` and its titles reportedly end ` - Vivaldi` (Vivaldi forum
posts, not documentation); Opera and Opera GX run as `opera.exe` with no confirmed title format,
since Opera has no visible title bar; nothing was found for Arc. Both would be single entries in
`BROWSERS`, but the table's own rule is to leave out a format not confirmed, and only the PC can
confirm one. The child in the report that prompted the patterns uses Chrome.

**Trigger.** Next change to page attribution; also worth one line in §D2 to confirm the empty-state is
distinguishable.

### O42 · Helper lifetime is reconstructed at teardown instead of established at spawn

`session.rs`'s `spawn_piped` calls `CreateProcessAsUserW` with `CREATE_UNICODE_ENVIRONMENT |
CREATE_NO_WINDOW` and **no job object**, so the service creates a child and immediately forgets it is
the parent. The orphaned-helper fix then re-derives "which processes are mine" from the process table at install
and uninstall time.

That fix is correct and stays. This entry is about its *depth*. The cost of answering the question
late rather than never losing the answer:

- ~190 lines of Win32 in `install.rs` — `process_image_path`, `processes_named`, `terminate_and_wait`,
  `terminate_resident_helpers` — of which only the selection predicate and the name-buffer parse can
  run off Windows.
- A new crate surface (`Win32_System_Diagnostics_ToolHelp`).
- **A security question that exists only because the answer was discarded:** "which same-named
  process may an elevated installer terminate?" It costs a paragraph of prose and two dedicated
  tests. Under a job object, membership *is* the identity and is unforgeable, so the question does
  not arise.
- Residual over-breadth: selection is on the image path, so it also matches a short-lived screenshot
  helper or an admin's own `doctor` run from the install directory. Harmless today, and the shape of
  a fix that must infer what it should have been told.

**The deeper fix:** one unnamed job object for the service process's lifetime with
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, and every `spawn_piped` child assigned into it
(`CREATE_SUSPENDED` → `AssignProcessToJobObject` → `ResumeThread`). Two things were checked because
they are the usual objections: job objects are **not** session-scoped (session affinity applies to
the *named* object namespace, and this handle would be unnamed and passed directly), and the handle
table is torn down on **any** process exit — crash, `panic = "abort"`, SCM force-kill — which is
exactly the set of cases a cooperative shutdown signal cannot cover.

**Honest costs, which are why this is filed rather than done:**
- More untestable FFI in `session.rs`, and unlike `helpers_to_terminate` there is no pure predicate
  to extract from it.
- `CREATE_SUSPENDED` widens the error paths: a failed assign must terminate the suspended child, and
  getting *that* wrong leaves a permanently-suspended orphan — worse than the leak being fixed.
- `spawn_piped` passes `bInheritHandles = TRUE`. The job handle **must** be non-inheritable or the
  child inherits a handle to its own job and `KILL_ON_JOB_CLOSE` never fires. `CreateJobObjectW(None,
  None)` is non-inheritable by default, so it is right for free — which is precisely the
  silent-failure shape this project keeps meeting.
- It does not delete the installer-side code, only shrinks it: job termination is *initiated*, so
  `deploy` still needs to confirm the image is free before `fs::copy`. `terminate_resident_helpers`
  becomes a wait.

**Trigger.** Not before §D2. Adding a large piece of unverified Win32 to the spawn path of the
service that locks a child's PC, on top of a release nobody has watched run, is the exact stacking
O2 and O4 both decline — when something misbehaves on the device there would be no way to tell the
feature from the refactor.

**Related and deliberately not treated as the fix:** `run_watcher_supervisor` is a detached
`std::thread::spawn` (`session.rs`) with no shutdown signal, which is a genuine root cause but a
*shallower* one — it covers `sc stop` and OS shutdown only, not a crash, not `panic = "abort"`, not
the `nestwatch run` CLI path. Worth doing for hygiene; not a substitute. If done, the seam is the
`axum_server::Handle` already held in `server.rs`, not the `mpsc` in `service.rs`, which is
single-receiver and already consumed.

### O87 · Six features are not in the Windows checklist at all

The release state above says the checklist is "the only method here with a track record", and every
other entry that defers to hardware defers to it. Scheduled routines and the integrations registry
are not in it.

**Measured 2026-09-02** by counting matches in `WINDOWS-TESTING.md`: `integration` **0**, `provider`
**0**, `StudyGo` **0**, `earned` **0**, `idempot` **0**. The word `routine` appears three times and
`schedule` twice, and none of those is the new behaviour — the only Routines item (§E2) is *"Save
current as routine… then Apply → the settings revert"*, which is the manual press-a-preset path that
predates the release. Section H, where new work goes, is titled **"New in 0.5.0"** and scoped to it.

**Re-measured 2026-09-04, and the gap grew rather than closed.** All five counts above are still
**0**, and four features merged since are absent on the same test: `revoke` **0**, `signed-in`
**0**, `masquerad` **0**, and no item anywhere exercises the absolute session cap. (`scope` matches
twice and `session` eighteen times; every one is unrelated — a firewall rule "scoped to
`private,domain`", a Session 0 reference. Checked, not assumed.)

**Re-measured 2026-09-09 at the 0.8.0 release, and the count moved for a reason that is not
progress.** Three terms now match — `integration` **1**, `provider` **2**, `StudyGo` **1** — and
every match is inside the new **§H8**, which covers 0.8.0's provider probe. None of the six features
this entry is about gained an item. The five terms that track them are unchanged at **0**: `earned`,
`idempot`, `revoke`, `signed-in`, `masquerad`. The lesson is about the measurement rather than the
gap: a keyword count is a proxy for coverage, and a proxy that another feature can satisfy will
report a closing gap that has not moved. Read the five, not the eight.

**0.8.0 did the thing 0.6.0 and 0.7.0 did not**, which is worth recording because it is the fix
working: its headline feature arrived with §H8 attached, written to section H's standard and titled
*"never run on Windows"*. That does not close this entry — the six remain absent — but it means the
gap stopped growing at this release for the first time since it was filed.

**0.9.0 did it once and failed to do it once, which is the more informative result.** Its message
box arrived with §H9 in the same commit. The gate's two sentences to the child — `O101`'s fix,
shipped the same day it was filed — arrived with nothing, and stayed uncounted for two days across
a cleanup pass, a docs-freshness pass and a release preparation, all three of which read that file.
Three items were added to §H8 while cutting `v0.9.0`, before the tag rather than after it, which is
the first time this gap has been closed on the same day it opened. The lesson is not that anyone
was careless: **a feature shipped in a hurry is exactly the one whose checklist item is skipped**,
and `O101` was closed the morning it was filed. A guard that counts items cannot catch this — the
count was correct the whole time, it just counted a smaller feature set than had shipped.

**They shipped in `0.7.0` on 2026-09-04, so this is no longer a warning about future work.** The
0.6.0 half of this entry became a gap by being forgotten at release; the 0.7.0 half was written
down while it was still unreleased and shipped anyway, which is the more useful failure to record —
knowing did not close it. All six now sit on the download page unexercised on the hardware they run
on, and three of them decide whether a parent can sign in. Scoped pairing, per-device revocation and the absolute cap are all the
*authentication* surface, where a checklist gap is worth most and where this release's one
breaking change — every existing session refused — actually lands.

**Why this is worse than an unrun item, not the same as one.** An unrun item is counted: section H
carries 66 items that have never executed (measured 2026-11-18; 61 at `v0.11.0`; 32 when this was
written, and the checklist's section-H preamble has the steps between), so the gap has a size and a
reader can weigh it. A feature absent from the checklist has no size. The release-state paragraph
could be read as "0.6.0 is unverified in the same way 0.5.0 was", and it is not — 0.5.0's features
were written down and left unchecked, 0.6.0's were never written down. The two together are the
tier-3 surface, and only one of them is visible.

**Not a documentation problem.** The features carry real Windows-side behaviour that nothing here
has exercised: a schedule that opens and closes against `clock`'s tamper-anchored local time and
must hand back on close; the `once per source per day` latch surviving a service restart; a provider
toggle that must stop a grant without unpairing. Each is the kind of wiring where this project's
real defects have lived.

**Fix.** A section I for 0.6.0, written to section H's standard — each item saying *what it proves*,
because H's own preamble warns that several checks pass trivially when the underlying call is
failing. Worth writing with someone who can run them: an item authored from the code alone risks
being the tautological fixture `tests/spawn_paths.rs` exists to prevent.

**Trigger.** Already fired — the release is on the download page. Until then, do not read "tier 3
unrun" as covering 0.6.0; it covers 0.5.0 and is silent about 0.6.0.

---

## Not covered by any of this

**None of the above has run on the target machine.** Everything here was found by reading, tests,
and cross-compilation — the same three gates that were green when `install` failed on real
hardware, and again when `remove_file` turned out not to be exclusive. See
[WINDOWS-TESTING.md](WINDOWS-TESTING.md); it is the only method with a track record of finding what
matters here.

### O64 · The helper pipe is a 4 KiB buffer, and full frames now cross it every tick

`spawn_piped` passes `nSize = 0` to `CreatePipe` (`src/session.rs`), which asks Windows for the
system default — 4 KiB. That was unremarkable while a full-tier frame crossed the pipe only when a
parent clicked. `liveTier()` now sends one every `_refreshMs` for as long as the full-size view is
open, so a roughly megabyte payload is a per-tick event.

At that size the frame is several hundred `WriteFile`/`ReadFile` pairs per tick, against about
seven for a preview, with the helper blocking each time the buffer fills. `read_to_end` compounds
it: the destination `Vec` starts empty, so it reallocates its way up and memcpys roughly twice the
payload, every tick.

**Estimated, not measured** — the 4 KiB figure is `CreatePipe`'s documented default for `nSize = 0`
and the payload size is inferred from this repo's own 4K measurements. Nobody has run it.

**Fix.** Give `CreatePipe` a real `nSize` (256 KiB) and hand `read_to_end` a `Vec::with_capacity`.
Two lines, no behaviour change. **Measure first** — this is on the child's machine, which is where
this project's guesses have been wrong before.

### O104 · Everything this service says to the child evaporates after thirty seconds

Every message the child receives goes through `control::notify`, which on Windows is
`WTSSendMessageW` with `NOTIFY_TIMEOUT_SECS = 30`. The box appears over whatever they are doing and
dismisses itself. That is right for a countdown — *five minutes left* is worthless once it is
false — and wrong for the two newest callers.

**The practice reminder is rationed to one a day** (`probe.rs`), so if he is away from the desk
when it fires, that is the day's notice spent on an empty chair. **A message a parent typed** is
worse: they were told it was *shown on his screen*, which is true and useless if he was in the
kitchen. Both are notes rather than alarms, and a note that cannot be read later is a poor note.

**The remedy is known and is a project rather than a parameter.** A toast notification lands in the
Action Center and stays there until dismissed, which is exactly the property both callers want.
Microsoft's documentation is explicit that a Session 0 service cannot raise one: toasts require a
process running in the user's session, registered as a notification source with an AppUserModelID
and a Start Menu shortcut. This codebase already *has* a user-session process — the helper that
captures the screen and watches the foreground — so the missing pieces are the registration, a
fourth helper subcommand, and a decision about what happens on a machine where the registration
failed.

**Raising the timeout is not the fix and should not be mistaken for one.** A modal box that sits
there for ten minutes is worse than one that clears: it blocks the desktop it is covering, and the
child's only move is to dismiss it unread. The choice is *persist somewhere else*, not *persist
harder here*.

**What makes this worth a finding rather than a wish:** the two newest callers were designed around
a channel whose one property they do not want, and the alternative is documented, supported, and
already half-present in this repository. Left open because the registration touches `install`, and
an install-time step that can fail silently is exactly the class this project spends the most
effort avoiding.
