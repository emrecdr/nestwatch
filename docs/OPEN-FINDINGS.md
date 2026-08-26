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

**Provenance.** Findings come from several independent review passes over this codebase (2026-08),
across four angles — reuse, simplification, efficiency, altitude — plus a security analysis and a
research review of per-app and web-page tracking against primary sources. Entries are not all equally
solid, and each says which it is: some are read directly off the tree and are facts about code that
exists; others rest on a primary source plus a mechanism, and name the one on-device observation that
would confirm or kill them.

Last audited against the tree on **2026-08-26**. Entries that did not survive that audit were removed
or rewritten rather than annotated, per the rules above.

---

## Open

### O2 · `rules.rs` has a real seam between the pure machine and the loop

The file is 2,707 lines (measured 2026-08-26) holding config types, the tally and its
persistence, `Targets`, `today_summary`, the pure enforcer, the async loop, shutdown-abort
coordination, notification helpers, and the tests.

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
`doctor` are its only two consumers. It was `rules::today_summary` until the read moved out to
the edge; the mechanism is unchanged, only where it is called from.

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

### O6 · Screen-time figures are machine-wide and count running, not focused, time

The report added in the screen-time work counts any account at the console, and counts an app while
its process runs rather than while it has focus. Both are conservative for enforcement and
misleading for a report; both are labelled on the card rather than silently accepted.

**Per-account attribution is cheaper than foreground tracking, but it is not "free" — corrected
2026-08-19.** The username genuinely is already fetched and discarded: `session.rs`'s `session_state` reads
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

`POLL` is 5 seconds (`watcher.rs::POLL`) and the page title is re-read on each resolve. Focus *time* is
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
- **A number without its denominator is not a finding.** "2.42 GB/year of writes" headed a
  recommendation to restructure the tally's persistence; against a conservative 10 TB endurance
  budget it is **0.03%**, and the recommendation evaporated. The byte count was correct throughout.
  Three separate numbers in this pass were accurate and misleading in exactly this way, so the rule
  is not "measure more carefully" — it is that a magnitude only becomes an argument once it is
  divided by the budget it is supposed to threaten.
- **Anchor an edit on the attribute, not the signature — and know which gate catches it.** An
  insertion anchored on `fn rollup_row_omits_budget_when_unknown() {` swallowed the `#[test]` above
  it, leaving two stacked on the new test and none on the old one. **The suite went green with that
  test silently not running**, because a test that stops being a test is invisible to the test
  suite by definition — `cargo test` cannot report the absence of something it never registered.
  Only `clippy -D warnings` caught it, as `duplicate attribute` plus `function is never used`. Two
  consequences: anchor on the attribute line when editing near one, and treat a green `cargo test`
  as insufficient evidence on its own for any change that moved test code.
- **Cost what the hardware charges, not what the model counts.** The same withdrawal turned on
  costing *logical bytes* where the device charges *physical pages*: `write_atomic` is
  `File::create` → `write_all` → `sync_all` → `rename`, and 338 B and 3,451 B round to the same
  single 4 KiB page. The payload was the free component; the fsync and two directory updates are the
  cost, and none of them scale with size. Shrinking the payload would have saved nothing and added a
  second fsync. Before optimising a size, confirm size is what is being billed.

**The same trap, one level up: confirm the experiment ran before believing its result.** A mutation
run in the same pass was reported as "the mutant survived, so the test is vacuous" when the `perl`
pattern that was supposed to introduce the mutant had silently matched nothing. The source was
never modified, so the passing test proved only that unchanged code still passes.

This generalises past benchmarks and is the sharper half of this entry, because **a no-op experiment
and a negative result are indistinguishable from the output alone** — both are green. The rule that
separates them: a mutation you believe in is one you *watched fail*. If a mutant is reported as
surviving, re-read the file and confirm the change is actually on disk before concluding anything
about the test.

(The test in question was vacuous anyway, for an unrelated reason — every case had two fields
agreeing, so a mutant collapsing them passed. Being right about the conclusion does not make the
method sound; the next one will not be right by luck.)

