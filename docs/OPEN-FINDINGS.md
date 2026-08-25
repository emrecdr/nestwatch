# Open findings

Known design problems that are **not** bugs and **not** scheduled. Each one was found by a review
pass, judged real, and deliberately left alone — with the reasoning, so it doesn't have to be
re-derived, and so nobody re-raises something already weighed.

`CHANGELOG.md` records what shipped. This records what didn't, and why: what is still **open**,
what proved worth **fixing** after all, and what was **considered and declined**.

**Provenance.** Several independent review passes over this codebase (2026-08, twelve reviewers in
total), plus a security analysis and a verification round — across four angles: reuse, simplification, efficiency,
altitude. Every claim below was verified against the tree before being written down; the measured
ones say so.

A later pass (2026-08-25) added O11-O15 and six more *Considered and declined* rows, including five
suspicions about the install and enforcement paths that were checked and found false. That pass also
recorded one of its own recommendations as wrong — see O10's entry below, where a proposed fix would
have been a no-op because it misread the flag it proposed to reuse.

That second pass is also why several entries in *Considered and declined* record findings that were
**refuted**: on this codebase a confident claim has repeatedly survived review and died on contact
with the code, so what was checked and found false is worth as much shelf space as what was found
true.

A third pass (2026-08-25) added **O16-O24**, from a research review of per-app and web-page tracking
against primary sources — Win32 documentation, Chromium's `ax_mode.h`, and Microsoft's own XInput
guidance. Each entry states how it was established, because they are not equally solid: O18, O19, O21
and O22 are read directly off the tree and are facts about code that exists; O16, O17 and O20 rest on
primary sources plus a mechanism, and each names the one on-device observation that would confirm or
kill it. **None of them has been seen happen**, which is the same tier the watcher itself sits in and
the reason O23 exists at all. One finding from this pass was dropped before it reached the file: a
suspected doc-to-code drift on Roblox's Store-build naming turned out to be implemented after all, in
`appLabel` — the drift is O16's, and it is the opposite of what it first looked like.

---

## Open

### O2 · `rules.rs` has a real seam between the pure machine and the loop

The file is ~1,950 lines holding config types, the tally and its persistence, `Targets`,
`today_summary`, the pure enforcer, the async loop, shutdown-abort coordination, notification
helpers, and the tests.

Size is not the finding. The finding is that **the loop could not be moved out**, because it read
the enforcer's private `budget_deadline`, and same-file privacy was what made that legal. Removing
that field-peek (`RuleAction::LockWarning`, shipped) unblocked the cut — so the seam is now
actually available, which it wasn't before.

**Fix.** Cut between the pure machine (`Tick` / `RuleAction` / `RulesEnforcer`) and the loop
(`run_rules_enforcer` + abort coordination + message helpers). A worthwhile follow-on is lifting the
tally (`Usage` / `Targets` / `today_summary`) out too — it has its own consumer in `api.rs` that
never touches the enforcer, and it would end the `rules::Usage` / `usage::UsageLog` name collision.

**Do not** split the config types out; `curfew.rs` holds its own the same way, and breaking that
symmetry buys nothing.

**Trigger.** Next time this file is opened for a feature rather than a fix.

**The trigger has fired, and the work is still deferred — deliberately.** The screen-time report
(the screen-time report) opened `rules.rs` for a feature: it added the rollup write and `decide_after_snapshot`.
That is exactly the moment this entry named. It is being held anyway, for the same reason O4 gives
for holding its own fix: this codebase has never been run on the target machine. Cutting the enforcer's pure machine away from its loop is a large change to the file that
decides when a child's PC locks — and stacking it on top of a release nobody has watched run means
that when something misbehaves on the device, there is no way to tell the feature from the
refactor.

**Revised trigger:** the *next* feature to touch `rules.rs` **after** the current build has been verified
on-device against `docs/WINDOWS-TESTING.md`. If that verification finds nothing, do this first,
before the feature.

### O4 · A wedged enforcer is reported but never recovered

