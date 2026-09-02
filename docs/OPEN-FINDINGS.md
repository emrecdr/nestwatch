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
each other. `O72` is the first pair. That makes this a channel, and a channel needs an address rather
than a sentence.

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

Last audited against the tree on **2026-08-31**. Entries that did not survive that audit were removed
or rewritten rather than annotated, per the rules above.

## Release state

**`v0.5.0`, published 2026-08-31.** Everything below is open against a release that is on the
download page, not against unreleased work — which is what makes the list worth keeping honest
rather than tidy.

What that release was verified by: unit and integration tests, `cargo test --all-targets --locked`
and `clippy -D warnings` on Linux and on a `windows-latest` runner, cross-compilation to
`x86_64-pc-windows-gnu`, and a published SBOM plus binary attestation that were both checked against
the downloaded artifacts.

What it was **not** verified by: running on the machine it is for. The 32 items in section H of
[WINDOWS-TESTING.md](WINDOWS-TESTING.md) cover everything headline in 0.5.0 — the bedtime extension,
the enforcer wake, the translated shutdown notices and the ask link — and none of them has executed
on Windows. The three gates that were green when it shipped are the same three that were green when
`install` failed on real hardware and again when `remove_file` turned out not to be exclusive. That
is not an argument for distrusting them; it is the reason the section below exists and the reason
the checklist is the only method here with a track record.

The enforcer wake is the one to run first. Its entire value is a timing property — an abort arriving
in well under a second where it previously took up to 30 — measured once, on macOS, where `shutdown`
is a no-op. On Windows it is a real `shutdown.exe` with a real pending timer, and whether the abort
beats a 60-second countdown there is unknown.

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

**The per-account half of this entry is unchanged and still open — and it is larger than this entry
says.** Everything above concerns *attribution in the report*. There is a second half, not tracked
anywhere until now: **enforcement has no person in it either.** `Config` holds one `Rules`, one
`Curfew` and one `DailyGrant`, and `Rules` holds one `daily_budget_mins`, one `blocklist` and one
`app_limits` map. A sweep of `src/` for `per_user|username|child_id|per_child` returns three hits,
all unrelated. So two children on one PC share a budget and the first to use it spends the second's
evening, and a parent doing the taxes on that machine draws down their child's screen time.

The reporting half is a `SystemControl` change. The enforcement half is a config-schema change with
migration, touching both enforcers, the API surface and the whole dashboard — a different order of
work, and it should not be smuggled in behind the smaller one.

**Documented rather than scheduled, 2026-08-31.** The README's *Not included* list now states "one
managed child per PC" as a boundary, because the failure was quiet: the budget is not wrong, it is
measuring something other than what the parent assumed, and it looks exactly like a child who used
more than they admit. Stating it is not the same as fixing it, and this entry stays open. Note the
knock-on if it is ever taken: `DECLINED-OPTIONS.md` names "a row per `(day, app)`" as the trigger
that would make SQLite defensible, and per-account multiplies rows by the same argument.

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

### O67 · There is no retention policy — only rotation, and rotation deletes

`jsonl.rs::append_line` renames the live file to `.jsonl.1` once it passes `MAX_BYTES` (2 MiB) and
**clobbers any existing `.1`**. Each log therefore keeps two generations and nothing else. There is
no prune, no configurable retention, and no notice: the oldest days are deleted at rotation time,
silently, long before anyone runs `--purge`.

This is a design decision rather than a defect, which is why it is recorded rather than fixed — how
much of a child's history a parental tool should keep is a question for a household, and erring
toward keeping less is defensible on its own terms. What makes it worth tracking is that **nobody is
told**. A parent reading a 90-day report has no way to know the tool will never show them a year.

**The 4 MiB across two generations is the fact; the horizon is a model.** A daily row's size is set
by how many apps, pages and groups were used, so it is a property of the household, not the product.
Two independent estimates, differing only in assumed name lengths, agreed on shape and differed by
~40%: decades of light use, and roughly **two to three years** for a child at `foreground::MAX_PAGES`
every day. Quote it as a range or not at all — "7.5 years" reads as a guarantee.

`screentime.rs`'s log holds nothing but rollups, so it is the slow one. `usage.jsonl` carries them
among session starts, stops, locks, warnings and grants, so it rotates far sooner and its copy of a
given day dies first — but that only affects installs predating `screentime.jsonl`, and
`screentime::history_rows` reads both for exactly that reason.

**The third option is done.** `Report::history_from` carries the oldest completed day still on
disk, and the report card renders it as *History from 2026-07-01* beside *Measured days 25/30*. It
is read from `by_date` — the whole retained history — rather than from the window, and
`the_oldest_day_held_does_not_move_when_the_window_narrows` pins that: derived from `day_rows` it
would compile, pass a single-window test, and then report a different horizon for each of the
7/30/90 buttons. Shown unconditionally rather than only once history is shorter than the range
asked for, because surfacing it only when it has already bitten leaves exactly the parent who has
not hit it yet uninformed, which is the same silence written differently.

**The loss is now recorded as well as previewed.** *History from …* is derived from what
**survived**, so it answers "how far back can I see" and cannot distinguish a fresh install from
one that quietly dropped a year. `jsonl::rotate_if_over_size` now writes a `rotated` row into the
newly-emptied file whenever a rotation actually destroys a previous backup, carrying
`discarded_bytes` and `discarded_through` — the newest timestamp in the backup being clobbered,
i.e. the instant before which history no longer exists. It travels into `GET /api/export`, so a
parent checking the tool against itself sees the gap rather than having to notice an absence.

Deliberately not a full read of the doomed file: that would be up to 2 MiB on whichever thread
called `record`, and for the audit log that is an axum handler on the async runtime. A `stat` for
the size and an 8 KiB tail read for the last timestamp cost one short read per 2 MiB written. The
line **count** is what that trade gives up. A *first* rotation destroys nothing and writes nothing,
because a row reporting that no data was lost is noise in a file a parent reads.