**Four instances in one day, which is why this now has its own heading: "the command ran" and "the
command did the thing" are different claims, and only the second is evidence.** The entry above
records two. A later pair made it four, and the two new ones failed in ways the first two do not
cover:

- **A stale artefact answered for a build that never happened.** Measuring the capture backend's
  cost meant building with and without a feature and comparing `stat` output. The without-`wgc`
  build *failed*; `stat` happily measured the previous run's binary still sitting on disk and
  reported a difference of **+0 bytes**. A plausible number, from a real file, for a build that did
  not exist. The correct figure is +33,792 bytes. Nothing in the output distinguished the two — the
  fix is to make the build's exit status a precondition of reading its artefact, not a thing you
  glance at above the number.
- **The working directory drifted and the command ran somewhere else.** `npm run build` was invoked
  from the repository root rather than `web/`, and a success was reported for a build that never
  ran. It then recurred later in the same session in a subtler form: a compound command whose
  earlier `cd` had silently persisted, so a mutation was applied to a path that did not exist, a
  restore-from-backup failed, and the tree was left mutated while the summary said "restored". The
  guard is absolute paths in anything that mutates, and a positive check afterwards — `grep` for the
  thing you expect to be there or gone — rather than trusting the command's own exit.

**A number you derived is not a number you measured, and careful algebra is not a defence.** Three
consecutive wrong answers to one small question in a single session, each produced by someone
reasoning correctly from an incomplete model of the thing:

- "About 150 bytes of headroom" before `strip-comments.mjs` fails the build — wrong.
- A peer's correction to "309 bytes", from `2 × out − src` — also wrong, because it assumed a
  stripped comment contributes nothing to the output. It contributes its newline.
- The corrected "~380 bytes" — wrong again, or rather **not general**: the scanner copies a comment's
  leading whitespace before it recognises the `//`, so the answer depends on indentation. Measured by
  sweeping it: roughly 7 lines at indent 0 and 8 at indent 8. **Deeper indentation buys more
  headroom**, which nobody deduces, and which means outdenting a comment block moves the build
  *closer* to failing.

The shape is the same as the mutation case one level up. All three figures were arrived at by
thinking, all three were plausible, and the question was only settled by running it across a range —
at which point the honest answer turned out to be a range rather than a number.

**Staleness and completeness are two different audits, and a green one says nothing about the
other.** Asked whether the documentation was up to date, two sessions independently swept for
*staleness* — "is anything written here now false" — found and fixed a dozen items apiece, and each
reported the docs as current. Both were right and both had missed the same class, because neither had
asked the other question: **"is anything true missing"**. `uninstall` had never been described beyond
one clause, on the only surface a parent reads; no staleness pass can surface a section that was
simply never written. The peer then applied the distinction a second time to its own new text and
found a completeness gap *inside a completeness fix* — the new bullet did not say that `--purge` is
irreversible and takes every recorded day, the pending requests and the certificate the family's
devices already trust. Run the two passes separately and expect them to fail differently.

**Trigger.** §D2 step 12, which already asks for this. It has simply never been done.

### O28 · Live mode creates a whole process per frame

Every tick runs, in the child's session:
`WTSQueryUserToken`, `DuplicateTokenEx`, `CreateEnvironmentBlock` (which reads their profile
environment), `CreatePipe`, `CreateProcessAsUserW` of the whole 3.79 MiB binary cold, xcap init, a
watchdog thread. All fixed cost, paid to deliver one image. `session::spawn_piped` already exists
and was generalised out of exactly this path for the resident watcher, so a `helper --cast` that
lives for the length of a viewing session is a natural shape. **Do it last**, once the tiers have
shown how much cost is actually left — the frames are now ~30 KB, so the process spawn may well be
the entire remaining bill, or may be noise. Measure first. **And do not fold casting into the
existing `--watch` helper**: that one runs as the child, is killable from Task Manager, and its
supervisor backs off to 30 s between respawns — making it the screenshot source would hand the child
a one-click way to blind the parent, with a delay that grows the more they use it.