`heartbeat.rs` calls a silently dead enforcer "the worst failure this product can have", and then
only *displays* the staleness — `api::usage_today` (which feeds the dashboard's Today card) and
`doctor` are its only two consumers. It was `rules::today_summary` until O3 moved the read out to
the edge; the mechanism is unchanged, only where it is called from. There is no `abort`, `exit`, or restart anywhere in `src/`. The signal is pull-based:
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
service builds a multi-threaded runtime (`Runtime::new()`, `lib.rs:185`) and the tick loop has
five await points, two of them `spawn_blocking`. If blocking work ever saturated the pool, an
on-runtime supervisor would be queued behind exactly the stall it was meant to report.

The codebase already has this shape and can be followed rather than invented: `session.rs:314`
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


### O6 · Screen-time figures are machine-wide and count running, not focused, time

The report added in the screen-time work counts any account at the console, and counts an app while
its process runs rather than while it has focus. Both are conservative for enforcement and
misleading for a report; both are labelled on the card rather than silently accepted.

**Per-account attribution is cheaper than foreground tracking, but it is not "free" — corrected
2026-08-19.** The username genuinely is already fetched and discarded: `session.rs:144` reads
`level1.UserName[0]` purely to detect the sign-in screen, from a `WTSINFOEXW` buffer it has
already validated. No new FFI call, no new `unsafe`, no extra syscall — that part holds.

What the earlier estimate missed is the blast radius above that line. `session_state()` is a
method on the `SystemControl` trait, so carrying a username out means changing the trait, the
`SessionState` enum, and all three implementations (`windows.rs`, `service_control.rs`, `fake.rs`)
plus `rules.rs` — six files, and the enum is matched on in the enforcement path. It is a small
change in the FFI and a trait-wide change everywhere else, which is a different proposition from
"one string per day".

That does not make it wrong, and the value is unchanged: without it the report cannot separate a
parent doing their taxes from a child's evening. But it is not the free win the first pass called
it, and it lands in the tier only on-device testing can verify at runtime — the same reason O4 is
held. Do it alongside the next change that already opens `SystemControl`, not on its own.


**Foreground accuracy is not cheap.** Microsoft disabled Interactive Service Detection in Windows 10
build 1803, so a session-0 service cannot reach user-session windows at all; it would need a helper
resident in the child's session, well beyond the existing on-demand screenshot helper.

**The foreground half is now designed and half-built — see
[FOREGROUND-TRACKING.md](FOREGROUND-TRACKING.md).** That estimate above was right, and checking it
against primary sources made it sharper: `SetWinEventHook` is scoped to a **desktop**, not a
session, so no arrangement of a session-0 service can observe the child's windows. A resident
helper is forced, not chosen.

Shipped: the whole data path, the watcher included — `Usage::foreground_secs`, a `focused` map in
the rollup row, `DayRow.focused` out through `GET /api/screentime`, the pure aggregation that bounds
what the watcher reports, and `src/watcher.rs` itself.

**What is still open is verification, not authorship.** The watcher is written, compiles and lints
against `x86_64-pc-windows-gnu`, and **has never executed — not here, not anywhere**. It waits on
§D2 of [WINDOWS-TESTING.md](WINDOWS-TESTING.md) for the reason O4 gives. The distinction is worth
the sentence: this entry previously read "still open: the watcher itself", which anyone deciding
what to pick up next would reasonably take as "not written yet".

Three things learned while designing it are worth not re-deriving:

- **A hook alone under-counts.** Every shipping *tracker* (ActivityWatch, Cobalt, screenpipe) polls
  or hybridises; the hook-only design is what tiling window managers do. Hooks miss transitions and
  `GetForegroundWindow` returns `NULL` during UAC and the lock screen — for a window manager that
  is a cosmetic glitch, for screen-time accounting it is a silent under-count that always favours
  the child.
- **`WTSGetActiveConsoleSessionId` is the wrong primitive for this** (it is the right one for the
  screenshot helper). It returns exactly one session, so a second child logged in under fast user
  switching would have no screen time at all.
- **Focused time must not enforce.** The watcher runs *as the child*, so its figures are
  attacker-chosen. They are report-only, and `foreground_time_cannot_trigger_a_per_app_limit` keeps
  them that way.

**The per-account half of this entry is unchanged and still open.**

### O16 · UWP windows resolve to `ApplicationFrameHost.exe`, not to the app

The watcher identifies an app by taking `GetForegroundWindow`, asking it for a pid, and reading that
process's image name (`watcher.rs:297-334`). For a **packaged UWP app** that chain returns
`applicationframehost.exe`. The OS hosts UWP windows in a frame process; the app's own
`Windows.UI.Core.CoreWindow` is a *child* window owned by a different process.

Why this one matters more than it looks: [WINDOWS-TESTING.md](WINDOWS-TESTING.md) §237 asks the
tester to confirm Roblox is attributed under **both** the direct download (`RobloxPlayerBeta.exe`)
and the Microsoft Store build (`Windows10Universal.exe`), calling switching between them "the obvious
dodge". [FOREGROUND-TRACKING.md](FOREGROUND-TRACKING.md) repeats the claim, and `assets/app.js:1056`
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

Idle is decided solely by `GetLastInputInfo` (`watcher.rs:234-252`), and `Tracker::bank` credits
**zero** while idle (`foreground.rs:417`). `GetLastInputInfo` reports keyboard and mouse only. A
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

`BROWSERS` (`foreground.rs:307`) lists four executables: `chrome.exe`, `msedge.exe`, `firefox.exe`,
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

**Trigger.** Next change to page attribution; also worth one line in §D2 to confirm the empty-state is
distinguishable.

### O20 · Domain capture is cheaper than FOREGROUND-TRACKING.md assumed — re-open the decision

[FOREGROUND-TRACKING.md](FOREGROUND-TRACKING.md) declines domain tracking, and the DNS half of that
reasoning **validates**: Chrome's built-in async resolver bypasses the Windows DNS client, so
`Microsoft-Windows-DNS-Client` ETW sees nothing without writing browser policy into `HKLM`. That
decision should stand and does not need re-arguing.

What the document never evaluates is reading the **omnibox via UI Automation**, and the assumption
that it would be too expensive does not survive checking the source. Chromium's `ax_mode.h` defines
`kNativeAPIs` as indicating "a third-party client accessing Chrome via accessibility APIs" and states
that without additional modes "the contents of web pages will not be accessible". The expensive
renderer tree is `kWebContents` and above. **The address bar is browser-native UI, not web content.**
Since Chrome 138, Chromium enables native UIA by default, removing the old MSAA emulation layer.

**What is confirmed is the flag semantics, not the behaviour.** Nobody has observed which AXMode Chrome
actually enters when a client queries only the omnibox. On this codebase's record that gap is the whole
finding, not a footnote — the claim is "worth one measurement", not "safe to build".

Costs that are real either way: COM plus `UIAutomationCore.dll` in a process that currently loads
neither (RAM); fragility, since the target is `"Address and search bar"` by name or `addressEditBox` by
AutomationId and vendors who do this commercially warn that browser versions and Windows locales break
it; and a genuine **privacy escalation** — capturing only the focused tab's eTLD+1 and discarding path
and query keeps most of the posture the "no browser history reading" decision protects, but it is a
parent-facing decision, not an installer detail.

**Fix.** Prototype, measure Chrome's CPU/RSS with and without the query attached, and only then decide.
If Chrome escalates past `kNativeAPIs`, drop it and keep page titles.

**Trigger.** Only after §D2 — measuring a second thing on a subsystem that has never run once is the
wrong order.

### O22 · Page attribution is sampled at 5 s, so short tab visits round away

`POLL` is 5 seconds (`watcher.rs:62`) and the page title is re-read on each resolve. Focus *time* is
computed from timestamp deltas so totals do not drift — but the **title** attached to those seconds is
whatever the last sample saw, so a tab visited for less than a poll interval may be credited to the
neighbouring page or missed entirely.

For app-level accounting this is fine and by design. For page titles it is weaker than it looks,
because tab switching is far more frequent than app switching — which is exactly why page titles are
the higher-cardinality dimension the caps exist for.

`FOREGROUND-TRACKING.md`'s own *Unverified* section already reaches the right idea and stops short of
committing: re-register a **PID-scoped `EVENT_OBJECT_NAMECHANGE` hook on each foreground change**, so
title edges are caught precisely while a background browser autoplaying video generates no events at
all. It notes this "combines two proven patterns but matches no existing tracker", which is the honest
reason it is a prototype rather than a change.

**Fix.** Prototype the scoped NAMECHANGE hook and measure event volume before adopting. Unscoped
NAMECHANGE is a firehose — it fires per control — and komorebi's source says plainly "this spams the
message queue, but I don't know what else to do."

**Trigger.** Only if §D2 shows page figures that look wrong. Do not do this speculatively.

### O23 · "Minimum resources" is the stated design target and no number has ever been taken

[FOREGROUND-TRACKING.md](FOREGROUND-TRACKING.md) is admirably direct that **no figure in its resource
table is measured**, and that the numbers usually quoted (komorebi under 1% CPU, `aw-server-rust` at
9 MB idle) describe somebody else's program. That honesty is right, and it leaves the project's stated
constraint — maximum tracking, minimum CPU and RAM — currently unverifiable.

This is recorded as a task rather than a caveat because it gates several entries above. O20 cannot be
decided without a baseline to compare against; O17's fix is chosen partly on cost grounds; and every
optimisation proposed for this subsystem is otherwise an argument between two guesses.

**Fix.** Take four numbers on the target PC over one normal evening: watcher CPU %, watcher RSS,
`ApplicationFrameHost`/browser CPU delta with any UIA query attached, and the count of resolve
wake-ups per hour. Publish them in the resource table with the date and the machine, replacing the
prose that currently stands in for them.

**How to take them, learned the expensive way on the capture path.** A peer session benchmarking PNG
encoding nearly published two confounded numbers, and the failure mode transfers directly:

- **Vary one thing at a time, and print the config you used.** An RGB-vs-RGBA comparison changed the
  compression level *and* the channel count together, and credited the whole 62% to the alpha
  channel. Isolated properly, alpha was 10% and compression was the rest.
- **A suspiciously round or suspiciously identical number is the tell, not a result.** One
  configuration produced byte-identical output for four different images — 960×540×3 exactly. It was
  emitting stored DEFLATE blocks. Not a measurement, an artefact.
- **Check what a library's "default" actually is.** `image`'s `CompressionType` *defaults* to `Fast`,
  while the variant *named* `Default` is a different and slower setting — so "compare against the
  default" and "compare against `Default`" are opposite instructions. That API shape will catch the
  next person too.

**Trigger.** §D2 step 12, which already asks for this. It has simply never been done.

### O24 · Game portals are identifiable from page titles at zero syscall cost

The product goal named in [FOREGROUND-TRACKING.md](FOREGROUND-TRACKING.md) is separating "an evening of
Roblox from an evening of homework". Native Roblox is already measured exactly by process name. Browser
portals — now.gg, Poki, CrazyGames, coolmathgames — are not, and today they land as undifferentiated
page titles in a list capped at `MAX_PAGES`.

But those titles are highly regular ("Poki — Free Online Games"), and the watcher **already has the
title**. Classifying it costs no additional Win32 call, no COM, no browser reconfiguration and no
privacy escalation. This is the cheapest available answer to the question the feature exists to answer,
and it is independent of O20 — worth doing whether or not domain capture ever happens.

**Honest limits, which belong on the dashboard and not just here:** a renamed tab defeats it, and so
does any portal not on the list. It is a *label* on data already collected, never a claim of coverage —
so it must not be presented as "no game sites visited". Absence of a match means nothing was
recognised, which is the same null-vs-zero rule as everywhere else.

**Fix.** A small static title→category table, applied at render time in `screentime.rs`/the dashboard
rather than at collection, so the stored data stays raw and the list can change without a re-collection
or a migration.

**Trigger.** Any time; it touches no Win32 and cannot regress measurement. Lowest-risk entry here.

### O41 · The screen-capture path has a Windows floor the README does not admit to

Any move from GDI `BitBlt` to Windows.Graphics.Capture needs `GraphicsCaptureItem::CreateForMonitor`,
which is **Windows 10 1903 (build 18362)**. The README promises "Windows 10 or 11" with no floor, so
an unconditional switch would silently stop working on 1809 — a build that is still out there on
hand-me-down family PCs, which is most of this product's market.

**The trap is the number.** This codebase already cites **1803** twice, correctly, for the removal of
Interactive Service Detection. 1803 is therefore the version in everyone's head here, and it is the
wrong one for this API. Anyone reasoning from memory rather than checking will be off by one release
in the direction that ships a broken build.

**There is no fallback to fall back to, and that is the sharp part.** The obvious fix — runtime
`GraphicsCaptureSession::IsSupported()` with a GDI fallback — **cannot be built on `xcap`**, which is
the crate in use. Its two capture paths are mutually exclusive *at compile time*
(`xcap-0.9.8/src/windows/mod.rs:5` is `#[cfg(not(feature = "wgc"))] mod gdi;` against `mod.rs:8`
`#[cfg(feature = "wgc")] mod wgc;`), and `IsSupported` appears nowhere in the crate. Enabling `wgc`
therefore **deletes** the GDI path rather than sitting in front of it. Keeping both would need a
second capture crate or hand-written WinRT/D3D11 FFI.

That inverts the finding: with no fallback available, the version floor stops being a graceful
degradation and becomes a **hard requirement**, which is what makes the 1803-vs-1903 confusion above
worth writing down.

**Also checked and disqualified:** DXGI Desktop Duplication, the one alternative that would give
correct capture without WGC's yellow border. It fails `DXGI_ERROR_UNSUPPORTED` against the discrete
GPU on a hybrid-GPU system — which describes every gaming laptop, i.e. precisely the machine this
product cares about.

**Fix.** WGC only, failing loudly, with the OS build checked in `preflight` — never an unconditional
switch, and never a fallback, which cannot exist. Either state the floor in the README or keep the
README's promise true.

Found by a peer session's validation pass and recorded here because that pass was report-only. The
fix line above is its **second** version: the first said "runtime `IsSupported()` with a GDI
fallback", which reads as obviously correct and does not compile. Verified against the vendored
source before rewriting, which is the only reason it was caught.

### O42 · Helper lifetime is reconstructed at teardown instead of established at spawn

`session.rs`'s `spawn_piped` calls `CreateProcessAsUserW` with `CREATE_UNICODE_ENVIRONMENT |
CREATE_NO_WINDOW` and **no job object**, so the service creates a child and immediately forgets it is
the parent. O21's fix then re-derives "which processes are mine" from the process table at install
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
`std::thread::spawn` (`server.rs`) with no shutdown signal, which is a genuine root cause but a
*shallower* one — it covers `sc stop` and OS shutdown only, not a crash, not `panic = "abort"`, not
the `nestwatch run` CLI path. Worth doing for hygiene; not a substitute. If done, the seam is the
`axum_server::Handle` already held in `server.rs`, not the `mpsc` in `service.rs`, which is
single-receiver and already consumed.

### O43 · The certificate's recorded SANs come from a different probe than the certificate

`install()` calls `cert::reachable_hosts()` and records the result as `cfg.cert_sans`; the SANs
actually baked into the certificate come from a **separate** `reachable_hosts()` call inside
`cert::generate`. If the machine's address changes between those two moments, the config
permanently claims SANs the certificate does not carry.

Why that matters more than a stale field: the next install decides whether to reuse the certificate
with `covered = cfg.cert_sans == hosts`. That comparison is the only thing standing between a routine
upgrade and re-issuing the certificate — which invalidates the exception every paired phone and
laptop accepted, and, as the comment there says, trains the parent to click through trust warnings.
A wrong `cert_sans` makes that decision on a list that was never true.

**Fix.** Probe once in `install()` and pass the list into `cert::generate`, so what is written to the
config is by construction what is in the certificate.

**Trigger.** Next change to the certificate path. Pre-existing; found during a cleanup review of the
uninstall work and recorded rather than fixed, because it is not that change.

---

## Fixed

### ~~O21 · An orphaned watcher never exits, so service restarts leak helpers~~ — **fixed**

`emit` wrote a sample and **discarded the write error**: `if writeln!(...).is_ok()` with no `else`.
Nothing else in `run()` could end the process — the pump exits only on `WM_QUIT`, which nothing
posts — so when the service went away (crash, upgrade, `sc stop`) the helper kept running forever,
holding a `SetWinEventHook` and waking every 5 seconds to write into a pipe nobody was reading.
`spawn_piped` uses no job object, so nothing bound its lifetime to the service's. One orphan per
service restart, accumulating until sign-out.

**The consequence was larger than the leak, and is why this was fixed rather than filed.** The
helper's image *is* the installed binary, and Windows holds a running executable open. So a leftover
helper made `std::fs::copy` fail on upgrade and `remove_dir_all` fail on uninstall — meaning an
update silently never applied, and an uninstall left the binary behind while reporting success. The
codebase already half-knew: `deploy` named "a lingering helper process" as a likely cause of a failed
copy and did nothing about it.

**Fixed on both ends**, because either alone leaves a window open:
- The sample write moved to `foreground::write_sample`, which **returns** its error; `watcher::emit`
  exits the process when it fails. Extracted rather than fixed in place so the broken-pipe path is
  unit-tested on a machine with no Windows — it was otherwise the one path only the target could
  exercise. Mutation-checked: restoring the original swallow fails two tests.
- `install` terminates any resident helper still running the installed binary **before** overwriting
  it (`deploy`) and before deleting it (`remove_service`), and waits for each to actually die —
  `TerminateProcess` only initiates termination, so returning early would reintroduce the same
  sharing violation intermittently.

Selection is on the **full image path**, never the file name: a child can put a file called
`host-health.exe` anywhere they can write, and matching by name would let them choose what an
elevated installer terminates. `helpers_to_terminate` is pure and tested on every platform for
exactly that reason, including that it never selects the installer's own pid.

**Still unverified on Windows**, like everything else in this tier. §D2 gained steps for it.

Raised here, then resolved. Kept rather than deleted, so nobody re-derives a question already
answered — and because *how* a finding was proved fixed is worth more than the fact that it was.
Each was confirmed by mutation: break the fix, watch the named test fail, restore.

### ~~O8 · The dashboard's logic is the least-verified code that ships~~ — **fixed**

**Two of three steps are done.** The scripts are now `assets/app.js` (744 lines) and
`assets/ask.js` (136), out of the markup, and `script-src` no longer admits `'unsafe-inline'` as a
result — an inline `<script>` can no longer run on either page, which is the directive that
matters most where injected content would land. `no_inline_script_on_any_served_page` holds that
shape, since the failure mode is silent.

**The JavaScript now has tests** — 81 of them, on `node:test` — no framework
installed, so the addition
costs the project nothing it was not already carrying. They cover the pure decision and formatting
methods: `compareVersions`, `isEnforcerStale`, `stBarPct`, `stDayLabel`, `stBarClass`,
`anyRulesSet`, `fmtBytes`, `stRecentDayWith` and its three wrappers, and the approve/deny decision.
Every mutation tried against them fails at least one test.

The DOM-facing note below is now narrower than it was. `loadList` and `loadToday` **are** covered —
the harness already injected `fetch`, so the network-shaped methods were reachable all along without
a DOM, and nobody had looked. That found two more silent failures of O10's exact class: an HTTP
error status never reached the `catch`, so the error messages three callers passed could only ever
fire for a dropped network; and the Today card read its placeholder zeroes out as measurement before
anything had loaded. What genuinely still needs a DOM is narrower — the polling loop, the screenshot
lifecycle, and Alpine's own rendering.

Writing them found O10 on the first run — the staleness indicator reporting healthy enforcement
for a service the page could not reach. That is the argument for this entry, made concrete: the
first tests ever run against this file found a safety-relevant bug in it.

**What remains.** No linter over the two files, and the DOM-facing half is still untested — the
polling loop, the screenshot lifecycle, the error paths. Testing those needs a DOM (jsdom or a
headless browser), which is a materially larger dependency decision than `node:test` was, and is
the kind of thing to decide deliberately rather than adopt in passing. Note it would *not* have
caught O9 either: that was a namespace bug in the markup, which is why the guard for it is a
source scan.

**The point here is that it is the same fix twice.** Moving the script to `assets/app.js` is what
makes it both lintable/testable *and* CSP-tightenable; neither is clearly worth the migration alone,
and together they are.

**The cost, measured — and the first estimate in this entry was wrong.** It claimed
`@alpinejs/csp` "forbids inline expressions, so every `x-text`/`x-show` in 1,650 lines has to move
into the component object", sourced from a GitHub discussion rather than the documentation. The
documentation says otherwise, and counting the markup settles it. Of **264** Alpine directive
attributes in `index.html`, **14** are incompatible:

| | count | verdict |
|---|---|---|
| Template literals | 10 | must become string concatenation or a getter |
| Spread (`[...days].reverse()`) | 1 | must become a getter |
| `Math.round` (inside one of the 10) | 1 | globals are unreachable; move into the component |
| `??` / `?.` | 3 | **undocumented either way** — verify before relying on it |
| `x-model` | 23 | **works** — the discussion claiming otherwise is stale |
| Dotted paths (`today.used_mins`) | 20 | works |
| Comparisons, ternaries, arithmetic, `+` concatenation, method calls | — | all work |

So this is roughly 5% of the directives, not all of them (it was 17 of 268 before O9's fix
retired four template literals along with the SVG chart). That changes the conclusion: the blocker
was never the markup, it is that until this entry's second step there were **233 Rust tests and no
JavaScript tests at all**, so a runtime
swap under the parent's only interface has nothing to catch a regression.

**What is left, in order.** The relocation and the unit tests are done. Next is `@alpinejs/csp`,
which is what `'unsafe-eval'` was still paying for. It looked, at the time this was written, like a bounded job — 14 of 264 directives
— and the tests above cover the methods those directives call, so a swap that broke the component
object would be caught. What would *not* be caught is a directive that stops evaluating, since
nothing tests the rendered DOM; a headless smoke test of both pages is the honest prerequisite,
and it is the same tooling decision the paragraph above defers.

**Fixed, 2026-08-25.** `script-src` is now `'self'` — no `'unsafe-inline'`, no `'unsafe-eval'`.
The page ships Alpine's CSP build (3.16.3, 69,625 bytes against the standard build's 46,346), which
parses attribute expressions with its own parser rather than `new Function` and reaches no globals.
`x-data="app()"` became `Alpine.data("app", app)` plus `x-data="app"`, since a global is exactly
what the build cannot see.

**The cost was 26 directives of 351, not 14 of 264 — and the page had grown in between.** Eleven
template literals, one spread, and fourteen uses of `?.`/`??`, each moved into a getter or method.
Nothing else changed: property paths, ternaries, comparisons, method calls with arguments,
assignment, `x-model` and array literals all still work in an attribute.

**The two undocumented categories were settled by probing the build, not by reading.** A throwaway
page against the real CSP build reported, in its own words: `?.` → *Unexpected token: PUNCTUATION
"."*; `??` → *Unexpected token: PUNCTUATION "?"*; a backtick → *Unexpected token: OPERATOR*. That
matters because this entry already records one confident claim — that `x-model` does not work,
sourced from a GitHub discussion — which the documentation and now the build both contradict.
Reading about this build has been wrong twice; running it has been right twice.

**The spread is the dangerous one and it is why there is a guard rather than a console check.** It
produces *no error at all* — the loop simply renders nothing, which is precisely how O9 shipped a
chart with thirty days of data and no bars.
`no_alpine_expression_needs_more_than_the_csp_build_can_parse` fails the build on any of the four,
confirmed by injecting each in turn.

**Verified by running it under the tightened policy**, which is the only check that means anything
here: with `script-src 'self'` actually served, the dashboard renders with **zero console errors**,
and the range selector, day pinning, theme switch and collapsible cards all still work — each of
which exercises expressions that were migrated.

**What remains open from this entry** is narrower than it was: there is still no linter over the two
scripts, and Alpine's own rendering is still only checked by a person driving a browser rather than
by anything automatic. Both are smaller questions than the one this entry was really about.

### ~~O1 · Curfew's per-tick state has two owners~~ — **fixed**

`curfew::Enforcer` owns `deadline`; a loose local in `run_enforcer` owns the `Countdown`. So no
single function answers "what should curfew do this tick" — the two machines are joined only in the
loop body.

**Cost, concretely.** Any rule coupling them has nowhere to live and no way to be tested: *don't
warn while a shutdown is already pending*, *re-arm the countdown when a shutdown is aborted*,
*suppress the warning on an abort tick*. Each would land in the loop as an ad-hoc `if`, invisible to
the test helpers. The rules enforcer has a test pinning exactly this kind of interaction
(`countdown_is_silent_once_the_budget_is_spent`) because both outcomes come out of one call; curfew
**structurally cannot** have the equivalent. A symptom you can see today: `active` is derived three
times per tick from the same `now` (`curfew.rs` — the loop, `bedtime_warning`, `mins_until_active`).

**Fix.** Move `Countdown` into `curfew::Enforcer` as a field and fold the warning into `tick()`,
taking the next-window state as an input so the enforcer stays config-free and clock-free, and
returning the warning alongside the `Action`. Tests then drive the real enforcer. This does *not*
require matching the rules enforcer's `Vec<RuleAction>` shape — a tuple is enough.

**Fixed, 2026-08-25.** `Countdown` is a field of `curfew::Enforcer`, and `tick` returns
`(Action, Option<u32>)` — both answers from one call. The next-window state arrives as an argument
(`Upcoming`), so the enforcer stays free of the config and the clock, which is what lets the tests
drive the real one instead of a free helper. `bedtime_warning` is gone; the `evening` test helper
now runs the actual enforcer over a simulated evening, computing exactly what `run_enforcer`
computes.

**Two of the three coupling rules this entry named are now enforced rather than merely true.**
*Don't warn while a shutdown is pending* and *suppress the warning on an abort tick* live in `tick`,
which returns `None` for the warning in both states regardless of what the caller observed. The loop
happens to pass `Upcoming::Nothing` in both cases today, so **no behaviour changed** — what changed
is that the guarantee moved from "the caller is careful" to "the enforcer cannot do otherwise". The
third rule, *re-arm the countdown when a shutdown is aborted*, is now explicit in the same place.

**One distinction worth not losing.** `Upcoming::Nothing` and `Upcoming::In(None)` look
interchangeable and are not. `Nothing` re-primes the countdown, so the reading after it announces
nothing; `In(None)` records a real observation of "further off than we can see", from which the next
reading *can* cross a threshold. Collapsing them would announce bedtime to a household that had just
switched curfew off — `nothing_to_count_down_to_is_not_the_same_as_a_distant_window` fails on
exactly that, confirmed by mutation.

Four new tests, and both mutations checked: forcing the suppression off fails one, collapsing the
two `Upcoming` cases fails two.

### ~~O11 · The dashboard is shaped for a desktop and arrived at from a phone~~ — **fixed**

**Shipped: the at-a-glance strip.** Three answers — is enforcement running, how much time is left today, is anything waiting — in one full-width row above every card, each with an explicit *unknown* state distinct from its good and bad ones. Nine tests.

**Also shipped, and all three were found by a person looking rather than by any gate.** The switch
beside "Curfew" carried only an `aria-label` — named for a screen reader, blank to everyone else, on
the control that decides whether a child's PC powers off at night. `every_form_control_can_be_named_by_a_screen_reader`
passed the whole time, and this entry was marked fixed on the strength of it. The bedtime time
fields were clipping their picker icon behind the digits, with `scrollWidth == clientWidth` so
nothing measured it as overflow. And the selected item in both button groups was painted
`btn-primary` — the colour reserved for *Save* and *Take screenshot* — so a settled choice looked
like a pending action.

The lesson is narrower than "test the UI": **an automated check answers exactly the question it
asks.** A name check cannot see an invisible label, an overflow check cannot see an overlap the
browser considers legal, and nothing has an opinion about whether a colour means the right thing.
Each of those passed while the defect stood.

**The collapse shipped too — with a browser open, which is what it was waiting for.** Five
`<details>`: Routines, Time codes, Recent access, Usage history, Change password. The page now
presents five headings where it presented five full panels. Plain `<details>` and no JavaScript —
the browser handles the toggle, Enter, Space, and announcing expanded state, none of which is worth
reimplementing badly.

Verified by driving it rather than reading it: all five collapsed at 56px, opening one expands it,
the chevron rotates, the summary takes keyboard focus, and **pressing Refresh inside an open panel
does not collapse it**. That last one is the trap this shape sets. A control inside a `<summary>`
is activated *and* toggles the panel, so it reads as a button that does not work. Both arrangements
parse, render and screenshot identically; only clicking one tells them apart.
`no_summary_swallows_a_control` now fails the build on it — confirmed by putting a button back in.


The whole first-run story is phone-first — `install` prints a QR *because* "typing an IP plus a
passphrase on a phone keyboard is the single biggest piece of friction in first-time setup", which
is the code's own comment. The parent scans it and lands on a single page of twelve stacked cards
with no navigation, no anchors and no search.

**Measured:** twelve cards, and **zero** `sm:` breakpoints in the whole document. Below the medium
breakpoint the grid collapses to one column and the parent scrolls twelve cards in source order.

**Cost, concretely.** Source order is not priority order. The three questions a parent opens this
page with — *is enforcement running, how much time is left today, is anything waiting for me* — are
answered in three non-adjacent cards, one of them below the fold on any phone.

**Fix, smallest first.** A status strip at the top answering those three, before any card. Then
collapse the rarely-touched cards (Routines, Time codes, Recent access, Usage history, Change
password) behind `<details>`, which costs no JavaScript and stays keyboard- and screen-reader
reachable. Tabs are the conventional answer and are worse here: they hide state a parent is scanning
for.

**Worth knowing:** `ask.html`, the child's page, is 63 lines, single-purpose, and carries
`inputmode`, `autocapitalize`, `aria-live` and `role="alert"`. The page built for the child is
better made for a phone than the page built for the parent.

**Trigger.** Before the next card is added, or the collapse work grows with it.

### ~~O12 · Nothing tells the parent a request is waiting~~ — **fixed**

**Shipped: the tab title carries the pending count.** `(2) Nestwatch`, and `Nestwatch` — never `(0)` — when the count is unknown. Cleared on sign-out so a login page cannot advertise the previous session's child.

The research that settled the mechanism is worth keeping: Web Push needs an external push service, which the privacy promise forbids. The **Badging API** (`navigator.setAppBadge`) badges an *installed* app's icon, and `MOBILE-APP.md` already establishes an installable page cannot work here — a home-screen app does not inherit the certificate exception. The **Notifications API** needs a secure context, and whether an accepted self-signed certificate on a private IP qualifies is **unverified** — localhost is the documented exception, not private addresses generally. The title needs no permission, no service worker, and no external anything. Its limitation stands: a tab must be open somewhere.


A child submits from `/ask`. It is queued durably, capped at five, folded to latest status, and
rendered on a card the dashboard polls every sixty seconds. Then nothing happens: no sound, no
notification, no unread count, no change to the page title. **Zero** hits for `Notification`,
`document.title` or any badge mechanism in `app.js`. The one concession is an `aria-live` on the
heading, so a screen-reader user is told and a sighted user is not.

For a feature whose whole value is a fast answer to "can I have twenty more minutes", that is the
feature not working. The card's *visibility* half was fixed (see the changelog); being told is not.

**What survives the privacy promise.** Web Push is out — it needs an external push service, and
nothing leaves the house. But push is not the only mechanism:

- **Title badging** (`document.title = "(1) Nestwatch"`) needs no permission, no API surface and no
  uncertainty. This is the recommendation.
- **The Notifications API** works without a service worker or any push service, but requires a
  secure context. An accepted self-signed certificate *should* qualify and this is **unverified** —
  five minutes in a browser settles it; do not build on it until someone has.

**Honest limitation.** Every option needs a dashboard tab open somewhere. Nothing reaches a phone in
a pocket without an external service, and that is an accepted cost of the privacy promise rather
than an oversight to engineer around. Say so in the README rather than leave a silence.

**Trigger.** Do the title badge with the next dashboard change; treat notifications as gated on that
one browser check.

### ~~O13 · Category time exists for today and vanishes tomorrow~~ — **fixed**

**Shipped end to end.** `PreRollover` carries `per_group_secs`, `rollup_row` writes a `groups` map, `DayRow`/`ParsedRow` carry it, `Report` gains `group_totals`, and the card renders categories *above* the executable-name lists because "Games: 14 h" is a sentence and twenty file names is a puzzle.

**`ParsedRow::detail()` extended to `(knows_groups, knows_focus, count)`** — generations ahead of the count, newest first. Ranking on the count alone once let a wide legacy row outrank a narrow modern one and silently discard the richer data; groups are a third generation and had to lead.

**The deferral in the first draft of this entry was wrong and is withdrawn.** It said the change should wait for on-device verification "because it changes the stored format". `focused` and `pages` were added to the rollup row in exactly this shape, and `parse_row` reads an absent key as *not recorded* rather than as zero. Same additive shape, same safety.


`AppGroup { name, apps, limit_mins }` already exists with a shared pool, and `today_summary` already
reports per-group minutes against the limit. `rollup_row` records `apps`, `focused` and `pages` —
and **no group data at all**, so category history does not exist. A parent can see "Games: 40 min"
this afternoon and never "Games: 14 h this month".

Categories are the primary view in every comparable product; Apple's taxonomy is public and stable
(Social, Games, Entertainment, Creativity, Education, Health & Fitness, Information & Reading,
Productivity & Finance, Shopping & Food, Travel, Utilities, Other) and iOS 27 adds per-category
limits. It is the view that turns thirty rows of executable names into a sentence.

**Fix.** Add a group map to the rollup row, and ship a starter set drawn from that taxonomy that a
parent can edit. Keep an explicit uncategorised bucket rather than a catch-all, so a group covering
nothing is visible as a gap. Both additive; neither changes what enforces.

**Trigger.** After the current build has been verified on-device — this writes a new key into the
daily history, and a format change is a bad thing to stack on a release nobody has watched run.

### ~~O14 · Prose in the served files compiles into the stylesheet~~ — **fixed, and the real cost was 15%**

**Shipped:** `web/scripts/strip-comments.mjs` writes comment-free copies into a git-ignored `web/.scan/`, and `@source` points there. A hand-written scanner rather than a regex, tracking string and template state so `"https://x"` survives; string contents are deliberately preserved because `stBarClass` names utilities appearing nowhere else.

**The prose was a rounding error beside what was actually wrong.** `@source` does not *replace* Tailwind v4's automatic source detection — it adds to it, and automatic detection was scanning the whole `web/` directory: `package.json`, `app.css`'s own comments, the test files. `@import "tailwindcss" source(none);` turns it off. **102,181 → 86,736 bytes, 15,445 saved.** That was true before any of this work; two careful comment rewordings were optimising the wrong thing by two orders of magnitude.

**Two things learned the hard way, both by measuring rather than reading:**

- The first build after repointing `@source` made the stylesheet *grow*, and `.steps` reappeared — the component removed earlier. It was `strip-comments.mjs` itself, whose documentation necessarily lists `step`, `list`, `tab` and `mask` to explain the hazard, being picked up by automatic detection. **A file explaining the trap was springing it.**
- Pointed at `alpine.min.js`, the scanner removed **13,543 bytes from a file with no comments** — minified code is full of regex literals and divisions, and a `/` there is not a comment marker. Vendored files are now excluded, and the script throws if any file loses more than half its bytes, because that is not comments, that is a mis-parse deleting code.


Tailwind finds class names by scanning `assets/**/*.{html,js}` as raw text, so it cannot distinguish
a class from English. daisyUI's components are named with ordinary words — `list`, `tab`, `step`,
`range`, `join`, `mask`, `collapse`, `tooltip` — so an explanatory comment ships whatever component
it happens to name.

**This is structural, not carelessness.** It has now happened twice in one day, to two different
authors: a comment reading "Width **steps** up with the screen" shipped 2,408 bytes of a widget the
product does not have, and a later pass added `tab`, `list` and `step` for another 1,544. Both were
found by measuring, not by review. No amount of care makes English avoid a vocabulary that includes
"list".

**Fix.** A prebuild step that writes comment-stripped copies of the served files into a scratch
directory and points `@source` at those. About twenty lines. It composes safely with the guard that
already exists: if the stripper ever removed something real, `every_class_in_the_markup_has_a_rule_in_the_shipped_css`
goes red rather than the UI going quietly unstyled.

**Why it is open rather than done.** It changes the shared `web/` build while two sessions are
working in this repository, and the current cost is ~1.5% of a stylesheet. Worth doing, not worth
doing unilaterally.

**Trigger.** The next time anyone touches the build, or the third occurrence — whichever comes
first.

### ~~O15 · The child is never told the screen can be watched~~ — **decided and fixed**

**Shipped: one quiet sentence on the child's page** — *"A parent set this up and can see this screen, which apps you use, and how long for."* Not a legal notice and not a warning.

The reasoning, so it is not re-litigated: this product takes the opposite view everywhere else it had the choice. It records page titles and not addresses specifically so it cannot rebuild a browsing history, and declines to read browser history at all as disproportionate. A tool that careful about what it should not know is a strange one to have a silent camera in. Reverting is one line if a household disagrees.


Not a vulnerability — a product question, recorded because the answer should be deliberate rather
than default.

`GET /api/screenshot` captures the primary monitor on demand. It is audited, so the *parent* has a
record, and `SECURITY.md` describes it plainly — for the parent. The child's page, the only surface
they ever see, mentions screen capture **zero** times.

Competitors mostly do not disclose either, so this is not out of step. But this product made the
opposite choice everywhere else: it records page *titles* and not URLs specifically so it cannot
reconstruct browsing history, and `FOREGROUND-TRACKING.md` declines browser-history reading as
disproportionate. A tool that reasons that carefully about what it should not know is a strange one
to have a silent camera in.

**Three options, none of them obviously right.** Leave it silent; add one line to the child's page;
or show a brief on-screen indicator when a capture happens. The middle costs a sentence and makes
the tool's honesty consistent with itself.

**Trigger.** None. Decide it, write down why, and move it to *Considered and declined* if the answer
is no.

### ~~O3 · `today_summary` is documented pure but reads process globals~~ — **fixed**

Its doc said "Pure (no I/O) so it's unit-tested" while calling `crate::heartbeat::worst_age_secs()`,
which reads two process-global atomics **and** `SystemTime::now()`.

The cost was visible in the tests: four of them passed only because *none* asserted on
`enforcer_age_secs`. The impure field was precisely the untested one, because pinning it would have
coupled the test to whatever else in the binary had called `beat()`.

`today_summary` now takes `enforcer_age_secs: Option<i64>`, and `api::usage_today` reads the
heartbeat at the edge alongside its other I/O. The function is pure for real, and
`today_summary_passes_the_enforcer_heartbeat_through` asserts all three cases — fresh, stale, and
`None` (never reported, which after one tick's uptime means the loops never started). Confirmed by
mutation: hardcoding the field to `None` fails that test while the other four stay green.

### ~~O10 · The dashboard reported healthy enforcement for a service it could not reach~~ — **fixed**

Found by the first JavaScript test ever run against `app.js`, on the first run.

`isEnforcerStale(age)` returned `age === null || age > ENFORCER_STALE_SECS`. The strict `===` was
deliberate and documented: the initial `today` literal carries no `enforcer_age_secs` key, so a
loose check would read `undefined` as stale and flash the warning on every page load until
`loadToday()` resolved. The comment said exactly that.

**The cost of that trade was not written down.** `loadToday()` routes through `loadList`, which
catches a failed fetch and — with no `errMsg` passed — does nothing with it. So a load that never
succeeds leaves `today` at its initial value permanently, `enforcer_age_secs` stays `undefined`,
and `undefined === null` is false while `undefined > 150` is also false. The function answered
**"enforcement is fine"** for a dashboard that could not reach the service at all. `heartbeat.rs`
calls a silently dead enforcer "the worst failure this product can have"; this is the browser
under-reporting exactly that, and the failing case is the one where the service is down — which
is when the warning is the whole point.

**Fix.** Split the two questions the `===` was conflating. `isEnforcerStale` now uses `== null`,
so an absent age counts as stale like an explicit one — matching how the rest of the file already
tests for absence. The flash is prevented instead by `todayAsked`, set once the first attempt
finishes whether it succeeded or not, and `stEnforcementStale()` stays quiet until then. The
"Today" banner was calling `isEnforcerStale` directly, bypassing the gate; both banners now go
through `stEnforcementStale()`, which is what the shared helper's own comment said it was for.

**Verified by mutation**: restoring `=== null` fails two tests, and removing the `todayAsked` gate
fails a third. Both properties — no flash before the first load, honest reporting after it — are
asserted, so neither can be traded away for the other again without a test going red.


**A later pass found the same class twice more, and got the fix wrong the first time.** The
`loadList` helper read `if (r.ok) { … }` with no `else`, so an HTTP error status never threw and
never reached the `catch` — meaning the error messages three callers passed were dead code for the
failure that actually happens; only a dropped network ever produced one. And the Today card read its
placeholder zeroes out as measurement before anything had loaded.

The instructive part is the proposed remedy for the second. It was "gate the figures on
`todayAsked` — the flag exists and already means exactly this". It does not:
`todayAsked` is set **whether the fetch succeeded or failed**, deliberately, because keeping the
staleness warning reachable when the service is unreachable is this entry's whole point. Gating the
numbers on it would have revealed them the moment the first attempt finished, including the failure
it was meant to suppress. The reviewer had quoted the flag's semantics correctly two sections
earlier in their own notes and still read the name for the meaning.

`day` cannot stand in either: `today_summary` emits `"day": usage.day.map(…)`, so a *successful*
response carries `day: null` on a machine whose enforcer has not yet written a tally — "nothing
recorded", not "nothing received". What shipped is `today` starting as `null`, so the data's own
presence answers the question and there is no second flag to keep in step.

### ~~O9 · The screen-time chart rendered no bars at all~~ — **fixed**

Not raised by a review — found by running the dashboard in a browser, which nothing else here had
done. The chart repeated its bars with `<template x-for>` **inside the `<svg>`**. A `<template>`
parsed inside `<svg>` belongs to the SVG namespace, is not an `HTMLTemplateElement`, and has no
`.content`; Alpine's `x-for` reads `template.content.children`, threw, and rendered nothing. It
shipped in 0.2.3.

**Why every existing gate missed it.** It is not a Rust bug, not a type error, and not a
formatting or lint issue: it is a DOM namespace rule that only exists once a browser parses the
file. 233 tests, clippy on two targets, and a cross-compile all passed over it. The failure was
silent in the UI too — the summary figures above the chart and the day-by-day table below it both
read from the same data and were correct, so the page looked sparse rather than broken, and the
only evidence was eight console errors nobody was looking at.

**Fix, as shipped.** The bars are HTML `<div>`s in a flex row, so no `<template>` sits inside an
`<svg>` anywhere. Per-bar hover text became a `title` attribute, which is more reliably surfaced
than SVG's `<title>` element; the hatch for unmeasured days moved from an SVG `<pattern>` to a
`.st-nodata` rule in `web/src/app.css`, written in theme variables so it still follows both themes.
`stBarClass`/`stBarStyle` keep the three states named in one place and keep the markup to method
calls — which also removes four template literals from the O8 migration's tally.

**Verified by running**, since that is the only thing that could have caught it: 30 bars where
there were 0, all three states present (10 hatched, 16 within budget, 4 over), the deliberate 3%
floor keeping a measured-zero day visible and hoverable at 3px, and the console going from 20
error lines to none. `web::tests::no_alpine_template_inside_svg` fails if the shape returns —
confirmed by putting it back.

### ~~O7 · The binary could not tell you which version it is~~ — **fixed**

Nothing in `src/` referenced `CARGO_PKG_VERSION`, and the release profile sets `strip = true`, so
the shipped `.exe` carried no version string at all — found by checking a published artifact, which
contained its own version number nowhere. There was no `--version` flag, and `doctor` did not print
one either.

**Why it mattered.** This is a tool installed by hand, from a downloaded file, onto a machine
visited rarely. The question you could not answer while standing at that PC was *which build is
actually running*, which is the first thing worth knowing when something behaves unexpectedly, and
the one that decides whether a given security fix reached the machine rather than just the
repository.

**Fix, as shipped.** `env!("CARGO_PKG_VERSION")` behind `crate::VERSION`, surfaced by a `version`
command (`--version` / `-V`), in `doctor`'s report header, and in the usage text. `strip = true`
does not affect `env!` — it is baked in at compile time as an ordinary string constant. Verified
where it counts: a **stripped release build** reports its version from both surfaces, and a test
pins the doctor header so it cannot silently drop out of the one report you read when something is
wrong. Confirmed by mutation: dropping `v{}` from the header format string fails
`the_report_header_names_the_build` and nothing else.

---

## Considered and declined

Weighed in review and deliberately not done. Re-raise only with new evidence.

| | Why not |
|---|---|
| A general `control::call` wrapper over all seven `SystemControl` methods | Two reviewers disagreed. The failure messages are call-site-specific by design (`"budget lock FAILED — screen time is not being enforced right now"`), so wrapping all seven means seven wrappers each taking a message parameter — strictly worse. The one concrete cost, two dropped `JoinError` arms, was fixed directly. `control::notify` stays because it is a *policy* wrapper (failure is a debug-level non-event; delivery is a boolean both callers branch on), not merely an async shim. |
| Splitting `RulesEnforcer::decide` into app-rules and budget halves | The seam is genuine — the two halves share nothing but the accrual that already runs first. Declined as a restructure of the security-critical pure function during a cleanup pass, not because the analysis was wrong. Revisit alongside O2. |
| Hoisting `parse_hm` out of curfew's lookahead probe | **Measured**: worst realistic config (7 windows) is 7.2µs per 30s tick, 0.000024% duty cycle. Hoisting is 4.7–7.6× faster and buys 2.5–20ms *per day*. |
| Trimming `sysinfo`'s per-tick process refresh | **Measured** at 8% of a 4.9ms call already on the blocking pool. Caveat: measured on macOS; the Windows syscalls it would skip have a different cost profile and are unverified on target. |
| Lengthening the 30s tally-save interval | The child is the adversary and a reboot is their tool. At 30s a reboot forfeits ≤30s of tally and costs more than that in boot time; at five minutes it becomes "reboot, gain five minutes, repeat". Write-on-change was taken instead — it removes most writes with the guarantee fully intact. |
| Renaming `rules::Usage` → `Tally`, and `RuleAction::Warn` → `LimitReached` | Both would read better. Rename churn across `api.rs`, `doctor.rs` and the tests isn't worth it on its own; fold into O2 if that lands. |
| Sharing one `CHECK_INTERVAL` between the two enforcers | They are independent loops and nothing breaks if they diverge; the comment is descriptive, not a constraint. The real constraint — that a loop must tick faster than the smallest warning threshold — is now documented on `WARN_AT_MINS` instead. |
| **Separating the child's `/time-request` audit line, to stop log eviction** | Raised as a live hole; **refuted by reading the code**. The concern was that an unauthenticated child could append audit lines at 5/min until the 2 MB log and its single `.jsonl.1` backup rolled every login and kill off disk. `api.rs` already audits **only submissions that joined the queue**, and the queue caps at `MAX_PENDING` — so further growth requires a *parent* action to resolve one. The comment there records it as the fourth site of that defect class, after `login`, `pair` and `logout`. Nothing to do. |
| **Moving `heartbeat::beat()` to the end of the enforcer loops** | The doc said it was called at the end "so it proves the tick finished"; the code calls it at the top. The tempting fix is to move the code. **Don't** — `run_rules_enforcer` has two early `continue` paths, and one of them is the parent pressing **Pause**, so beating at the end would report enforcement as dead every time the feature is used. The doc was corrected instead; see `heartbeat.rs`. |
| **Widening `is_lan` to admit CGNAT (`100.64.0.0/10`)** | Confirmed by running: `Ipv4Addr::is_private()` excludes that range, so a parent tunnelling in over Tailscale is rejected by the app-layer gate. Declined anyway — the tool is LAN-only by design, and admitting a range no home network uses would extend the trust boundary for every install to fix an explicitly unsupported setup. Documented in the README instead, so it reads as a boundary rather than a bug. |
| Adopting `clippy::unused_qualifications` | Clean everywhere except 9 sites, all of them in `curfew.rs` and `rules.rs` — the enforcement path, including the pure function a previous pass already declined to restructure during cleanup. A style lint is not a reason to touch it. The other lints adopted in `Cargo.toml`'s `[lints]` were each verified to produce zero warnings first, so none of them opened a cleanup. Same for `missing_docs` (84) and `clippy::str_to_string` (36). |
| Widening `is_lan` — **second look, still no** | The original row below stands, and the case for widening got weaker, not stronger: Tailscale run as a *subnet router* on another machine already reaches this service from `192.168.x.x`, because subnet routers masquerade routed traffic to their own LAN address by default. So a working Tailscale arrangement exists without touching the allowlist — and it is the better one anyway, since it keeps the tunnel daemon off the monitored PC. README corrected, which had claimed Tailscale simply does not work. |
| **Back-dating the return from idle**, so the reconciliation poll could back off while nobody is there | Raised as an accuracy fix and **withdrawn after the counter-argument**. `GetLastInputInfo` reports when input last *happened*, not when the user *returned* — on resume those are the same instant, so any correction guesses in the over-credit direction. Understating is the direction this codebase chooses deliberately elsewhere (`countdown`'s floor division, `clamp`'s scaling). The drop is real: up to one poll interval of genuine use per idle episode. Revisit only if an idle-poll back-off is actually wanted, and then measure first. |
| **Replacing the enforcement process scan with `WTSEnumerateProcessesW`** | One call, one buffer, zero per-process handles, and it carries `SessionId` — which is the key O6's per-account half needs. The `Win32_System_RemoteDesktop` feature is already enabled, so it costs no new dependency. **Killed by the documentation:** a caller outside the Administrators group does not get an error, it gets a *partial list*. On the enforcement path a partial process list is a silent fail-open — apps that should have been killed are simply absent. Running as LocalSystem satisfies the requirement today, but that is a property of the install rather than of the code, and the failure is invisible when it breaks. |
| **Reusing one long-lived `sysinfo::System` across ticks** | Strictly the largest remaining win without new FFI — sysinfo would skip `ProcessInner::new` for processes it already knows. **Declined:** it holds a `PROCESS_QUERY_INFORMATION \| PROCESS_VM_READ` handle open for every live process for as long as the `System` lives. A SYSTEM service permanently holding a few hundred read handles to everything on the machine is a textbook EDR heuristic, and being quarantined by antivirus costs a family more than the syscalls do. Revisit only if a real measurement shows the narrowed refresh is insufficient. |
| **An embedded database — DuckDB, as used in a sibling project** | The precedent is real and defeats the obvious objection: that project builds `x86_64-pc-windows-msvc` natively on `windows-latest`, exactly as this one does. **What does not transfer is the shape.** A realistic rollup row measures 763 bytes typical / 1,891 worst case, so ten years is 3,650 rows and under 7 MiB — a `Vec` holds the entire history. The shipped binary is 3.79 MiB and DuckDB's bundled amalgamation alone sits at crates.io's 10 MB *source* ceiling. And `bundled` cross-compilation is best-effort and needs a C++ cross-compiler for the target, which would cost the `x86_64-pc-windows-gnu` check that is the only way to lint eight `#[cfg(windows)]` `unsafe` blocks from a Mac. **SQLite becomes defensible** if the model ever changes from one blob per day to a row per `(day, app)` — that is ~11,000 rows a year and makes aggregation a query rather than a fold. Not yet earned. |
| **Shortening the 30-second enforcement tick to improve resolution** | The reflex when someone asks for better tracking, and it buys nothing: focus changes are caught within 250 ms by the watcher's hook, and the tick only decides how often that is folded into the day's tally. Resolution is already an order of magnitude finer than the tick, and every cost in the loop would multiply. |
| **Five suspicions about the install and enforcement paths, all refuted by reading** | Recorded because each is plausible enough that someone will suspect it again. **Localised group names** — `doctor` queries the Administrators group by SID (`S-1-5-32-544`) precisely *because* the name is localised; "Administratoren"/"Beheerders" appear only in the comment saying why. **Secrets written before the lockdown** — `prepare_data_dir` runs `create_dir_all` then `harden_acl` *before* the config is constructed, under a comment stating the ordering is deliberate. **ACL hardening** — `icacls /inheritance:r` strips inherited entries first, grants are by SID, and a failure bails the install rather than continuing. **An arbitrary 825-day certificate** — it is Apple's hard limit, and both bounds are set because Apple measures `not_after − not_before`. **Curfew defeatable by `shutdown /a`** — still on past `deadline + slack` re-issues as the *uncancellable* `ShutdownNow`, so cancelling buys one interval, not an evening. |
| An `Enforcer` trait unifying the two background loops | The genuinely shared skeleton is ~6 lines. The blocks that *look* duplicated aren't: curfew calls `disarm()` when a shutdown fails so it retries with a fresh countdown; the rules enforcer deliberately doesn't, and returns as the uncancellable `ShutdownNow`. A shared helper would extract the boilerplate and leave the divergent part behind. |

---

## Not covered by any of this

**None of the above has run on the target machine.** Everything here was found by reading, tests,
and cross-compilation — the same three gates that were green when `install` failed on real
hardware, and again when `remove_file` turned out not to be exclusive. See
[WINDOWS-TESTING.md](WINDOWS-TESTING.md); it is the only method with a track record of finding what
matters here.