**What remains open is the deletion itself, which is untouched.** Rotation still keeps two
generations and still clobbers the older one; nothing prunes and nothing is configurable. The two
costed-at-nothing options stand: a rollup-only prune that keeps N days regardless of bytes, or a
larger `MAX_BYTES` for `screentime.jsonl` specifically, since its rows are the irreplaceable ones.
What has changed is that the loss is now both visible in advance and recorded after the fact, so
this is no longer silent — only unbounded.

**Trigger.** Any decision to advertise a retention period, or the first parent who watches
*History from* move forward and asks why.

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

### O69 · Nothing detects a doc comment that has drifted onto the wrong item

Inserting an item directly above an existing one silently transfers the doc comment: the new item
adopts the block, and the item that owned it is left undocumented. Neither `cargo test` nor
`clippy -D warnings` nor the cross-compile has ever failed on it. It has happened **five times** in
this codebase.

All five are fixed. `screentime::totals_across` and two others went in `0eb0bc4`; `api::notify` was
caused and fixed the same day; `lib::run_cli` went in `1c6fe7a` — that one had been stranded since
the root commit `e760aa4`, so the CLI entry point carried no doc in **any** revision of this repo
until 2026-08-27.

**No live instance is known.** Scanned against `bef3a59` on 2026-08-27: two undocumented
module-level `pub fn` (`config::data_paths`, `install::install`) and thirty across all visibilities.
Each of the two was read and neither is a victim — `install` is described in `install.rs`'s own `//!`
header, and `data_paths` sits below `DataPaths`'s own doc. So this entry is not a live defect; it is
the absence of anything that would catch the sixth.

**The one time a gate did catch it, it was an accident of formatting.** `api::notify`'s stolen block
happened to end in a `*` list, which tripped `doc_lazy_continuation`. A block ending in prose splices
in total silence. Four of the five were found by a person reading.

**Three detector designs were tried on 2026-08-27 and all three failed.** Recorded because each is
the obvious next proposal:

- *Flag any doc block naming an item defined later in the file.* Far too noisy at every setting:
  **between 7% and 24% of blocks flagged**, depending on three choices the rule as stated does not
  specify — whether nested items (methods, `#[test]` fns) count as "defined later", whether the name
  must be backticked or matches as bare text, and whether indented doc blocks are in the denominator.
  Two independently written scanners in the same pass each quoted a precise figure and neither
  reproduced, until the variants were pinned down; they then agreed exactly. One reproducible point,
  measured 2026-08-27 at `6e75a85` over `src/*.rs`, counting module-level doc blocks and module-level
  item names only: **46 of 368 (12%)** matching on a word boundary, **66 of 368 (18%)** matching as a
  bare substring. The cause is not a tuning problem: this codebase cross-references forward on
  purpose — `rules.rs` alone accounts for around 17, mostly docs pointing at `decide` — so a forward
  reference is the norm rather than the signal, and no threshold separates them.
- *Flag any undocumented `fn` whose own name appears in a doc block above it.* Exactly one hit,
  `lib.rs::accepts`, and it is a false positive: `struct Accepts`'s doc legitimately contains the
  word "accepts". It does **not** flag `run_cli`, the instance it was written to catch.
- *Restrict either scan to `pub fn`.* Adoptable — two doc comments — but inert on the motivating
  cases, because `api::notify` and `screentime::totals_across` are both private.

**Why no lexical rule reaches it.** The stranded sentence never names its subject. "Parse `argv` and
dispatch the requested subcommand" contains no `run_cli`, backticked or plain. The defect is that a
doc's *content* describes a different item — a semantic property, not a token one. Adding a
backtick requirement to the second design makes it strictly worse: it takes the one hit to zero,
deleting the pointer that led to the find.

**A structural trap, recorded because it cost two review sessions a wrong conclusion.** The orphaned
doc never sits above the victim. It stays attached to whatever item displaced it — 68 lines earlier
and on a different item, in `run_cli`'s case. Checking directly above an undocumented function is
looking exactly where the displaced doc cannot be.

**It has a deletion form, and that one is harder to see.** Removing part of what a doc describes
leaves prose that is well-formed, attached to the correct item, and quietly false. On 2026-08-27 a
`/simplify` pass removed the toast-pinning half of a loop in `src/web.rs` and left the enclosing
test's doc still reading *"Four surfaces restate these, none of them near the enforcement: two
`max=` attributes and two toast messages"* — two of the four no longer pinned there. It was caught
in the same pass by re-reading the doc after the deletion, and repaired in the same change.

The insertion form at least produces an absence: some item ends up visibly undocumented, and a list
of undocumented items can surface it. The deletion form produces no absence at all — every item
still has a doc, every doc still sits on its own item, and only the *content* has gone stale. So the
mitigation below does not reach it, and neither does any of the three scanners above, which all key
on a name appearing or failing to appear. Recorded as one observed instance, separate from the five
insertions counted above.

**Fix.** None proposed, and a low-precision scanner would be worse than none — a check people learn
to ignore is its own failure. The nearest useful thing is not a detector: keep the undocumented
module-level `pub fn` list at zero (it is two entries away) so that anything appearing on it is
short enough to be read, since reading is what found every instance. That is a guard on list length,
not on the defect.

### O70 · The enforcer loops are driven down one path each, never across a scripted day

The coverage run that opened this entry (`cargo llvm-cov`, 2026-08-27 at `7091c84`, host target)
put the crate at **84.03%** of lines. The number was not the finding; where the misses sat was:

| file | lines uncovered | what they are |
|---|---|---|
| `rules.rs` | 251 of 1,471 | almost all of `run_rules_enforcer` |
| `curfew.rs` | 72 of 579 | almost all of `run_enforcer` |
| `helper.rs` | 29 of 32 (**0%**) | the whole file — capture/lock/watch, all Windows shell-outs |
| `clock.rs` | 6 of 224 | effectively none |

**The headline this entry used to carry — that no test executes the loops — is retired.** Three
now do, all spawning the real driver against a `FakeControl`: `tests/enforcer_loop.rs` asserts what
a paused tick and an unconfigured one write to the tally, `tests/enforcer_shutdown.rs` runs the
budget out to its shutdown and asserts the bytes the child would really see, and
`tests/curfew_enforcer.rs` does the same for `run_enforcer` at bedtime.

**What survives is the shape of the coverage rather than its absence.** Each of those drives one
path to one ending. Nothing asserts a *sequence*: the 15/5/1 warnings in order and each fired once,
then the kill or lock or shutdown, with the tally write and the audit rows that belong beside them,
over a simulated day. Ordering is where this project's real defects have lived — the dropped
`enabled` guard and the Win+L bypass were both wiring — and a single-path test cannot see a warning
that fires twice, one that fires after the ending, or an audit row written for a notice that never
reached the child.

**Not a coverage gate** — see the row in [DECLINED-OPTIONS.md](DECLINED-OPTIONS.md), whose
re-raise trigger this entry has now tripped. The remaining work is `tokio::time` pause/advance over
a scripted config, asserting an ordered list of control calls.

**The harness is now whole.** `FakeControl` records every `shutdown` as `(delay_secs, message)` in
order, bounded at 64, and — since the app-stopped work — every `notify_user` as `(title, body)`,
bounded at 128, with `notification_bodies()` for the common assertion. This entry used to end by
naming that gap: *"the warnings — the half of the sequence that carries the ordering risk — cannot
be asserted on at all until it records too."* They can now, and
`tests/enforcer_app_stopped.rs` is the first test to do it. What remains is only the scripted
*sequence*; nothing is missing from the fake any more.

**helper.rs is untouched by any of the above** and stands at 0%: capture, lock and watch are
Windows shell-outs that no host test reaches. That row of the original measurement is unchanged,
and it is not the row a driver test can move.

**Already banked from the original run.** Three child-facing strings were reachable only from these
loops and so had never been asserted at all — `lock_warning_message`, `limit_reached_message` and
`bedtime_title`, each with a Dutch translation that nothing checked. Fixed, with `Language::ALL`
added so the tests cannot go stale when a third language lands.

### O71 · The dashboard is one Alpine component, and the usual argument for splitting it is wrong

`assets/app.js` is **2,056 lines** registering a single `Alpine.data("app", app)` with ~97
methods, consumed by two `x-data="app"` roots across 1,450 lines of markup. By comparison `src/`
is 39 modules with a stated responsibility each.

**The premise most reviews attach to this is false, and it was false when they wrote it.** The
argument arrives as "a component this size cannot be tested without a browser, so split it to
make it testable". `web/test/harness.js` has evaluated `app.js` in a `vm` context since
`4434447`, and `web/test/app.test.js` is **1,910 lines** exercising its pure methods. Reachability
was never the problem and splitting would not improve it. Anyone re-raising this should check the
harness before repeating the testability argument.

**What is actually left.** One scope holding capture, process list, curfew editing, rules editing,
routines, time codes, the audit feed, the screen-time report, theming, toasts and the update
check. The cost is coupling, not reach: two rows in `DECLINED-OPTIONS.md` correctly refused
front-end duplication fixes as churn, and they are churn *because* any local fix in a shared scope
looks disproportionate. The genuinely untested half is the DOM half, which a split does not reach
either.

**If it is ever done**, the mechanism is available and cheap: the CSP build already requires
`Alpine.data()` registration — that is why `x-data="app"` works at all — so additional
registrations need no change in CSP posture. Seams follow the cards: *screen*, *report*, *rules*,
*access*. Extract one, prove it, and leave the rest until a change lands there anyway.

---

### O72 · `RENEW_WARN_DAYS` is client-visible, and only a source grep can reach it

> **Cross-repo** · filed by `nestwatch-mobile` · pairs with `nestwatch-mobile#M6`

`tests/golden.rs::limits` exists because a client renders some numbers *before* it can ask, and its
own doc comment records why the alternative was retired: the client used to grep this repository's
source for those constants, "the failure mode is that the check stops running rather than that a
number is wrong", and "it broke within hours of being written, when these constants were named
instead of left inline".

`cert::RENEW_WARN_DAYS` is now in exactly that position and is not in `limits.json`.

**Why it became client-visible.** The Android client pins this PC's certificate, and the pin is the
sole authority — its `badCertificateCallback` returns true on a fingerprint match and nothing further
is consulted, including the validity dates. **Measured on 2026-08-27**, in that repo's
`test/expiry_test.dart`, against a TLS server presenting a certificate whose `notAfter` was
2024-01-01: the pinned client completes the request normally. A browser hard-fails on the same
certificate.

So an expired certificate produces a *working phone and a broken dashboard*, and every instinct sends
the parent looking at the PC rather than at a lapsed certificate. The phone is the only client still
working, so it is the only one placed to explain — which means it warns, which means it needs a
threshold.

`RENEW_WARN_DAYS` is already `pub` for this same reason on this side. Its comment says it is public
so that `doctor` "nags at the same threshold as the service log", because "two different answers to
'is this cert about to lapse?' would have the parent reading a diagnostic that contradicts the
warning in their log file". A phone carrying its own number is a third answer to that question.

With no published channel, the client now reads `pub const RENEW_WARN_DAYS: u64 = <n>;` out of
`src/cert.rs` with a `sed` in its own `tool/check_golden.sh`. That reader does have an explicit
`UNREADABLE` branch, and both of its failure modes — a changed value, and a renamed constant — were
watched to fire before it was trusted. It is still the retired channel, re-opened, and the previous
one also looked fine right up until the day it stopped reading anything.