### O33 · Nothing detects that the frame has not changed

A child reading or away from the desk
produces a stream of near-identical frames, each captured, encoded and sent in full. An `ETag` over
a hash of the raw frame would let an unchanged screen return `304` with no body. Much cheaper after
the WGC move *if* its frame pool turns out to deliver only on change — **that property is
unverified**; it could not be sourced from the documentation and should be measured rather than
assumed.

### O36 · The usage timeline is bounded to the last few days by `recent(200)`

The 24-hour strip — *"When the PC was in use today"* — is **built and shipped**, and the data
blocker it waited on is fixed: the pause path used to discard `prev_active` without writing a
`session_stop`, so pause→resume produced *start, start* with nothing between. Starts and stops now
pair by construction.

What is left is its **reach**. `recent(200)` caps the events the timeline can see at roughly the
last few days. A longer view wants `recent_matching_including_rotated`, which already exists and is
what the screen-time report uses; the events are already on disk, so only a *per-app-per-hour*
breakdown would cost storage.

**The constraint any extension must keep.** Two orphan sources cannot be fixed by a running
process — chiefly that a service restart cannot write a stop for the session that died with it. So
a start with no preceding stop means *"previous span ended, time unknown"* and must never be paired
across: pairing would shade a bar from an afternoon crash through to bedtime and label it use,
which is the original defect one layer up. The shipped strip draws an unpaired start with **no
width** for exactly this reason, and that is mutation-checked — stretching it to the next start
fails the suite. Anything that widens the window inherits this rule.

**Trigger.** When a longer timeline is actually wanted, not speculatively. The current strip
answers "was he on at two in the morning?" for today, which is the question it was built for.

### O39 · The executable's full path is fetched, used for one character class, and discarded

`watcher.rs::process_name` gets the full image path from `install::process_image_path`, then
`path.rsplit(['\\', '/']).next()` throws everything but the basename away — at the only point in
the system where the path is already in hand and costs nothing to keep. Where a program lives is a
real signal, distinct from what it is called: `C:\Program Files\` means somebody installed it,
`C:\Users\<child>\Downloads\` means a file was downloaded and run, and those are the same
executable name and completely different facts.
<br>**The codebase states the exact principle in shipped code, and applies it in one direction
only.** `install::helpers_to_terminate` decides what to kill with
`path.eq_ignore_ascii_case(&target)` against the full install path, under a comment reading that the
file name is *"the filter, never the decision — `helpers_to_terminate` makes that on the full
path."* `process_name` does the inverse, and the surviving basename then **is** the decision: the
tally key and the blocklist key both.
<br>Keep the **directory class** — installed / user-profile / removable / other — not the path. A
full path is higher-cardinality and more identifying than anything else this system stores, and the
watcher is untrusted input; a coarse class keeps the signal for a handful of bytes and cannot be
used to enforce, consistent with how `foreground_secs` is already fenced off from `decide`. Rules
are configured by name, so enforcement must keep keying on the name.

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

### O49 · The one case the baseline cap was written for is the one case nothing records

`first_seen_in` gives up and returns `None` when the baseline exceeds `MAX_BASELINE_APPS` (2,000
distinct names). `None` is also what it returns for "no focus history", "only one day of it" and
"this is the first day" — so the UI, which renders `None` as no panel at all, cannot tell them
apart, and neither can the logs.

`foreground::MAX_APPS` is 200/day and `rollup_row` writes the map uncapped, so 2,000 distinct
executable names is not something ordinary use produces. It is reachable essentially only by the
deliberate renaming the cap's own comment names as the reason it exists — a child cycling executable
names to keep every day looking new. That child silently and permanently disables the feature, and
produces **exactly** the same dashboard and exactly the same audit log as a fresh install where the
watcher has never run.

**Fix.** At minimum record it (`audit.record("first_seen_baseline_overflow", …)`) so the one
interesting case leaves a trace. Properly, it is a distinct state rather than a third spelling of
`None`, and it should reach the parent as "this check has stopped working, and here is why".

**Trigger.** Whenever O52 is taken,
since both are about the same `Option` losing information on its way to the reader.

### O51 · The property the audit change exists for is proved for a type and never for the endpoint

`LiveViewAudit` has four unit tests covering the coalescer in isolation. `tests/api.rs` has three
tests hitting `/api/screenshot`. Neither set covers the seam: **no test asserts that a preview
request produces at most one `live_view` line per window, or that a full request produces a
`screenshot_taken` line.**

The mapping from tier to audit behaviour lives in an `if let` in `api::screenshot`, while the
35 lines of reasoning explaining it live in `audit.rs`. Feeding a `Full` frame to `observe`, or
dropping the `if let` — there is no `#[must_use]` — loses the count with every test still green.

