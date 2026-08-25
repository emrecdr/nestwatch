# Per-app foreground tracking — design

The screen-time report counts an app while its **process runs**, machine-wide, and only breaks out
apps that already have a limit set. This document designs the missing half: **how long each app was
actually in front of the child**, per day, for every app — not just the limited ones.

It is the fix for [O6](OPEN-FINDINGS.md#o6--screen-time-figures-are-machine-wide-and-count-running-not-focused-time),
and it retires half of the "foreground-app-aware limits — not yet" line in the README's
*Not included*. The other half — making limits *enforce* on focused time — is deliberately still not
built; see [Deliberately not done](#deliberately-not-done).

**Status: built, and not yet verified on a real machine.** Every piece exists — the watcher
(`src/watcher.rs`), the supervisor that keeps it alive (`session::run_watcher_supervisor`), the
collector (`foreground::Feed`), the accounting (`foreground::Tracker`), and the path into the daily
report. The accounting is unit-tested and mutation-checked; the Win32 half compiles and passes
clippy for the Windows target and **has never been executed**. Until someone works through
[the checklist](#on-device-checklist) on the target PC, treat it as untested code that builds —
which, on this project, is the state that has twice shipped something broken.

---

## What already exists

Most of this feature is already shipped. Only collection is missing.

| Layer | State |
|---|---|
| Storage | **Done.** `screentime.jsonl` rows carry an `apps: {name: minutes}` map (`rules::rollup_row`). |
| Report | **Done.** `screentime::parse_row` reads that map into `DayRow.apps` as `AppMinutes`. |
| API | **Done.** `GET /api/screentime` serves it; `GET /api/usage/today` serves today's per-app figures. |
| Dashboard | **Done.** Per-app bars on the Today card, per-app list under the 30-day chart. |
| Collection | **Built, unverified.** `helper --watch` measures focus; the enforcer folds it in each tick. Previously `Usage::accrue` charged an app whenever its process was *running*, and only for apps in `Targets::app_limits`. That still happens, and still drives enforcement — the focus figures are a second, separate number. |

So this is not a new subsystem bolted on. It is one new input feeding a pipeline that is already
built, already tested, and already rendered.

---

## Verified constraints

Every claim here was checked against a primary source before the design was drawn, because three of
them independently rule out simpler designs.

**A session-0 service cannot see the child's desktop.** `SetWinEventHook`'s `idProcess`/`idThread`
of `0` receives events *"from all processes on the current desktop"* — the hook is scoped to a
**desktop**, not a machine. Interactive Service Detection, which used to bridge that gap, was
removed in Windows 10 build 1803. The service therefore cannot host the watcher, however it is
written. ([SetWinEventHook][mssweh])

**The watcher needs a message pump.** *"The client thread that calls SetWinEventHook must have a
message loop in order to receive events."* And *"for out-of-context events, the event is delivered
on the same thread that called SetWinEventHook."* So the hook, the pump, and the callback are one
thread — it cannot be registered on one thread and serviced from another. ([SetWinEventHook][mssweh])

**Desktop scoping is a feature, not just a limit.** The lock screen (`winsta0\Winlogon`) and the UAC
secure desktop (`winsta0\Secure`) are *different desktops* from `winsta0\default`. A watcher hooked
on the default desktop therefore observes nothing while the machine is locked — which is exactly the
accounting rule `rules.rs` already enforces ("only charge screen time while the machine is actively
in use"). The correct behaviour falls out of the OS rather than needing to be coded and kept in sync.

**Duration cannot come from anywhere else.** DNS tells you a domain was contacted; the OS resolver
cache means a repeat visit inside the TTL emits no event at all, so DNS can never say *for how long*.
Browser history gives URLs but no dwell time, and records nothing in private browsing. **Only the
foreground watcher measures time.** Any "2h on Roblox" figure derived from DNS would be confidently
wrong — the same class of error the `measured` flag exists to prevent.

[mssweh]: https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwineventhook

---

## Architecture

One resident process **per interactive session**, reporting deltas back to the service.

Three corrections were made to this design after checking it against what shipping trackers actually
do. Each had a plausible-sounding wrong answer, and all three are recorded here rather than silently
fixed, because the wrong answer is the one a reader will reach for again.

**Collection is a hybrid, not pure event-driven.** The tempting design — one `EVENT_SYSTEM_FOREGROUND`
hook and nothing else — is what tiling window managers (komorebi, glazewm) do. Trackers do not.
ActivityWatch polls at 1s; Cobalt, the closest Rust analogue, uses `SetTimer` polling for foreground
and reserves hooks for title changes; screenpipe pairs a hook with a 5-second safety-net poll.
The reason is that **hooks miss transitions**, and for a window manager a miss is a cosmetic glitch
while for screen-time accounting it is a silent under-count that always favours the child.
`GetForegroundWindow()` also returns `NULL` during UAC, the lock screen, and briefly after a window
closes. So: **hook for edges, plus a 5-second reconciliation poll, plus the 30-second aggregation.**
Still far cheaper than ActivityWatch's unconditional 1-second poll, and it cannot silently drift.

**One helper per session, not one per console.** `WTSGetActiveConsoleSessionId` — which
`session.rs` uses today, correctly, for screenshots — returns exactly **one** session. Under fast
user switching a second child can be logged in simultaneously, disconnected but with apps running,
and their screen time would simply not exist. This feature therefore enumerates with
`WTSEnumerateSessionsW` and treats **`WTSQueryUserToken` succeeding** as the test for "a real
interactive user is here", since `WTS_SESSION_INFO` carries neither a username nor a logged-on bit.
That is a deliberate divergence from the existing helper's launcher, not an oversight in it: a
screenshot has one obvious subject, and screen-time accounting does not.

**Supervise by reconciling, not by reacting.** The service opts into `SERVICE_ACCEPT_SESSIONCHANGE`
and treats every notification merely as a hint to re-enumerate. Microsoft documents two traps that
make reaction-ordering unsafe: a service is only notified of a logon *"if it is fully loaded before
the logon attempt is made"*, and the handler *"should avoid operations that might block"* and must
return within 30 seconds. Reconciling against the real session list makes notification order
irrelevant — which matters because the only published fast-user-switching ordering is XP-era and
unverified on Windows 10/11. Triggers: service start, any session change, a 30–60s safety timer, and
helper exit.

Lock, unlock, and a fast-user-switch disconnect **idle** a helper; they never kill it.

### Idle is back-dated, not thresholded

The obvious way to handle "away from the keyboard" is a flag: once `GetLastInputInfo` reports more
than three minutes of silence, stop counting. That over-counts every idle episode by the whole
grace period, because the seconds between the user leaving and the threshold tripping have already
been banked.

`GetLastInputInfo` reports *how long ago* the last input was, so the moment presence ended is known
exactly: `last_input + IDLE_AFTER`. The watcher hands the tracker that timestamp rather than "now",
so the grace period is credited exactly once and nothing after it is credited at all.
`Tracker::bank` never moves its marker backwards, so noticing late cannot claw back time already
and correctly earned. The result is exact idle accounting with no threshold error, from an API call
the loop was making anyway.

```
SYSTEM service (session 0)                 Child's session (winsta0\default)
┌──────────────────────────┐               ┌──────────────────────────────────┐
│ run_rules_enforcer       │               │ nestwatch helper --watch         │
│  every 30s:              │               │                                  │
│   ├ read helper deltas ──┼◀── stdout ────┤  message pump                    │
│   ├ validate + clamp     │   (JSONL)     │   └ SetWinEventHook(FOREGROUND)  │
│   ├ Usage::accrue_fg     │               │       └ HWND → pid → exe name    │
│   └ rollup at midnight   │               │   accumulates app → secs         │
└──────────────────────────┘               │   emits one line per 30s         │
         │                                 └──────────────────────────────────┘
         ▼
   screentime.jsonl  ──▶  GET /api/screentime  ──▶  dashboard (already built)
```

**Why a pipe and not a named pipe or a local port.** `session.rs` already creates an inheritable
pipe and hands the write end to a `CreateProcessAsUserW` child. Reusing it adds no new IPC surface:
no named-pipe ACL to get wrong, no loopback port for another local process to connect to, nothing on
disk for a standard user to read or squat. The screenshot helper writes one PNG and exits; the
watcher writes one JSON line every 30 seconds and stays. That is the only difference.

**Supervision.** The watcher is long-lived in a session the child controls, so the service must
assume it can die at any moment — killed by the child, or lost to a session change. The service
respawns it when the pipe reaches EOF and a user is present, with a backoff so a crash-looping
helper cannot become a fork bomb.

### The helper is untrusted input

This is the design's sharpest edge and the reason the validation below is not optional.

**The watcher runs as the child.** It has to — it must live on the child's desktop to see the
child's windows. That means everything arriving over that pipe is attacker-controlled in the
threat model this project already assumes ("the child is the adversary and a reboot is their tool").
A child who finds the helper can kill it, or replace what it writes.

Four rules follow, and the aggregation enforces all four:

1. **Bound every delta by real elapsed time.** The service knows how long the tick actually took. A
   report of 900 seconds inside a 30-second tick is a lie and is clamped. `rules.rs` already does
   exactly this for its own tally (`elapsed.min(CHECK_INTERVAL * 2)`).
2. **Bound the *sum*.** Only one window has focus at a time, so the total foreground time across all
   apps in a tick cannot exceed the tick. This is the invariant a naive per-app clamp misses: a
   forged line claiming 30s each for twenty apps passes a per-app check and fails this one.
3. **Bound the *size*, at every point the data rests.** Rules 1 and 2 bound what the numbers may
   say; they say nothing about how many there may be, or how long one line may run. Three separate
   ceilings, because a forged report can grow in three separate places:
   - `MAX_LINE` on the **read itself**. `BufRead::lines` grows one buffer until it meets a newline,
     so a writer that never sends one takes the reader's memory with it — and the reader is the
     SYSTEM service that enforces the rules. Inspecting the line afterwards is too late; the
     allocation that mattered has already happened.
   - `MAX_APPS` / `MAX_PAGES` on the **`Feed`**, which is what accumulates between drains. `clamp`
     runs when the enforcer drains, thirty seconds apart; everything arriving in between lands in
     the feed first, at whatever rate the watcher writes.
   - `MAX_APPS` / `MAX_PAGES` on the **stored day**, which is persisted each tick and folded into
     the daily rollup, so growth there is growth on disk.

   In each case the *heaviest* entries are kept, so a flood costs the flood: an app with real hours
   behind it outweighs any number of one-second forgeries.
4. **Missing data is `null`, never `0`.** A killed helper produces *no* figures, which must never
   render as "he used nothing". This maps onto the `measured` distinction `screentime.rs` already
   draws, and for the same stated reason: collapsing them "would let a dead enforcer render exactly
   like a well-behaved child".

Rule 4 is why the helper cannot be a silent optimisation. If it dies, the report must say so.

Rule 3 is the one that was got wrong first. `MAX_PAGES` existed from the start, on the reasoning
that page titles are the only unbounded dimension — every tab is a new key — while `apps` is
"bounded by how many programs are installed". That is true of an honest watcher and this section is
the reason it cannot be assumed: executable names arrive over the same pipe as everything else. The
stored tally grew to 6,000 entries under a forged feed before the cap was added, and the read had
no ceiling at all.

### Data model

`Usage` gains one field, alongside the existing tally rather than replacing it:

```rust
pub struct Usage {
    pub day: Option<NaiveDate>,
    pub total_secs: u64,
    pub per_app_secs: BTreeMap<String, u64>,      // unchanged — drives ENFORCEMENT
    pub per_group_secs: BTreeMap<String, u64>,
    pub foreground_secs: BTreeMap<String, u64>,   // NEW — report only
}
```

`per_app_secs` keeps its exact current meaning and keeps deciding when a limit fires. `foreground_secs`
is additive, defaulted for serde so an existing `usage_state.json` loads unchanged, and read only by
the report. **No enforcement code path reads the new field.** That is what makes this change
unable to regress the thing that locks a child's PC.

The rollup row gains a parallel `focused` map beside the existing `apps` map, so a day's row can
report both "60 minutes running" and "40 minutes focused" without either being lost, and so a row
written before this feature still parses (the key is simply absent).

### Web pages

**Not tracked as domains. Tracked as page titles, from the window title.**

The watcher runs a **second [`Tracker`] keyed by page title**, fed by `browser_page`, which strips a
browser's own suffix off the window title. Time is credited to a page only while a recognised
browser is in front; every other window yields `None`, and `Tracker::focus(None, _)` charges those
seconds to nobody. So an hour in Notepad never appears in the page list.

Two Trackers rather than one map with mixed keys, because the keys are different kinds of thing:
`"chrome.exe"` is a program the enforcement tally also knows about, `"Roblox"` is whatever a tab
happened to be called. Mixing them would let a page title collide with an app rule.

**Page titles are capped everywhere they rest.** They are the widest dimension here — every tab,
video and renamed document is a new key — and they arrive from a process running as the child.
`clamp` keeps the heaviest `MAX_PAGES` from each report, the `Feed` caps what accumulates between
drains, and the enforcer re-caps the running day, because forty *different* titles every thirty
seconds would still reach thousands by bedtime in a map that is persisted to `usage_state.json` and
rolled into a year of history.

Executables get the same treatment at `MAX_APPS`, set far higher because the two are different
bets: dropping a title loses display text, while dropping an executable loses measured time shown
beside the enforcement tally. The ceiling sits where a real machine cannot reach it. It is not that
apps are a narrower dimension — it is that they are only a *closed set while the watcher is
honest*, which is precisely what this design does not assume.

The watcher already has the foreground window; reading its title costs one `GetWindowTextW`. A
browser's title carries the page title — `"Roblox - Google Chrome"` — which gives coarse attribution
for free and requires nothing to be installed or configured.

Private browsing is **expected** to be covered, on the reasoning that private modes suppress
*history*, not window titles — but that has never been observed on a running machine, and the
[open questions](#unverified) say so. It is called out here rather than left implicit because it is
the one claim in this document a parent might rely on to decide Incognito is not a blind spot, and
an unconfirmed "yes" is the wrong thing to lean on for that. Confirm it on-device before repeating
it anywhere a parent reads.

What it does **not** give is the domain. Getting that would mean writing browser policy into `HKLM`
to disable each browser's built-in DNS resolver, because Chromium's `kAsyncDns` is enabled by
default on Windows and browsers therefore resolve names themselves, invisibly to the Windows DNS
ETW provider. That is a real, working technique — and it is a change to the child's browser
configuration that belongs in front of a parent as a decision, not inside an installer as a detail.
**Declined for now.** See [Deliberately not done](#deliberately-not-done).

Note the consequence honestly on the dashboard: Roblox played in the **native app** is measured
exactly (match both `RobloxPlayerBeta.exe` and the Microsoft Store build `Windows10Universal.exe`),
while Roblox streamed through a **cloud-gaming site** in a browser tab counts as browser time and is
not separately identified.

---

## Deliberately not done

Recorded here so neither has to be re-argued.

**Foreground time does not enforce limits.** Making per-app limits count focused time instead of
running time sounds fairer, and it weakens enforcement: an idle-farming game or an autoclicker left
running in the background would stop consuming its limit entirely. `SECURITY.md` already names
background accrual as intentional. Changing which number locks a child's PC is a separate decision
from measuring a better one, and it is not made here.

**No DNS/domain tracking, and no registry writes.** Covered above. The mechanism is understood and
documented; the decision was to keep this feature free of browser reconfiguration.

**No browser history reading.** Cheap and gives real URLs, but it is defeated by one private window,
and it reads a child's full browsing history — a materially larger privacy step than measuring how
long a window was in front. Out of proportion to "how long on Roblox".

**No forced browser extension.** Best data available, but on a non-domain-joined home PC, Chrome and
Edge only force-install from the Chrome Web Store — so it would trade local-only distribution for
data quality.

---

## Resource budget

The design target was "maximum tracking, minimum resources". The mechanism chosen is the one that
does no work when nothing happens.

| | Cost |
|---|---|
| Idle (no focus changes) | One `GetForegroundWindow` every 5s for reconciliation. Not zero — see below. |
| Per focus change | `GetWindowThreadProcessId` + `QueryFullProcessImageNameW` + a map update, on a worker thread. A human generates a few hundred a day. |
| Per 30s | One short JSON line to a pipe. |
| Memory | One small process per session; the map is bounded by distinct apps used in a day. |

**No number here is measured, and the honest version of this section says so.** An earlier draft
claimed idle cost was *zero* on the strength of a pure-hook design; adding the reconciliation poll
that correctness requires makes that false. The figures usually quoted for comparable tools —
komorebi under 1% CPU, `aw-server-rust` at 9 MB idle — describe **somebody else's program**, and
this project's own history is that green-looking numbers hide defects until something runs on real
hardware. **The resource claim is therefore a prototype measurement to take, not a figure to
publish**, and nothing in the README should assert one until it is taken.

Three implementation choices carry most of the cost risk, all learned from tools that got them wrong:

* **The callback must return almost immediately.** Microsoft is explicit that out-of-context hook
  memory is held until the callback returns, and *"if a hook function does not process events quickly
  enough, USER resources are lowered, eventually resulting in a fault or extremely slow response
  times"* — a slow callback degrades the **whole desktop**, not just this process. The callback
  therefore only filters and pushes to a bounded channel; every resolution happens on a worker.
* **Narrow hooks only, never `EVENT_MIN..EVENT_MAX`.** A Microsoft engineer traced a 3–5% PowerToys
  CPU spike on mere cursor movement to exactly that, concluding *"it is more efficient to have 30
  hooks registered for the specific events necessary than it is to have 1 hook registered for a
  larger range"*.
* **`PROCESS_QUERY_LIMITED_INFORMATION`, and never WMI.** ActivityWatch's well-reported 5–30% Windows
  CPU problem is traceable in its source: it opens processes with `PROCESS_QUERY_INFORMATION`, which
  **fails against an elevated process**, and falls back to a WMI query — every second, forever. The
  limited right avoids the failure. This is a **security** fix as much as a performance one: with the
  wider right, a child could evade tracking simply by running something as administrator.

For scale, the **existing** rules enforcer already scans the entire process table every 30 seconds
(~4.9ms per call measured on macOS), forever. The watcher should sit well under that, but "should"
is the operative word until it is measured.

Deliberately avoided: polling `GetForegroundWindow` on a timer (does constant work to observe
nothing), and any network-level interception (a WFP driver needs attestation signing and can
bluescreen the family PC; an HTTPS proxy needs a root CA installed, which contradicts this
product's entire privacy posture).

---

## Testing

### What CI can check

The aggregation is pure — deltas in, tally out — so it is unit-tested on the dev machine with no
Windows, no clock, and no filesystem, exactly like `screentime::build_report`. Covered:

- a delta larger than the tick is clamped
- a *sum* of deltas larger than the tick is clamped (the forged-line case)
- a malformed or truncated line is skipped, not fatal
- a line with no newline in it is discarded rather than buffered, and skipping it does not swallow
  the line after it
- a flood of forged names is capped in the `Feed` and in the stored day, and the app that actually
  earned time survives being buried in one
- an absent helper yields `null`, never `0`
- an existing `usage_state.json` without the new field still loads
- a rollup row without the new key still parses
- enforcement figures are untouched by any foreground input

### What CI cannot check

Everything that makes it work: `SetWinEventHook` firing, the pump, `CreateProcessAsUserW` into the
console session, the pipe surviving a session change, behaviour on fast user switching, and whether
the watcher dies at the lock screen or merely goes quiet. None of it is reachable from `cargo test`,
`clippy`, or the Windows cross-compile — the three gates that were **all green** when `install`
failed on real hardware, and again when the screen-time chart shipped rendering nothing.

The watcher is written and compiles clean for the Windows target under `clippy -D warnings`, which
is worth exactly what it is worth: it proves the API calls exist with the signatures used, and
nothing about whether the thing works. Per [O4/O6's standing rule](OPEN-FINDINGS.md), code in this
tier is not trusted until [WINDOWS-TESTING.md](WINDOWS-TESTING.md) has been walked through on the
target PC.

### On-device checklist

1. Watcher starts with the session; `helper --watch` is present in Task Manager as the child.
2. Alt-tab between two apps; both accrue, and the totals track wall-clock.
3. A **minimised** app accrues no foreground time while still accruing running time.
4. Lock the machine (Win+L) for two minutes: foreground time does not advance.
5. Trigger a UAC prompt: the watcher survives it and does not attribute time to the secure desktop.
6. Fast-user-switch away and back; the watcher recovers.
7. **Kill the helper from Task Manager.** The report must show the gap as *not measured*, and the
   service must respawn it.
8. Confirm Roblox is attributed under both the direct and Microsoft Store builds.
9. Confirm the browser's window title reflects the active tab, and check what the Roblox app's own
   window title actually contains — **unverified**, and it decides how useful title capture is.
9b. Open a browser and confirm page titles appear under "In the browser" — then open **Notepad** and
    confirm it does *not*, which is the check that the page tracker is scoped to browsers rather
    than crediting every window.
9c. Play Roblox in the native app **and** through a cloud-gaming site in a tab. The first should
    appear under its process name, the second under a page title. That split is the whole reason
    page tracking exists.
10. **Sign a second user in and fast-user-switch between them.** Both sessions must accrue to their
    own totals. This is the case `WTSGetActiveConsoleSessionId` would have silently lost.
11. **Run an app elevated (as administrator) and confirm it is still identified.** This is the
    `PROCESS_QUERY_LIMITED_INFORMATION` fix, and it is an evasion route if it regresses.
12. **Measure CPU and RSS** over a normal evening's use, and only then write a figure anywhere.

---

## Unverified

Carried here rather than buried, because each would change something above.

- Whether the Roblox app's window title names the current experience while in-game.
- Whether Chrome/Edge Incognito window titles always carry the page title (expected yes).
- **Every CPU and memory figure.** No µs-level benchmark exists publicly for the hook, for
  `OpenProcess`, or for the HWND→identity chain, and none of the tool figures quoted above measures
  *this* program. Measure a prototype before any number is published.
- The right `EVENT_OBJECT_NAMECHANGE` debounce. It is a genuine firehose — it fires per *control*
  (check boxes, list views, status bars), and komorebi's source says plainly *"this spams the
  message queue, but I don't know what else to do."* Shipping values range from 100ms (Seelen-UI)
  to 1s (ActivityWatch's poll) with no consensus. The better lever is probably **scoping rather than
  debouncing** — re-registering a PID-scoped NAMECHANGE hook on each foreground change, so a
  background browser tab autoplaying video generates no events at all. That combines two proven
  patterns but matches no existing tracker, so prototype it before committing.
- Whether a `WinSta0\Default` hook actually receives `EVENT_SYSTEM_DESKTOPSWITCH` (community
  reports only). Subscribe opportunistically; do not make it load-bearing.
- Fast-user-switching notification *ordering* — the only published account is XP-era. This is
  precisely why the design reconciles instead of reacting.