**Fix.** One line each in `tests/golden.rs`:

```rust
use nestwatch::cert::RENEW_WARN_DAYS;
// ...in fn limits(), alongside the other five:
"renew_warn_days": RENEW_WARN_DAYS,
```

Checked rather than assumed: `src/lib.rs` already declares `pub mod cert`, and `RENEW_WARN_DAYS` is
already `pub`, so this needs no visibility change. The client then vendors the enlarged `limits.json`
like the other golden files, asserts it in its own suite, and deletes its `sed`.

**The argument is consolidation, not automation, and an earlier draft of this entry said otherwise.**
It claimed the client asserts the vendored copy "on every commit". It does not, and could not:
measured in that repository on 2026-08-27, there is no CI of any kind — no `.github/`, no runner
config of any flavour — and `.git/hooks/` contains nothing but the shipped `.sample` files. So
`flutter test` there is exactly as manual as `tool/check_golden.sh` is. What this fix buys is not a
manual gate replaced by an automatic one; it is one bespoke reader of this repository's source
replaced by a gate that already covers five other values and is already run for them.

**`VALIDITY_DAYS` is deliberately not part of this.** It is private, and the client does not need it:
the handshake hands it the certificate's actual `notAfter`, which is better than a constant because
it describes the certificate in front of it rather than the one this version would issue.

**Cost of leaving it.** One bespoke reader of this repository's Rust, in a repository that cannot run
this repository's tests, for a number a parent reads.

Not, however, a *silent* one — and the first draft of this entry closed by saying it was, two
paragraphs after saying the opposite. The `sed` shouts `UNREADABLE` when it cannot read, which is the
lesson of the previous grep applied. What is wrong with it is narrower and worth stating exactly: it
is a bespoke reader of another repository's source, which is the category that has already failed
here once, and it only speaks when somebody runs it.

**What the fix does not cover.** Publishing the constant moves it from a `sed` to a golden file, and
that is better — but the two copies would then be pinned in opposite repositories with no shared
gate. This repository's CI never runs the client's suite; the client's never runs this one. Drift is
caught by `tool/check_golden.sh` over there, which needs both checkouts on one machine and a person
choosing to run it. That mechanism does work — it is what reported `limits.json` arriving here before
the client consumed it — but it is a manual gate, and "fix" should not be read as "closed". The
retired channel failed by going blind; a cross-repo golden fails by drifting, which is quieter still.

*(That last paragraph came from a review by a concurrent session in this repository, which verified
the one-line fix independently and pointed out the gap. Recorded because the entry is better for it.)*

### O73 · One CSS rule is all that keeps `unsafe-inline` in `style-src`

`security.rs` gives two reasons `style-src` still admits `'unsafe-inline'`: the `[x-cloak]` rule is
an inline `<style>`, and Alpine writes `style` attributes for `x-show` and `:style`. Only the second
is structural. The first is a single declaration — `[x-cloak] { display: none !important; }` at the
end of `index.html`, the only `<style>` element on either served page.

Moved into `web/src/app.css` it compiles into `assets/app.css`, which loads from `<head>` and so
applies *earlier* than a `<style>` at the end of `<body>` — the anti-flash behaviour improves rather
than degrades. With no inline stylesheet left, CSP Level 3 separates the two cases:

```
style-src 'self'; style-src-attr 'unsafe-inline';
```

Alpine keeps its attributes; an injected `<style>` element stops being executable. That is a real
narrowing on the one page where injected content would land.

**Why it is recorded rather than done, and the caution is the entry.** A browser that does not
implement `style-src-attr` ignores it and falls back to `style-src 'self'` — which forbids Alpine's
attributes and therefore breaks every `x-show` on the page. The failure mode is not a missing
style: it is the whole dashboard rendering with every conditional element visible at once, including
the ones `x-cloak` exists to hide. Firefox only shipped the directive in 128. This is precisely the
class the Alpine CSP-build comment already warns about, where "looks equivalent" and "is equivalent"
diverge without an error.

**Trigger.** Confirm the browsers this household actually uses implement `style-src-attr`, then move
the rule and split the directive in one change — and look at the page afterwards, because no test
here can see a CSP the browser enforces.

### O74 · A bedtime extension can be granted but never taken back

**Later bedtime tonight** offers +15/+30/+60 and nothing else. There is no way to shorten an
extension, and no way to cancel one. A parent who meant +15 and hit +60 has no route back:

* Saving the curfew form deliberately preserves `extra_until` — that is the fix for a real defect
  (`tests/curfew_extend.rs`, phase 3), so the obvious escape hatch is the one thing guaranteed not
  to work.
* Switching curfew off and on again preserves it too, for the same reason.
* Nothing else on the dashboard touches the field. Only editing `config.json` by hand clears it,
  which requires Administrator and is not something a parent at 22:00 will do.

So the control is one-way by construction, and its irreversibility is a *consequence* of the fix
next to it rather than a decision anyone made.

**Why this is worth an entry rather than a quick button.** Current guidance on destructive or
hard-to-reverse actions measures them on reversibility, frequency and complexity, and prefers an
**undo** over a confirmation dialog wherever the action can be reversed: a confirm on every press
would tax the common case (the parent meant it) to protect the rare one. That argues for a way out
after the fact, not a speed bump before it — most likely a "Back to normal" control that appears
only while an extension is running and clears `extra_until`.

**The part that needs deciding, and is why this is not just written.** Clearing the field is not
symmetric with granting it. An extension that has already been *announced* has, in effect, been
promised to the child — they were not shut down at 22:00 and have arranged the rest of their evening
around it. Revoking silently at 22:20 hands the child the same experience this whole feature exists
to prevent, only with the parent as the cause. Options worth weighing:

* **Clear outright**, and accept that bedtime can arrive early. Simplest, and the honest reading of
  "undo" — but the child loses the *advance* notice, not merely some of it: once `extra_until` goes,
  `is_active_at` is true immediately, so `mins_until_active` returns `None` and the "bedtime in N
  minutes" popups never fire. They still get the shutdown's own `warn_secs` grace (60s by default),
  which is the Windows dialog rather than a heads-up.
* **Clear, but never sooner than `LOOKAHEAD_MINS`**, so the child always gets the same "bedtime in
  N minutes" warning they would have had. Kinder, slightly more code, and it makes an undo that is
  not quite an undo.
* **Offer −15/−30 rather than a clear**, mirroring the grant. Reversible in the same units it was
  granted in, and it cannot land the child in an immediate shutdown unless the parent walks it there.

Not urgent: the failure is a parent granting more time than they meant to, which is the benign
direction. Filed so the asymmetry is a choice next time rather than an oversight.

### O75 · Pure helpers are tested thoroughly; the lines that call them are not tested at all

Five instances in two days, across two sessions working this repository concurrently. Each one is
the same shape, and in each the deletion was **measured**, not argued:

| what was removed | suite afterwards | what the family would have lost |
|---|---|---|
| `api.rs:228`, carrying `extra_until` across a curfew save | **497 pass** | a granted bedtime extension, silently revoked by saving the form |
| `install.rs`'s call to `web::ask_url` | **497 pass** | the child's link reverts to a LAN IP that moves on a new lease |
| `install.rs`'s call to `alternate_note` | **504 pass** | both printed addresses labelled the same; the durable one unmarked |
| `curfew.rs:444`'s call to `bedtime_shutdown_message` | **507 pass** | the bedtime notice reduced to the single word "Bedtijd" |
| `app.js`'s `noteOtherLimit(j)` in `extendCurfew` | **148 web pass** | the parent is not told screen time will still lock the PC |

**The mechanism is structural, not carelessness.** A pure function is trivial to call from a test,
so it attracts thorough tests — `alternate_note` has four, `bedtime_shutdown_message` has one per
language. The single line that *calls* it sits inside an enforcer loop, an install routine or an
async handler, none of which a unit test can reach. So the suite grows precisely where testing is
cheap, and the hole forms precisely where a silent revert does the most damage. Coverage cannot see
this: the call site is *executed* by nothing, and even when it is, executing a line is not asserting
on it.

The fourth entry is the sharpest evidence that this is structural. It was introduced **in the commit
that was fixing the same class elsewhere** — a driver test was written for the rules enforcer and
its sibling for curfew was not, so a helper added that hour was already unguarded at its only call
site.

**What has been done.** Each of the five now has a guard, and each guard was proven by re-applying
the mutation: `tests/curfew_extend.rs`, `install.rs`'s two source scans,
`tests/curfew_enforcer.rs`, and three call-site tests in `web/test/app.test.js`.

**What has not.** Nothing finds the *next* one. Every guard above was written after a human or an
agent went looking, and the searching was manual each time.

**The obvious answer, and the reason it is not simply done.** `cargo-mutants` is built for exactly
this — it injects mutations and reports which survive, which is the question asked five times by
hand above — and it is on the Thoughtworks Technology Radar rather than being exotic. It is already
installed on this machine and another project here runs it. Two things to weigh before adopting it:

* **Cost.** Each mutant is an incremental build plus a test run. This crate's baseline test run is
  already seconds and the mutant count would be in the hundreds. The practical shapes are
  `--in-diff` on pull requests, or a scheduled full run, not a per-push gate.
* **Disk, concretely.** A single `cargo-mutants` run on a *sibling* project filled this machine's
  shared `~/.cache/cargo-target` to 100% while this entry was being written, which is exactly the
  hazard that makes builds fail in crates nobody touched. Whatever is adopted has to bound where it
  builds, or it takes the whole machine down with it.

A cheaper interim measure, and the one already in use: when adding a helper, mutate its call site
once by hand before believing the tests. Four of the five above were found that way in an afternoon.

### O76 · The enforcement wake fires on every config write, including the ones that change nothing

`api::try_update_config` calls `heartbeat::wake` after every save. That choke point is deliberate and
the reasoning still holds — it is the one place config is mutated, so no future handler has to
remember to wake — but it does not distinguish a write that can invalidate a pending shutdown from
one that cannot. `set_language`, `change_password`, `save_routine` and `delete_routine` all wake both
enforcers.

**What an out-of-cadence wake costs**, from `run_rules_enforcer`:

- a `session_state()` call on the blocking pool;
- on any install with a blocklist, a per-app limit or a group — i.e. `TickMode::Enforce` — a full
  `running_processes()` enumeration, also on the blocking pool. This is precisely the scan the
  `has_targets` gate above it exists to skip, and the pool it shares with screenshots;
- a `Usage::to_json()` of the whole tally for the `save_tally_if_changed` comparison;
- a curfew-loop pass alongside it.

**Human-paced, so this is waste rather than a hazard.** A parent working through the settings page
buys a handful of extra enumerations inside a 30-second window. Nothing accrues wrongly: the loop
derives elapsed time from `duration_since(last_tick)` rather than counting ticks, which is the
property that makes an early wake safe in the first place.