There is a concrete obstacle worth recording, because it is why this was not simply written:
`tests/common` builds state with `AuditLog::disabled()`, so the integration harness has no audit log
to assert against. Closing this means either a test-only constructor that writes to a temp dir, or
moving the mapping into a method (`record_capture(&audit, tier, now)`) that can be unit-tested with
the reasoning beside it.

**Fix.** Prefer the method: it puts the tier→event decision next to the argument for it, and makes
`observe` stop being a public entry point that accepts a frame it cannot classify.

### O52 · `first_seen`'s three states collapse to two at the last hop

`Option<FirstSeen>` carries three states deliberately: `None` = the report could not tell,
`Some` with an empty `apps` = it checked and nothing was new, `Some` non-empty = the notice. That
distinction is typed in Rust, serialized, mirrored in `emptyScreentime()`, argued for in a doc
comment that says "the UI must distinguish that", and pinned by four tests across two languages.

The UI does not distinguish it. `x-if="showFirstSeen"` requires `apps.length > 0`, so `None` and
`Some{apps:[]}` render identically — nothing at all. `firstSeenNote()` computes a sentence for the
quiet day and throws it away.

To a parent, "checked, nothing new" and "gave up" are the same blank space, which is the failure the
`Option` was introduced to prevent, arriving one layer past where it is guarded. Meanwhile two JS
tests maintain an internal difference with no observable consequence.

**Fix, and it is genuinely a choice.** Either the quiet-day state reaches the reader — one line where
the panel would be, and `firstSeenNote()` already renders it — or `first_seen_in` returns `None` for
it and the doc, the type's contract and four tests drop to two states. What is not defensible is
carrying three states everywhere and rendering two.

Weigh it against the reason the panel is hidden on quiet days at all: a notice that appears every day
stops being read. That argument is sound and may well win — in which case the honest move is to
*delete* the third state rather than keep paying for it.

### O54 · Two source-text scanners and a standing exemption, for a property a shared list would make true

`run_helper` reads `--tier` with a hand-rolled `args.iter().position(...)` scan, and its two usage
strings are hand-maintained literals. Keeping those in step currently costs: an `in_run_helper` skip
inside `every_flag_the_code_reads_is_listed_in_the_table` — a **standing exemption in the scanner
that polices every other flag in the codebase** — plus a second test that slices `run_helper`'s body
out of `src/lib.rs` with `split_once("\nfn run_helper(")` / `split_once("\n}\n")`, greps two string
idioms out of it, and reassembles wrapped `eprintln!` continuation lines. That test carries a
`flags.len() >= 4` self-check because, in its own words, "the scan drifted and proves nothing", and
its comment records that an earlier version already reported two documented flags as undocumented.

This is **not** the sanctioned source-scanning pattern. `the_capture_backend_is_named_not_defaulted`
and `the_css_build_chain_stamps_only_after_a_successful_compile` scan text because the fact lives in
`Cargo.toml` and `package.json`, where Rust cannot reach it. "Every flag I dispatch on appears in the
message I print" is expressible in Rust.