**Why it is filed rather than fixed.** The obvious repair is to wake only when the enforcement-
relevant slice actually changed — `(curfew, rules, extra.for_day(today))` before versus after
`mutate` — which keeps the choke point and its "nothing to remember" property intact. Two things
make that more than a tidy-up. `Config`, `Rules` and `Curfew` derive no `PartialEq`, so it needs
three new derives or a hand-written comparison. And getting the slice wrong does not fail loudly: it
silently restores the 30-second delay on some path, which is the defect `f16f4de` exists to remove —
a machine powering off after the parent cancelled it. **Any attempt must be inverted to be safe** —
wake unless *only* known-irrelevant fields changed — and must land with a test per handler proving
which ones still wake. Passing an explicit `Wake::{Enforcement,None}` at the four call sites is the
other option and is worse: it hands back exactly the remembering that the choke point removed.

**Trigger.** Worth doing when something makes a wake expensive — a heavier tick, a slower
`running_processes`, or a config-writing endpoint that stops being human-paced.

### O77 · A leaked session can only be revoked by signing every device out

Sessions persist 30 days across restarts, which is right: a parent on a phone behind a certificate
warning should not be signed out by every service restart. The consequence is that a cookie is
valid for 30 days and the **only** way to end one early is `api::change_password`, which calls
`sessions.clear_all()` and signs out *everything*.

So "I left my phone in a taxi" costs a password rotation plus re-pairing every other device, on a
service where signing in already costs a certificate click-through. There is no way to see which
devices hold a session, and no way to drop one.

The information already exists on both sides and is simply not joined: the audit log records every
`auth_success` with source IP and user-agent, and `tower_sessions::Record` carries the id and
expiry. The dashboard already shows *Recent access*, which answers the neighbouring question.

**Fix.** A *Signed-in devices* card — first seen, last seen, source IP, user-agent — with per-row
revoke. One route, one card, no schema migration.

**Weigh first:** `FileSessionStore` deliberately keeps reads off the disk and writes only on
mutation, and a revoke list is a read of the map on a parent action, which is fine. What is not
obviously fine is putting user-agent strings in the store — they are attacker-influenced text
rendered in the parent's dashboard, so this lands next to the escaping rules rather than beside
them.

### O78 · The notification decision rests on an assumption nobody has spent a minute testing

`app.js::titleFor` reasons through the options for telling a parent that their child is waiting,
and lands on the tab title. Two of its three rejections are solid and should not be re-opened: Web
Push needs an external push service, which "nothing leaves the house" forbids outright, and the
Badging API needs an installed app, which `MOBILE-APP.md` already refuted because an installed PWA
does not inherit the browser's certificate exception.

The third was not a rejection. It read: *"The Notifications API needs a secure context, and whether
a self-signed certificate accepted on a private IP counts as one is **unverified**"* — an honest
note that stood in for a decision.

**Settled, and the lean recorded here was backwards.** This entry used to say the research leaned
toward `false`. It leans toward `true`:

* W3C *Secure Contexts*, algorithm **Is origin potentially trustworthy?**, step 3 — *"If origin's
  scheme is either `https` or `wss`, return Potentially Trustworthy."* The algorithm has **no step
  that examines certificate validity**. The `localhost` / `file:` / `wss:` steps are additional
  exemptions for origins that are *not* `https`, not a narrowing of it; reading them as a list of
  the only trustworthy cases is the mistake this entry made.
* Chromium's stated position, on the W3C webpayments list: once the user proceeds past the
  interstitial the context **is** considered secure, and individual APIs are neutered case by case
  (Payment Request is the worked example) rather than the context being downgraded.

**The conclusion survives, for a reason nobody had named.** The API is unavailable on the devices
this product is used from, and not because of the certificate:

* `new Notification()` throws a `TypeError` in nearly all **mobile** browsers by design — MDN says
  so outright and adds "this is unlikely to change", because pages on phones do not run in the
  background. The phone path needs `ServiceWorkerRegistration.showNotification()`.
* Chromium **refuses to register a service worker** on an origin whose certificate error was
  bypassed (`crbug 40423989`) — which is every install of this tool.
* iOS Safari needs an installed Home Screen web app, which `MOBILE-APP.md` already refutes: an
  installed PWA does not inherit the certificate exception and cannot connect at all.

So notifications work in a **desktop browser tab and nowhere else** — the surface this product
cares least about, since the parent is on a phone. `app.js`'s `titleFor` comment has been corrected
to say this; it stated the wrong reason.

The `isSecureContext` check on the paired phone is still worth a console tap, but it is now a
confirmation rather than the decision.

**Which makes the fallback the more interesting half.** There is a permission-free option the
comment does not consider: the dashboard already holds an SSE connection and already knows the
moment a request arrives. An **audio cue** needs no secure context, no permission prompt, no
service worker and no installed app — only a prior user gesture, which a parent who has just signed
in has made. On a phone with the tab open that is the difference between a silent title change and
something a person notices.

Neither reaches a locked phone. That limit is real and must stay stated — but "cannot notify at
all" and "cannot notify while away from the page" are different claims, and the docs currently make
the stronger one.

### O79 · The meta-guard classifies a file as line-oriented from a substring of its own comments

**The class is otherwise closed.** A guard whose needle spans a syntactic boundary is defeated by
`rustfmt`; `src/srcscan.rs` is the shared answer and `tests/scanner_guards.rs` pushes new scanners
onto it. Every scanner in the crate now reads tokens rather than lines, the last two migrating in
`f76ba07` and the commit that added this entry. What is left is how the meta-guard decides which
files to police.

**`reads_lines` is `text.contains(".lines()")` over the whole file, comments included.** So a file
that no longer reads a line anywhere is still classified as line-oriented if any comment in it
*mentions* `.lines()` — and `tests/spawn_paths.rs` is now exactly that file: its scanners use
`find_tokens`, and the only `.lines()` left is a doc comment recording the historical bug. It
remains on the `KNOWN_SAFE` list for that reason alone.

This is the mirror of the defect fixed in `e8f257e`, which stopped adoption being decided by a
substring. Both read code out of prose; they differ only in direction — that one over-exempted, this
one over-reports. Over-reporting is the safe direction, which is why this is a tidy-up rather than a
hole, but the two rules should not disagree about whether comments are code.