**Fix.** One `const HELPER_FLAGS: &[(&str, &str)]` that `run_helper` both matches against and renders
its usage from. The property becomes true by construction; both text scanners and the exemption go
away with it.

**Not done here, and the reason is the interesting part.** This is argument dispatch on the capture
helper — a path that has **never run on Windows hardware**. Rewriting it immediately before the
on-device verification pass would mean that when something misbehaves on the machine, there is no way
to tell the capture work from the refactor. Same reasoning as O2's revised trigger.

**Trigger.** After `docs/WINDOWS-TESTING.md` has been run on the device.

### O56 · The screen-time report pays for every day ever recorded, not the window asked for

`build_report` parses the whole retained history, then does work on rows it will never render:

- `DayRow::measured` **deep-clones** four vectors (`apps`, `focused`, `pages`, `groups`) out of
  `by_date`, a map local to `build_report` that is dropped moments later. Bounded by `MAX_APPS` 200 +
  200 + `MAX_PAGES` 40 + groups, so up to ~445 `String` allocations per measured day — up to ~40,000
  per request at the 90-day window, realistically a few thousand.
- Out-of-window rows are still four-way sorted by `app_minutes`, although their only consumer is
  `first_seen_in`, which reads **only** `focused`'s names — not the minutes, not the order, and none
  of `apps`/`pages`/`groups`. A year installed with a 30-day window is ~335 such rows, ~17,000 wasted
  `String` allocations and ~1,300 wasted sorts per request.

Cost scales with **how long the tool has been installed**, not with the window requested — the same
shape as the `usage.jsonl` waste already fixed one level up.

**Fix.** Run `window_total` and `first_seen_in` before the day loop (both only read), then build
`day_rows` with `by_date.remove(&cursor)` and move the vectors in; and move the sort into
`DayRow::measured`, the only consumer that needs an order.

**Why it is here and not done.** Measured at sub-millisecond to ~2 ms on a request a parent triggers
by hand, and — the deciding fact — **it predates the reviewed diff**, so it falls outside "wasted work
this change introduces". Mechanical and safe when someone is next in that file for another reason.

### O63 · The comment stripper cannot parse a regex literal containing a quote

`stripJs`'s doc comment says regex literals are safe "by construction — a `/` is only treated as
opening a comment when the very next character is `/` or `*`". That is true, and it is only half the
question. The scanner also tracks string delimiters, and it does not know it is inside a regex — so
`/["']/` opens a string at the `"` and consumes everything after it looking for a close.

Confirmed by running it: `stripJs('const re = /["\']/;\nconst a = 1;\n')` reports `unterminated`.

**This is now loud rather than silent, which is the improvement, not the finding.** Before the guard
swapped to an unterminated-string trigger, such a file was mis-stripped quietly and only failed the
build if the damage happened to exceed a byte ratio. It now fails immediately, and the message names
a regex literal as the likely cause. No file in `assets/` contains one today — the build passes —
so nothing is broken.

**What is left is that a legitimate expression cannot be written.** A first-party `assets/*.js` that
needs to match a quote character has no way to do it, and the failure arrives as a build error about
strings rather than about regexes.

**Fix.** Track regex literals in the scanner, which means distinguishing a regex from division —
the classic JavaScript lexing ambiguity, resolved by looking at the previous significant token. That
is real work for a build script. The cheaper alternative is a character class escape (`/[\x22\x27]/`
parses fine today) documented at the call site, which costs one comment and no scanner change.

**Trigger.** The first time someone needs a quote inside a regex in `assets/*.js`. Not before —
the workaround is a footnote and the proper fix is a lexer.

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

### O65 · The audit's file append runs on the async runtime, not the blocking pool

`state.audit.record(...)` is the only file-touching call in `api.rs` that does not go through
`blocking` or `spawn`, so it appends on a tokio reactor thread. Per line `jsonl.rs::append_line`
does a `metadata`, an `OpenOptions::open`, a `writeln!` (two `WriteFile`s, since there is no
`BufWriter`) and a close — about seven syscalls, against an ACL-hardened `ProgramData` directory
that Defender is scanning in real time.

`redeem_code` already does the right thing by putting its file work on `spawn`; this is the outlier.
Note the frequency is **not** the problem and the live-view coalescer is what keeps it that way —
this is about which thread pays, not how often.

**Fix.** Wrap the record in `spawn`, or hold a buffered handle. One line. Not introduced by the
capture work; found while tracing it.

### O66 · A full frame is sized by the overlay being open, not by what the overlay can show

`liveTier()` returns `full` whenever `shotFull` is set, and full means the native capture — 3840×2160
on a 4K panel. The overlay renders it `object-contain` inside `inset-0`. On the parent's phone, which
is the device the pairing QR exists for, that box resolves roughly 1170×658 of the frame: about a
tenth of the pixels the child's PC captured, encoded, piped, encrypted and uploaded. On a 2560×1440
desktop browser the overhang is closer to 2.4×, so this is phone-shaped.

**Fix, and it is a decision rather than a tidy-up.** The overlay could send its own device-pixel box
(`?tier=full&w=…&h=…`, clamped to native) and let `encode_shot` fit to it; the machinery exists and
`a_small_frame_is_never_scaled_up` already pins the never-upscale rule. Against that,
`ShotTier`'s doc argues deliberately for one variant and one code path so the full path cannot rot,
and a third size axis is exactly what it was written against. Weigh those before touching it.

### O67 · Redeeming a code parses the whole code log, and the throttle is now the only defence

`redeem` → `active()` → `JsonlLog::recent(usize::MAX)` reads `time_codes.jsonl` whole, runs
`serde_json::from_str` on every line ever written, collects a `Vec<Value>`, reverses it and builds a
`BTreeSet` of every code seen. A **wrong** guess pays that in full, and the cost grows with install
age since the file only rotates at 2 MiB.

That was affordable while eight characters carried the security. Shortening codes to six moved the
defence onto the rate limiter — `redeem_code`'s own doc now says "that rate limit is the primary
defence, not a secondary one" — which makes sustained wrong guesses the *expected* steady state
rather than an anomaly: five a minute per IP, from as many LAN addresses as the household has.

**Fix, already in-house.** `jsonl.rs::read_events_matching` documents this exact pattern — a
`line.contains(…)` reject filter before `from_str`, with the authoritative check after the parse.
`redeem` only asks "is *this* code active?", so pre-filtering turns a parsed `Value` tree per line
into a substring scan. Mitigating meanwhile: it runs on the blocking pool, so it burns a pool thread
and the disk rather than stalling the reactor.

### O68 · An aborted live frame is captured on the child's PC and never counted

When a click supersedes a timer frame, the handler future is dropped at the `await` in `blocking`,
which is *before* `screenshot` reaches `live_audit.observe(...)`. `spawn_blocking` cannot be
cancelled, so the capture still happens — the child's machine spawns the helper, captures and
encodes — but the frame is never added to the `live_view` count.

So `frames` means *frames delivered*, not *frames captured*. For a log whose question is "was this
child's screen watched, and for how long", delivered is arguably the right measure, and the
undercount is bounded by how often a parent clicks during a live session. It is recorded because
the field's name does not say which of the two it means, and `SECURITY.md` describes it as "the
number of frames it stands for".

**Fix.** Either rename the field to say `delivered`, or say plainly in `LiveViewAudit`'s doc which
one it counts. No behaviour change either way — this is about a name being read as the other thing.

### O69 · A 401 during a capture orphans the frame's blob URL

`takeScreenshot`'s 401 branch sets `authed = false`, stops the live view and returns — without
revoking `shotUrl`. Every other exit revokes: supersede, replace, and logout. Pre-existing, and the
session ends anyway so the leak is bounded by the tab's life.

What changed is the size. While the full-size view is open the orphaned frame is now a megabyte-scale
blob rather than a ~25 KiB preview.

**Fix.** Revoke on the 401 path, or route it through `resetSessionData`, which already does.