**Adoption has the same shape, and its content is now wrong.** `adopted` requires the `use` item to
name `statements` — the reader `f76ba07` replaced *for failing open*. A file importing `find_tokens`,
the primitive that superseded it, is therefore not credited with adopting anything, and the guard's
own failure message still tells authors to use `statements`.

**Fix.** Decide both from parsed items rather than raw text: `reads_lines` from a `.lines()` call
outside comments, `adopted` from a `use` naming any of the reflow-proof primitives. Then delete the
`tests/spawn_paths.rs` row from `KNOWN_SAFE`, whose stated reason — that its needle is matched with
`match_indices` over the whole text — is stale as of this commit.


### O81 · Nothing bounds the number of connections

**The stalled-connection half of this is fixed; what is left is the count.** The entry used to
describe two holes — no first-byte deadline and no connection cap. The first is closed:
`server.rs` now serves **HTTP/1.1 only** and narrows ALPN to match, which skips `hyper-util`'s
`ReadVersion` state entirely and arms the h1 header timeout on the first poll. Measured over a
socket against the real binary on 2026-09-02:

| connection | before | now |
|---|---|---|
| TLS handshake, then a partial header block | open at 65 s | closed at 30.0 s |
| TLS handshake, then **zero bytes** | **open at 66 s** | **closed at 30.0 s** |
| ALPN offering `h2,http/1.1` | negotiated h2 | negotiates `http/1.1`, `200 OK` |
| ALPN offering `h2` only | — | refused at handshake, TLS alert 120 |

**A trap this entry previously recommended walking into, recorded so nobody repeats it.** The old
text called `http1_only()` "one line, and verified in `hyper-util` source". The `hyper-util` half
was right and the layer above it was not: `axum-server` hard-codes
`alpn_protocols = ["h2", "http/1.1"]` in `config_from_der`, which is where
`RustlsConfig::from_pem_file` ends up, and `Server::http1_only()` does not touch it. That one line
alone would have advertised h2, let every current browser negotiate it, and then fed an h2 preface
to an HTTP/1.1 parser — a blank dashboard for everyone, shipped as a cleanup. The fix needs the
ALPN narrowing too (`alpn_http1_only`), and
`serving_one_protocol_and_advertising_it_cannot_drift_apart` fails if either call is removed
without the other.

**What remains.** `axum-server`'s accept loop spawns a task per connection with no semaphore,
permit or limit of any kind. Nothing caps how many may be open at once.

**Severity is lower than it was, and the change is worth stating precisely.** Before, a stalled
connection was held *forever*, so the leak was permanent and grew until the attacker stopped
caring. Now every one of them dies after 30 s, so holding N connections costs the attacker a
sustained re-connect rate rather than a one-off cost. The peak is unchanged — one machine's
ephemeral range (~16 k on Windows' default) at the measured ~42 KB and one handle each is roughly
690 MB — but it now drains within 30 s of the attack stopping instead of persisting. Enforcement is
unaffected either way: both enforcers are separate tasks that touch no HTTP.

**Why a cap was not added at the same time.** It is a different shape of change. A deadline is a
property of one connection and could be bought by choosing a protocol; a cap is a property of the
listener, needs a `Semaphore` threaded through an accept loop this crate does not own, and has its
own failure mode — a cap reached by an attacker refuses the *parent* as readily as the child, which
is the harm the cap exists to prevent. That trade wants deciding on its own evidence rather than
bundled into a protocol decision.

**Trigger.** Any report of the dashboard becoming slow or unreachable while the PC is otherwise
healthy, or any work that puts this service anywhere other than a home LAN.

### O82 · The DST high-water mark does not survive a reboot, so tamper resistance loses an hour

**The mechanism is right and its memory is too short.** `clock::decide` catches a substituted time
zone by comparing the zone *identity* rather than the offset, which is correct and is the whole
point of the module. What it falls back to when the identity differs is
`high_water.max(anchor)` — and `HIGH_WATER_MINS` is a process-global `AtomicI32` that
`set_anchor` overwrites with the config's install-time offset at every startup. Nothing persists it.

**The order is what makes it reachable: change the zone, *then* reboot.** After the reboot the zone
is still changed, so the identity never matches again, so the mark is never re-seeded from the OS
and stays at the install-time anchor for the life of the install. Both steps are free — changing
the time zone raises no UAC prompt (`SeTimeZonePrivilege` is granted to Users), and a child owns
the console.

Measured against the real decision table. Installed at `+60` in winter, true local `+120` in
summer, child selects `UTC`:

| | fallback offset | error vs true local |
|---|---|---|
| service kept running through the DST change | `+120` | 0 min |
| after a reboot | `+60` | **60 min** |

A trusted clock an hour *behind* true local makes a **21:00 curfew fire at 22:00**, every night of
the half-year DST is in force. That is half of the two hours this module's own header says the
identity check closed, and `HIGH_WATER_MINS`' doc claimed outright that the fallback was "correct
rather than merely bounded" — corrected in place, because the claim was the more dangerous half.

**Why it is not fixed here.** Three candidate fixes, and choosing between them is a design decision
in the highest-consequence code in the project — the one that decides when a child's PC turns off:

* **Persist the mark.** Honest and complete. `config.json` is the natural home beside
  `tz_offset_mins`, but `clock::now()` is synchronous, has no `AppState`, and is called from pure
  helpers — so the write has to happen somewhere else that already owns a safe path. The enforcer's
  `usage_state.json` sidecar is written atomically every tick and is the one candidate that needs
  no new lock, at the cost of putting clock state in the tally file.
* **Assume maximum DST excursion on the tamper branch** (`anchor + MAX_DRIFT_MINS`). No persistence
  at all, and never fails open. It over-enforces by an hour in winter, which is defensible against a
  child who chose to tamper and is *not* defensible against a household that genuinely moved and has
  not re-anchored — the case `POST /api/re-anchor` exists for.
* **Ask Windows for the recorded zone's current offset** via `GetTimeZoneInformationForYear`. The
  only one that is exactly right in both seasons, and the only one needing new FFI and a stored
  `DYNAMIC_TIME_ZONE_INFORMATION` rather than an opaque key name.

**Trigger.** Any work on `clock.rs`, or the first parent who reports bedtime drifting by exactly an
hour in summer.

### O83 · The phone app cannot say which routine is running

> **Cross-repo** · pairs with `nestwatch-mobile`

`GET /api/usage/today` now carries `active_routine` — the name of the scheduled routine whose rules
are in force, or `null` when the base rules are. The dashboard renders it under the budget, because
a budget that changes at 16:00 with nothing to explain it reads as a defect rather than as a
setting working.

`nestwatch-mobile` shows the same budget and does not read the field. `UsageToday.fromJson` in
`lib/src/api/models.dart` takes keys one at a time with null-safe defaults and there is no
`json_serializable` anywhere, so the addition **cannot break it** — verified by reading, before the
golden files were regenerated. The gap is display, not compatibility: a parent on the phone sees the
number change and is told nothing, which is the same quiet failure the dashboard line exists to
close, on the surface more likely to be checked in a hurry.

Not filed on the other side, and deliberately: that repository was read-only for the session that
made this change. Whoever picks it up should file the counterpart there and turn this into a proper
`pairs with <repo>#<ID>` pair.

**Fix.** One nullable field on `UsageToday`, and one line under the figures on the home screen.

**Trigger.** The next change to `nestwatch-mobile`'s home screen, or the first parent who asks why
the app and the dashboard disagree about how much time is left.


### O84 · A refused earned grant still rewrites the config and wakes the enforcer

`api::extra_time` decides the day latch **inside** `try_update_config`, which is correct — that is
what serialises two concurrent pushes from one source so the second sees the first's latch. But the
already-granted branch sets `granted = false` and returns `Ok(())`, and `try_update_config` saves on
every `Ok`. So a push that grants nothing still serialises the whole `Config`, writes `config.json`,
and calls `heartbeat::wake`.

Bounded and not a security issue: the caller is authenticated, and an authenticated caller has far
larger levers than this (`POST /api/shutdown`). The cost is a pointless disk write and an enforcer
tick per refused retry, which for a phone scheduler retrying through the day is a handful.

**Not fixed deliberately.** The clean fix is a "no change, do not save" signal out of the mutate
closure, and `try_update_config` is the single choke point every config write in the service passes
through — the one place where an extra branch is most expensive to get wrong. That is not a trade
worth making for a few writes a day, but it should be made if a second caller ever needs the same
signal, because then it stops being a special case.

**Fix.** Either a third closure outcome distinguishing "changed" from "correct, but nothing to
persist", or hoist the latch read — which cannot be done without losing the serialisation the
current shape exists to provide, so it is the first one or nothing.

**Trigger.** A second handler wanting to mutate-or-not under the same lock, or evidence that config
writes are actually costing something on the target hardware.

### O85 · An idempotency replay can cross midnight and report a grant for a day that got none

`idempotency::RETENTION` is 48 hours and the cache does not know about days. A scheduler that grants
at 23:59 and retries the same logical grant at 00:01 is handed the stored `{"ok": true, "minutes":
N}` — correct as idempotency, since it *is* the same logical grant, and wrong as an answer to "did
today get bonus time", because the new day's latch was never touched.

Not reachable by accident from the current client: Voortgang uses a fresh key per logical grant and
reuses it only across that grant's retries, which is exactly the discipline the draft asks for. It
also has `extraMinutesToday()` and reads the grant back, which catches this — that method exists for
this class of lie.

The shape is recorded rather than fixed because both plausible fixes are worse than the symptom. A
key scoped to the local day would break the honest cross-midnight retry, which is the one case the
header exists for. Expiring the cache at midnight does the same thing with extra machinery.

**Fix.** Probably none. If it is ever worth closing, the honest form is to store the day alongside
the response and answer a cross-day replay with the *stored* outcome plus a field saying which day
it belongs to, letting the client decide.

**Trigger.** A second pushing client that does not read its grant back, or a report of bonus time
that the parent can see was never added.

### O86 · `/api/extra-time`'s response is a cross-repo contract with nothing pinning it

> **Cross-repo** · a second consumer now exists

`tests/golden/` pins every JSON shape `nestwatch-mobile` parses, and its own first line says so.
`POST /api/extra-time` is now parsed by a *different* repository — Voortgang, in `studygo`, reads
`ok`, `reason` and `minutes` out of that response in `lib/nestwatch/nestwatch_client.dart` — and
nothing on either side pins it. Renaming `reason`, or dropping `minutes` from the success body,
breaks that client with every test in both repositories green. That is precisely the failure
`tests/golden.rs` was built to prevent, one repository over.

It cannot be fixed by adding a golden file. `nestwatch-mobile/tool/check_golden.sh` walks
`nestwatch/tests/golden/*.json` and counts `MISSING HERE` as drift for every file that repo does not
also carry, so a new fixture there fails a repo that has no parser for it and never will. The
mechanism is hardwired to one consumer, and there are now two.

**Fix.** Decide what `tests/golden/` is for before adding to it. Either it is "shapes the Android
client parses" — in which case a second, separately-checked directory covers other consumers — or it
becomes "shapes any client parses", and `check_golden.sh` on the other side has to learn which files
are addressed to it. The second is better and needs a change in a repository that was read-only to
the session that found this.

**Trigger.** The next change to `extra_time`'s response body, or a third consumer.


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
