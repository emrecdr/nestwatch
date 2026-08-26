# On-device Windows test checklist

The cross-platform logic is covered by automated tests, and CI runs the Windows tests on a real
`windows-latest` runner. This checklist covers what neither can reach: behaviour that needs
**privileges, a logged-in user, or the machine's own configuration**. That means the SYSTEM
service and SCM restart behaviour, the `CreateProcessAsUser` session helper, ACL hardening, the
firewall rule and network profile, WTS session state, recovery-mode boot paths — and the
handful of things that
depend on a **real browser** rather than a test client, such as the origin check in §C.

Run through it once on his PC after installing.

## Short on time? Do these nine first

The full list is 161 items, which is why it keeps not happening. These nine are the ones whose
answers change what you'd do next — about fifteen minutes, and worth more than the rest combined.
Each links to its full entry below.

1. **Is his account a standard user?** (§0) — `net localgroup Administrators`. If he is listed,
   stop: every other check on this list is measuring something that a local administrator can
   simply undo, and the honest fix is to change the account type first.
2. **The question in the private operational notes.** It decides whether a reboot bypasses
   everything here. If the answer is bad, one install-time change fixes it — and nothing else on
   this list matters until you know.
3. **Screenshot returns the live desktop, not a black image** (§D) — the single hardest piece of
   the whole system to get right, and the one most likely to be broken by a Windows update.
4. **A game in exclusive fullscreen is captured, not black** (§D1a) — the capture backend was
   replaced for exactly this. The old one returned black, and the child could select that failure
   from the game's own display settings with no prompt and no admin right. An ordinary desktop
   captures fine either way, so nothing else on this list distinguishes the two backends. One
   game, one setting, thirty seconds.
5. **Lock actually locks the screen** (§D) — same mechanism as the screenshot, opposite direction.
   If screenshots work and lock doesn't, that narrows the fault sharply.
6. **He cannot stop the service or read the data directory** (§B) — two commands as him. This is
   the tamper-resistance claim; everything else assumes it holds.
7. **A blocked app still gets killed** (§E5) — the enforcer's process scan was rewritten to stop
   gathering four things it threw away, and that code has never executed anywhere. If it returns a
   short list, blocked apps quietly survive and the dashboard looks perfectly normal. One app, thirty
   seconds.
8. **Every dashboard card still works from a real browser** (§C) — the origin check
   fails *silently*, as buttons that do nothing rather than an error. A test client cannot catch
   this; only a browser can.
9. **A day the PC was off shows as "not measured", not as a zero** (§D) — if those two states
   look alike, a stopped enforcer reads exactly like a well-behaved week, which is the failure the
   feature exists to prevent. The chart *has* now been seen rendered, in Chrome on macOS with
   seeded data covering all three states — which is how the bug that made it draw nothing at all
   through 0.2.3 was found. It has still never been seen on Windows, with real data, on a phone.

Everything below is worth doing eventually. Nothing below is worth doing before these.

**One addition since this list was written.** Foreground tracking (§D2) is new code that has never
executed anywhere — not on this machine, not on any machine. It is not in the nine above because
nothing depends on it: it measures and reports, it never enforces, so if all of it is broken the
locks and limits behave exactly as they do today. That is also why it is safe to leave until after
the nine. But it is the largest untested surface in the build, so when you do reach it, expect
failures rather than being surprised by them.

---

## 0. Prerequisites

- [ ] **Verify the download before you run it.** It is about to run as SYSTEM on his PC.
      Two independent checks, and the second is the stronger one:
      ```powershell
      Get-FileHash nestwatch.exe -Algorithm SHA256 | Format-List   # match the .sha256 file
      gh attestation verify nestwatch.exe --repo emrecdr/nestwatch
      ```
      The hash only proves the download did not corrupt — it ships from the same place as the
      file. The attestation is a signature proving this exact binary came out of the project's
      release workflow, and it fails closed: alter one byte and the lookup, which is keyed on
      the file's own digest, finds nothing to check it against. Skip this only for a binary you
      built yourself.
- [ ] **Confirm which build you are looking at:** `nestwatch.exe version` (or `doctor`, whose
      report leads with it). Worth doing first on a machine you visit rarely — every check below
      is about *this* build, and it is also how you tell whether a given security fix is present.
- [ ] **His account is a *standard* user, not an administrator.** Check:
      `net localgroup Administrators` — his username must **not** be listed. (Tamper-resistance
      is meaningless against a local admin.)
- [ ] **The PC's network is set to *Private*** (Settings → Network → properties). The firewall
      rule is scoped to `private,domain`; on a "Public" network it won't match.
- [ ] You have `nestwatch.exe` (from CI artifact, a release tag, or a cross-build).
- [ ] You're at an **elevated (Administrator) console** for install/uninstall.

## A. Install

- [ ] **Pre-flight runs before anything is touched, and before the password prompt.** Run
      `.\nestwatch.exe install` and confirm the checks appear *first*. On a machine with nothing
      wrong it says `Pre-flight checks passed.` and continues.
- [ ] **A blocker refuses cleanly.** Occupy the port first — in a second window,
      `python -m http.server 8443` or any listener — then run `install`. It must report
      *"port 8443 is already in use"*, say **nothing has been changed on this machine**, and
      exit **before** asking for a password. Confirm `dir C:\ProgramData\HostHealth` is
      unchanged (or absent).
- [ ] **The Public-network caution offers to fix itself.** With the network set to Public, run
      `install`: it should offer `Fix "this PC's network is set to Public" now? [y/N]`. Answer
      **y** and confirm it reports `done: network set to Private`, then re-checks and says the
      remaining findings are gone. `Get-NetConnectionProfile` must now show Private.
- [ ] **Answering no changes nothing.** Set the network back to Public, run `install`, press
      Enter (the default is no) — it must say *skipped*, leave the profile Public, and still
      install.
- [ ] **`--fix` needs no console.** `install --fix` applies the same fixes without prompting;
      useful when running it over a remote session where nobody can answer.
- [ ] **Unblock first:** right-click the downloaded `nestwatch.exe` → Properties → tick
      **Unblock** → OK. Confirm that running it then does *not* show "Windows protected your PC".
- [ ] **Non-elevated install is refused:** from a *normal* (non-admin) PowerShell, run
      `.\nestwatch.exe install` → it stops immediately with "must be run from an elevated
      console" and creates **nothing** (`dir C:\ProgramData\HostHealth` → not found).
- [ ] `nestwatch.exe install` (or `install --port <N>`) completes and prints a **TLS SHA-256
      fingerprint** — write it down.
- [ ] The success output shows the **real LAN URL** (e.g. `https://192.168.1.42:8443`), not a
      `<this-pc>` placeholder, plus the child's `/ask` URL.
- [ ] **Scan the QR** with your phone's camera → it opens the dashboard **already signed in**
      (after the one-time certificate warning). Then confirm the token is single-use: opening the
      same pairing URL again in a private window shows the **login page**, not the dashboard.
- [ ] `nestwatch.exe pair` prints a fresh QR that also works.
- [ ] `nestwatch.exe doctor` reports the service running, the port listening, the firewall rule
      present, the network profile Private, and lists the local administrators. With no rules set
      it warns "nothing is being enforced yet". Fix anything it flags.
- [ ] **A busy port is reported, not hidden.** Occupy the port first (e.g. in another window:
      `python -m http.server 8443`), then run `install`. It must **fail with a clear message**
      naming the port — not print "Installed" and leave you with a dead dashboard. Free the port
      and re-run.
- [ ] Service exists & is running: `sc query HostHealthService` → `STATE: 4 RUNNING`.
- [ ] Runs as SYSTEM, auto-start: `sc qc HostHealthService` → `SERVICE_START_NAME: LocalSystem`,
      `START_TYPE: 2 AUTO_START`.
- [ ] Recovery configured: `sc qfailure HostHealthService` → restart actions listed.
- [ ] Binary in place: `C:\Program Files\HostHealth\host-health.exe` exists.
- [ ] Firewall rule present: `netsh advfirewall firewall show rule name=HostHealthService`
      → LocalPort = your port, Profiles = Domain,Private.
- [ ] **Service diagnostics are written:** `dir C:\ProgramData\HostHealth\service.*.log` shows a
      dated `service.<YYYY-MM-DD>.log`, and (as admin) `type` it → the "listening on…" startup
      line is there. This is your debugging trail if anything below misbehaves — the SYSTEM
      service has no console, so this file is where its errors/warnings go. It's ACL-locked like
      the rest of the dir, so as HIM `type` should say Access denied.

## B. Tamper-resistance (the key differentiator — do these while logged in as HIM)

- [ ] Cannot stop the service: `sc stop HostHealthService` → **Access is denied (5)**.
- [ ] Cannot delete it: `sc delete HostHealthService` → **Access is denied (5)**.
- [ ] Cannot read the data dir at all: `dir C:\ProgramData\HostHealth` and
      `type C:\ProgramData\HostHealth\config.json` → **Access is denied**. The whole folder is
      ACL-locked to SYSTEM + Administrators, so the password hash, TLS key, **and every log**
      (`audit.jsonl`, `usage.jsonl`, `screentime.jsonl`, `time_requests.jsonl`, `usage_state.json`,
      plus `.jsonl.1`
      rotation backups) are unreadable and undeletable by the child.
- [ ] Cannot modify/delete the binary: `del "C:\Program Files\HostHealth\host-health.exe"`
      → **Access is denied**.
- [ ] In Task Manager → Details, `host-health.exe` runs as **SYSTEM**; "End task" → Access denied.

## C. Remote access, cert, login (from your phone/laptop on the same Wi-Fi)

- [ ] Browse to `https://<his-pc-ip>:<port>` — it **loads** (proves the firewall rule works; if
      it times out, see Troubleshooting).
- [ ] Browser shows a one-time "not trusted" warning. View the cert → its **SHA-256 matches**
      the fingerprint from step A. Proceed. (Lost the fingerprint? As admin, run
      `nestwatch.exe fingerprint` to re-print it — handy when verifying a new phone/laptop later.)
- [ ] Login page shows the bland **"Host Health"** header (not "Nestwatch").
- [ ] Wrong password → rejected; after ~5 quick wrong tries → **locked out** briefly (429).
- [ ] Correct password → dashboard shows **"🪺 Nestwatch"**.
- [ ] **The dashboard still works end to end.** Every request is now checked
      against the browser's own report of where it came from, and a mistake here would show up as
      buttons that silently do nothing (`403`) rather than as an error. Click through **each**
      card once — screenshot, kill, lock, curfew save, rules save, grant time, approve a request —
      and confirm none fails. Do this on **both** the phone and a laptop, and once from a
      **bookmark** and once by **scanning a fresh `nestwatch.exe pair` QR**, since those arrive
      with different origin headers than clicking inside the app.
- [ ] **The child's page still works:** open `https://<his-pc-ip>:<port>/ask` directly in the
      address bar and submit a time request. Typed-URL loads are the case most likely to be
      caught by an over-strict origin rule.

## D. Core features

- [ ] **Screenshot** → shows his **live desktop** (NOT black). This proves the session helper
      ran in his session via `CreateProcessAsUserW` — the trickiest piece. Black = Session-0
      problem (see Troubleshooting).
- [ ] **Running apps** lists real processes, heaviest first.
- [ ] Open Notepad on his PC → Refresh → it appears → **Kill** it → it closes; the row disappears.
- [ ] **Lock** (navbar 🔒) → his screen locks to the sign-in prompt (password to resume). This
      exercises the session-helper lock (`helper --lock` launched into his session) — a Session-0
      service can't lock the desktop directly, so if nothing happens see Troubleshooting.
- [ ] **Live** toggle on the Screen card → the screenshot refreshes without clicking; toggling it
      off (or logging out) stops the refresh. The **2s / 5s / 15s** buttons appear beside the
      toggle only while Live is on, and changing one keeps the view running rather than switching
      it off.
- [ ] **The picture says how old it is.** With Live on, a line under it reads "updated Ns ago" and
      counts up on its own. Now **stop the service** (`sc stop Nestwatch`) with Live still on: the
      line must turn red and read "not updating — last frame …". This is the whole point of it. A
      frozen live view and a child sitting still are the same picture, and before this the last
      good frame simply stayed on screen with the toggle still lit.
- [ ] **Expand fetches a sharper frame, and *stays* sharp.** With Live running, click the picture
      (or **⤢ Expand**). The full-size view must be visibly sharper than the thumbnail — the live
      stream sends a 960×540 preview, and expanding asks for a full-resolution one. If it looks
      like a stretched, soft version of the thumbnail, the refetch is not happening.
      <br>**Then leave it open for several refreshes.** It must stay sharp. Live frames follow the
      surface on screen, so while this view is open the timer asks for full-resolution frames too.
      If the picture degrades to a soft one after a few seconds, `liveTier()` is not being consulted
      and the old defect is back — the sharp frame surviving only until the next tick.
- [ ] **A click is never silently swallowed.** With Live running at the 2s cadence, press **Take
      screenshot** repeatedly, including while a frame is obviously mid-flight. Every press must do
      something visible — a spinner, a new picture, or a failure toast. A press that produces no
      reaction at all is the defect this replaced: the button stayed enabled, accepted the click and
      discarded it, which at a 15s worst case against a 2s cadence was the usual outcome rather than
      a rare race.
- [ ] **The audit log does not fill up with live frames.** Open the full-size view with Live running
      and leave it a few minutes, then read **Recent access** (or `GET /api/audit`). There must be
      *coalesced* `live_view` lines carrying frame counts — **not** one `screenshot_taken` row per
      frame. Frames the timer fetched are counted; only captures a person asked for get a line each.
      This is the one item here with a security consequence: at roughly 1,800 rows an hour — the
      fastest cadence on offer — a timer
      would evict every login, kill and password change from the log to make room for itself.
- [ ] **Turning Live off stops it immediately.** Toggle Live off while a capture is mid-flight
      (easiest at the 2s cadence). The picture must not change afterwards. A frame arriving after
      the parent said stop means the in-flight request was not aborted.

> **Partly verified in a browser, on macOS, against the fake controller** (2026-08-25) — and the
> capture path has **changed since**, so the three lines marked below need redoing rather than
> trusting. What still holds: the age line renders and counts up;
> with `/api/*` returning 503 the line turns red and reads *"not updating — last frame Ns ago"*
> while the toggle stays lit, and clears to *"updated 2s ago"* when the API returns. Preview was
> 21,985 B against full's 62,795 B on the fake's 1280×720 frame. The console showed no CSP or
> Alpine parser errors — which matters because that build fails *silently*.
>
> **Superseded by later changes, and unverified since (2026-08-26):** that every timer frame
> requests `tier=preview` — it now requests `full` while the full-size view is open; that
> 41 preview frames plus 3 full captures wrote **4** audit lines — the audit now keys on who asked
> rather than on tier, so the arithmetic differs; and that a mid-flight capture is simply aborted —
> a person's click now supersedes it rather than being dropped.
>
> None of that touched Windows, a real desktop, or the WGC backend. The items above and in §D1a are
> what those numbers do not cover.

- [ ] **The "when the PC was in use today" strip matches reality.** Sign in and out a couple of
      times during the day, then compare the strip against what actually happened. Two specific
      checks beyond "there are bars": pause the rules mid-session and confirm the bar *ends* there
      rather than running on, and stop the service mid-session (`sc stop Nestwatch`) then restart
      it — that session's end is unknowable and must appear as a thin gold marker, never as a bar
      stretching to the restart.
- [ ] **A new app is called out after it is first used.** Install something the child has never
      run (or rename a copy of an existing exe), open it for a few minutes, and let the day roll
      over at midnight. The next day the report must name it under "new apps". Two things to check
      beyond that it appears: the count of earlier days is right, and an app used every day is
      **not** listed. This runs off the foreground watcher, so it is also an end-to-end proof that
      the watcher reported at all — if nothing is ever called new, suspect §D2 before suspecting
      this.
- [ ] **The first day of history says nothing.** On a machine where the watcher has only ever run
      for one day, the report must show no new-apps panel at all rather than listing everything.

### D1a. The capture backend (new, and the reason for the Windows version floor)

The capture moved from GDI `BitBlt` to Windows.Graphics.Capture. Everything here is new code that
has **never run anywhere**, and the failures it fixes are invisible on an ordinary desktop — which
is exactly why the old backend survived thirteen review passes.

- [ ] **A yellow border appears around the screen while the parent is watching.** Windows draws
      it; the app cannot and does not suppress it. Its presence is the proof the new backend is
      live — the old one drew nothing. Its absence with a working picture means the build fell back
      to GDI.
- [ ] **Capture a game running in EXCLUSIVE FULLSCREEN.** Set a game to exclusive fullscreen in its
      own display settings (not borderless), then take a screenshot. **This is the single most
      important item in this file.** The old backend returned black here, and the child can select
      that failure from the game's own settings menu with no prompt and no admin right. A black
      frame is indistinguishable from a monitor that is switched off.
- [ ] **Capture DRM video** — Netflix, Disney+ or Prime Video playing in a browser. Same failure
      class as above.
- [ ] **Capture on a display scaled above 100%** (Settings → Display → Scale, 125% or 150%). The
      whole desktop must be in the frame. If the picture holds the desktop in its top-left corner
      with black down the right and bottom, `SetProcessDpiAwarenessContext` is not taking effect —
      that was predicted to lose 36% of the frame at 125% and 55.6% at 150%, and was never verified.
- [ ] **Two monitors: the picture is the PRIMARY one.** Arrange the secondary as monitor 1 in
      Windows' display settings if possible, so enumeration order and primary disagree. The old
      code took whichever enumerated first.
- [ ] **On a machine below build 18362** (if one is reachable): `nestwatch install` reports a
      **caution** naming the build, and installs anyway. Screen-time limits, curfew and blocked
      apps must all still work there — only the picture is expected to fail.
- [ ] **Shut down** → Windows shows a countdown notification, then the PC powers off.
- [ ] **The system tools still resolve.** `shutdown`, `rundll32`, `icacls`,
      `netsh` and `sc` are now invoked by absolute path instead of by name. CI proves those files
      exist on a stock Windows image, but not that *this* machine's `System32` is where the API
      says it is. **Lock** and **Shut down** above are the live proof for two of them; `install`
      completing without an ACL or firewall warning covers the other three. If any of those
      started failing with "program not found" after upgrading, that is this change and it should
      be reported — not worked around.
- [ ] **The screen-time chart draws bars at all.** Check this before anything subtler about it:
      the Screen time card must show a row of columns, not an empty space above the legend. Through
      0.2.3 it drew **nothing** — thirty days of data and a blank chart — while the total, the
      average and the day-by-day table below it were all correct, so the card looked merely sparse.
      If it is blank here, that regression is back.
- [ ] **Screen-time report.** After the PC has been through at least one midnight with the
      service running, the Screen time card shows a bar for that day and "Measured days" counts it.
      Days the PC was off must appear **hatched** ("not measured"), never as a zero bar — that
      distinction is the difference between "he didn't use it" and "we weren't watching". A day he
      was signed in but used nothing is a third case: a thin bar you can still hover, not a hatch.
- [ ] **Hovering a column names the day and its figure** — the tooltip is how the chart is read at
      all on a phone, and it moved from an SVG element to a plain `title` in this release.
- [ ] **Per-app rows are plausible.** The most-recent-measured-day list should roughly match what
      he actually ran. The **Minutes** figure counts apps that are *running*, not focused, so a
      launcher left open all evening will look large — that is expected, not a bug. The separate
      **focused** figure is the one that should match what he was actually doing.

## D2. Foreground tracking (never run on any machine)

Everything in this section is **new code that has compiled and linted and has never executed**. It
is the largest untested surface in the build, and unlike the rest of this document it has no track
record at all — treat a failure here as expected rather than surprising. The design and the
reasoning are in [FOREGROUND-TRACKING.md](FOREGROUND-TRACKING.md).

**Watch the Today card, not the daily report.** The dashboard's Today panel now carries **In front
today** and **In the browser today**, fed by the same watcher and refreshed about once a minute. It
is the fastest signal available: everything below shows up there within a minute or two, where the
day-by-day report only gains a row after midnight's rollover. Keep it open on a second device while
working through this section — that is what turns most of these checks into a glance rather than a
wait until tomorrow.

- [ ] **`nestwatch helper --watch` is running as him.** Task Manager → Details, while he is signed
      in. If it is absent, nothing below can pass, the Today card shows no "In front" section at
      all, and the report shows no focused minutes — which correctly renders as *not measured*
      rather than as zero.
- [ ] **Alt-tab between two apps for a few minutes.** Both accrue focused minutes, and the totals
      track wall-clock rather than drifting.
- [ ] **A minimised app accrues no focused time** while still accruing running minutes. That
      difference is the entire point of the feature.
- [ ] **Lock the PC (Win+L) for two minutes.** Focused time does not advance across the lock.
- [ ] **Walk away for four minutes without locking.** Focused time stops after about three — the
      away threshold — and does not resume until input does.
- [ ] **Trigger a UAC prompt.** The watcher survives it, and no time is attributed to the secure
      desktop.
- [ ] **Kill `helper --watch` from Task Manager as him.** Two things must both happen: the gap
      shows as *not measured* rather than as zero minutes, and the service respawns the helper
      within about a minute.
- [ ] **Sign a second user in and fast-user-switch.** Both accounts accrue to their own totals.
      This is the case the single-console-session approach would have lost silently.
- [ ] **Run something elevated (as administrator)** and confirm it is still named in the report.
      If it is not, that is an evasion route, not a cosmetic gap.
- [ ] **Roblox is named under both builds** — the direct download (`RobloxPlayerBeta.exe`) and the
      Microsoft Store build (`Windows10Universal.exe`). Switching between them is the obvious dodge.
- [ ] **Browser pages appear under "In the browser"**, and **Notepad does not**. The second half is
      the real check: it proves page tracking is scoped to browsers rather than crediting every
      window's title.
- [ ] **Roblox played natively vs. streamed through a cloud-gaming site in a tab** land in
      *different* places — the app under its process name, the tab under a page title. That split
      is why page tracking exists at all.
- [ ] **Open a private/Incognito window.** Does its title still appear? **This is an open question,
      not a known answer** — the expectation is yes, since private modes hide history rather than
      window titles, but nothing has confirmed it. Whatever you observe, write it down: a parent
      may be relying on this to decide whether Incognito is a blind spot.
- [ ] **Measure the cost.** Task Manager → Details → CPU and Memory for `helper --watch` over a
      normal evening. Every performance figure in the design document is extrapolated from other
      people's programs; this is the first real one, and nothing should be published until it exists.

## D3. What the dashboard now shows (verified in a browser, never on this machine)

These reached the screen this release. All were driven in a real browser against a seeded instance,
so the shapes are known to work — what has not been checked is any of it against **real data from
this PC**, which is the half that matters and the half only you can do.

- [ ] **"In front today" lists real apps, on the same afternoon.** Until this release those minutes
      were measured every thirty seconds and only reached you the next morning. Use the PC for a
      while as him, then open the dashboard: the apps he actually had in front should be listed with
      minutes, under **Today's screen time**.
- [ ] **The "no focus figures" note appears when the helper is not running.** Kill `helper --watch`
      as him, use the PC for five minutes, then look. The card must say the figures are missing and
      point at `nestwatch doctor` — not simply show an empty list, which reads as "he did nothing".
      The five minutes matter: below that the card deliberately stays quiet rather than accuse a
      helper that may be fine.
- [ ] **The report answers "how much Roblox this month".** Screen time card → the most-used lists
      cover the whole window, not one day. Switch **7 / 30 / 90** and confirm the totals and the
      headings both change.
- [ ] **Clicking a bar shows that day.** Pick a day on the chart; all three breakdowns below should
      follow it and the heading should name that date. Pick a hatched (unmeasured) day: the panels
      must say *nothing recorded for this day* rather than disappearing.
- [ ] **Categories appear — if you have set up app groups.** With groups configured, tomorrow's
      report should carry a **By category** list. It cannot show history from before this release;
      days recorded earlier have no category data and correctly show none.
- [ ] **The tab title counts waiting requests.** Have him ask for extra time and leave the dashboard
      open in a background tab. The tab should read **(1) Nestwatch**. This is the only way this
      product can tell you something without a tab being open somewhere — there is no push
      notification and there will not be one.
- [ ] **App names read like apps.** `RobloxPlayerBeta.exe` should show as *Roblox*. Hover gives the
      real file name back. If something common on his PC still shows as an executable, say which —
      the list of friendly names is curated by hand and only covers what we thought of.

## E. Curfew (the enforcement feature)

- [ ] Set a window that includes **now** (e.g. now-1min → now+10min), warn = 60s, Save.
- [ ] Within ~30s the PC shows the shutdown countdown. **Cancel the test:** disable curfew in the
      dashboard → within ~30s the pending shutdown is **aborted** (no power-off). Verify:
      `shutdown /a` as admin should say "no shutdown in progress" (we already aborted it).
- [ ] **Anti-bypass: cancelling doesn't help.** Re-enable the covering window; when the countdown
      starts, as HIM run `shutdown /a`. Within ~30s the PC should **shut down with no countdown at
      all** — the re-issue is deliberately immediate, because a second warned countdown would just
      be another one to cancel. Try it two or three times in a row (or loop
      `for /l %i in () do (shutdown /a & timeout /t 5)` as him): it must still power off.
      **Save your work before this one.**
- [ ] **Bedtime warnings arrive early.** Set a window starting **7 minutes from now** and leave it
      alone. Within ~30s of the 5-minute mark a **"Bedtime in 5 minutes — good time to save."**
      message box appears on his desktop, and another at **1 minute**. (For the 15-minute one, set
      the start 17 minutes out instead and wait.) Each fires **once** — not every 30s. Then disable
      curfew before it actually shuts him down.
- [ ] Set a normal bedtime window (e.g. 22:00→07:00) and leave it for real use.

## E2. Screen-time rules & time requests

- [ ] In **Screen-time & app limits**, set a tiny **Daily limit** (e.g. 1 min), action **Lock
      screen**, warn 30s, Save. About 30s before the limit, a **"Screen time is up. This PC will
      lock in 30 seconds."** message box appears on his desktop (proves `WTSSendMessageW` from the
      service). After ~1–2 min of use the screen **locks**. Set it back to 0 (off) afterwards.
- [ ] **Screen-time warnings arrive early.** You don't have to burn a whole budget to test this —
      aim the limit just above where he already is. Check today's **used minutes** on the
      dashboard, then set the **Daily limit** to `used + 16`. Within ~30s a **"15 minutes of screen
      time left today."** message box appears. Set it to `used + 6` for the 5-minute one, and
      `used + 2` for **"1 minute of screen time left!"**. Each fires **once**, not every 30s.
- [ ] **A warning that didn't reach him isn't recorded as if it did.** After the above, check
      **Usage history** for `budget_countdown` rows — one per warning you actually saw on his
      desktop, and none for any that failed to appear.
- [ ] **Granted time re-arms the warnings.** With a small daily limit spent, approve a **+20 min**
      grant → he gets the 15/5/1 warnings again on the way down the granted time, rather than
      silence until it locks a second time.
- [ ] **Locked/idle time doesn't count.** With a small daily limit set and some minutes already
      used, **lock his screen** (Win+L) and leave it a couple of minutes, then check **Usage
      history / today's tally** — the used-minutes figure has **not** advanced while locked (proves
      the `WTSQuerySessionInformation` session-state gate). It resumes when he unlocks. (Same holds
      at the sign-in screen with nobody logged in — a PC left on overnight won't burn the budget.)
- [ ] **A grant rescues an in-flight shutdown.** Set the daily limit action to **Shutdown**, tiny
      limit; when the shutdown **countdown** starts, from your dashboard approve a `/ask` request
      (or use bonus time) → within ~30s the pending shutdown is **cancelled** (verify as admin:
      `shutdown /a` says "no shutdown in progress" because we already aborted it). Set action back
      to Lock afterwards.
- [ ] Add a **Blocked app** (e.g. `notepad.exe`), Save; launch Notepad → within ~30s it's
      **killed**. Remove it afterwards.
- [ ] **Know the limit of app rules** (this one is *expected to fail* — run it so you're not
      surprised later). With `notepad.exe` blocked, as HIM run
      `copy C:\Windows\System32\notepad.exe %USERPROFILE%\notes.exe` and launch `notes.exe`.
      It keeps running: matching is by filename. There is no fix at this level — see the note in
      the README. Rely on the daily budget and curfew for limits that must hold, and use
      AppLocker / Microsoft Family Safety if you need a real app wall.
- [ ] Add a **Per-app limit** (e.g. `notepad.exe` = 1 min), Save; run Notepad → after ~1 min
      it's killed on sight, while other apps keep running. Remove it afterwards.
- [ ] **App group (shared pool):** add a group (name `Test`, apps `notepad.exe, mspaint.exe`,
      1 min), Save. Open Notepad for ~30s, close it, open Paint for ~30s → once the shared minute
      is spent, the running member is killed (the pool is shared across both, not per-app). Remove
      the group afterwards.
- [ ] **Budget survives a restart:** with a small daily limit set, accrue a little usage, then
      `taskkill /f /im host-health.exe` (it auto-restarts) → the used-minutes tally is **not**
      reset (it's persisted in `usage_state.json`); enforcement resumes from where it was.
- [ ] **…and survives it while he's *using* the PC.** The tally is now written only when it
      changes, so this is the case that would break if that went wrong. With him **actively at the
      machine** and a limit set, note the used minutes, wait ~2 min, then `taskkill /f /im
      host-health.exe` **without** a clean stop. After it restarts, the tally must include those
      two minutes — not just whatever it was at the last change before them. (Check the file's
      timestamp too: `dir C:\ProgramData\HostHealth\usage_state.json` — it should be recent while
      he's active, and *stop* advancing once he locks the screen. Both halves matter: fresh while
      active proves it still saves, frozen while idle proves it stopped writing needlessly.)
- [ ] **The child's page shows their own time:** open `https://<his-pc-ip>:<port>/ask` → the top
      shows **minutes left today** with a progress bar (or "No time limit today" when none is
      set), and it updates after a code is redeemed. As HIM, confirm it reveals **no rules** —
      no blocked-app names, no per-app limits, no curfew times.
- [ ] From his browser, open `https://<his-pc-ip>:<port>/ask`, request e.g. 15 minutes →
      you see it under **More-time requests** in the dashboard → **Approve** → the granted
      minutes are added to today's budget (and appear in **Usage history**).
- [ ] **Time codes:** in the dashboard **Time codes** card, generate a 20-min code. It must be
      **six characters** — check it is comfortable to read off the screen and retype, since that is
      the whole reason it is six rather than eight, and note that `I`, `L`, `O` and `U` never appear
      so nothing can be mistyped into a different working code. Then on `/ask`, enter it under
      **Have a code?** → "Added 20 minutes!" and today's budget rises; the code disappears from the
      active list, and re-entering it says "not valid" (single-use). A random wrong code is
      rejected. Enter six wrong codes quickly: the sixth must be refused by the rate limiter rather
      than merely rejected — that throttle is what makes a six-character code safe. (As HIM, `type C:\ProgramData\HostHealth\time_codes.jsonl` →
      Access denied — he can't read the code list.)
- [ ] **Today's screen time** card shows minutes used/remaining and a progress bar that grows as
      he uses the PC; a **+30** bonus button raises the remaining figure immediately.
- [ ] **Per-day budgets:** tick "Different limit each day", set today to 1 min and another day to
      120, Save → enforcement uses today's 1-min value (locks quickly); the card's budget matches.
- [ ] **Pause toggle:** flip **Enforcing → Paused** → the card shows a "Paused" badge and, with a
      tiny daily limit set, the screen no longer locks; flip back → enforcement resumes.
- [ ] **Routines:** with some rules set, in **Routines** type a name (e.g. `Weekend`) and **Save
      current as routine**. Change the daily limit, then **Apply** `Weekend` → the settings revert
      to the saved preset (and the paused/enforcing toggle is left as-is). **Delete** it afterward.

## E3. Tamper resistance (do these as HIM — the point is that they don't work)

- [ ] **Time-zone change can't reset the budget.** With a small daily limit and some minutes
      already used, as HIM open Settings → Time & language → Date & time and change the **time
      zone** to somewhere ~12 hours away (this needs no admin rights — that's why it's a test).
      Check **Today's screen time** in the dashboard: the used-minutes figure must be **unchanged**.
      Change it back. `type C:\ProgramData\HostHealth\service.<date>.log` as admin should show a
      line about the offset being ignored.
- [ ] **Time-zone change can't dodge curfew.** With a curfew window covering *now*, shift the time
      zone so local time falls outside it. Within ~30s the shutdown must still be issued.
      (Cancel with `shutdown /a` as admin once you've seen it, then restore the zone.)
- [ ] **A *one-hour* time-zone change can't dodge curfew either — this is the one that used to
      work.** The check above uses a large jump, which the old offset comparison already caught.
      The reachable attack was a small one. With a curfew window that has just opened, as HIM set
      the zone to **one hour west** of your own (from Amsterdam: *(UTC+00:00) Dublin, Edinburgh,
      Lisbon, London*). Local time now reads an hour earlier — outside the window. **The shutdown
      must still be issued within ~30s.** Before the `tz_zone` identity check this bought a free
      hour every night, and two hours in summer.
- [ ] **Selecting plain UTC in summer can't dodge curfew.** The worst case, and only reproducible
      between March and October. Same setup, but choose *(UTC) Coordinated Universal Time*. That is
      two hours behind Amsterdam summer time, and the old check tolerated it because it compared
      against a winter-recorded anchor. The shutdown must still be issued.
- [ ] **Unticking "adjust for daylight saving time automatically" can't dodge curfew.** Same
      settings page, one checkbox down; it moves the clock an hour without changing the zone name.
      The shutdown must still be issued. (Re-tick it afterwards.)
- [ ] **A real machine reads its own zone identity at all.** The three checks above all pass
      trivially if `GetDynamicTimeZoneInformation` returns nothing, because the service then falls
      back to the old offset rule — so confirm the mechanism is live rather than merely
      unexercised. As admin, `type C:\ProgramData\HostHealth\config.json` and confirm **`tz_zone`
      is present and names your zone** (e.g. `"W. Europe Standard Time"`). If it is `null` after a
      fresh `install`, the Win32 call is failing and none of the above is actually being tested.
- [ ] **An honest DST transition is still followed.** The identity check means the OS clock is now
      believed outright while the zone matches, so this must keep working. Easiest proof without
      waiting for October: set the zone to one that transitions on a different date, leave it as the
      *recorded* zone by re-running `install`, and confirm the dashboard's day boundary and curfew
      still track local time. Otherwise, check it on the last Sunday in October.
- [ ] **Win+L doesn't earn more time.** Set a tiny daily limit with action **Lock**. When the
      warning appears, press **Win+L** before it locks, wait ~40s, then log back in. It must lock
      again **immediately** — not give another warning countdown. Repeat twice; it must lock every
      time. (After a parent grant, the *next* time he goes over he should get a fresh warning —
      the guard shouldn't make it feel punitive once more time has been given.)

- [ ] **The coverage check kept outside this repository.** Its steps are in
      `docs/private/OPERATIONAL-FINDINGS.md`; work through them from that file. It is the
      highest-value check on this list, and the only one whose answer decides whether
      everything else here holds. See O5 in `OPEN-FINDINGS.md` for why it is held back.
## E4. Enforcement is visibly alive

- [ ] **The dashboard proves enforcement is running.** With the service up, the **Today's screen
      time** card shows no warning banner.
- [ ] **The banner appears when the dashboard loses the service** — and this *is* stageable now,
      unlike in earlier versions. Open the dashboard on your phone and leave it open. As admin,
      `sc stop HostHealthService`. Within a minute (the page refreshes itself on a timer) the card
      must show **"Enforcement may not be running. The dashboard could not reach the service to
      ask."** Start the service again and the banner must clear on the next refresh.
      **If the banner does not appear, that is the bug this release fixed reappearing**, and it is
      the one worth reporting: until now the page read "no answer" as a good answer and stayed
      silent, which is exactly backwards for the warning that matters most.
- [ ] **A stale-but-alive enforcer also shows it.** Harder to stage — it needs the service up with
      its background loops wedged — so treat this as knowing the wording rather than a step to
      perform: the same banner reads **"No check-in for N min."** instead. Any of the three
      messages means the limits below it may not be being applied.
- [ ] **Sleep doesn't burn the day's budget.** With a daily limit set and some minutes used, note
      the figure, sleep the PC for 30+ minutes, wake it, and check **Today's screen time** within a
      minute. The used-minutes figure must have grown by at most a minute or two — not by the whole
      time it was asleep.

## E5. The narrowed process scan (Windows-only code that has never run)

Every thirty seconds, forever, the enforcer asks Windows what is running. That call was rewritten to
stop gathering four things it immediately discarded — CPU share, disk-I/O counters, memory, and the
full executable path for every process on the machine. It now asks for the pid and the name.

**Why this section exists.** The rewritten call is `#[cfg(windows)]`, so **it has never executed
anywhere** — not here, not in CI. It compiles clean for the target under `clippy -D warnings` and it
is covered by tests through the fake, which proves the shape and nothing about the behaviour. It is
also on the enforcement path: if it returns fewer processes than really exist, apps that should be
killed simply are not there, and **the dashboard would look completely normal while that happened**.
That is the failure mode worth a few minutes.

- [ ] **Blocked apps are still killed.** With an app on the blocklist, start it as him. It must
      disappear within about thirty seconds, same as before this change. This is the check that
      matters: it is the one that fails if the scan came back short.
- [ ] **Per-app limits still accrue.** Set a two-minute limit on something harmless (Notepad), run
      it as him for three minutes, and confirm **Today's screen time** shows the app at its limit
      and the app is closed. Accrual reads the same scan.
- [ ] **The parent's process panel still shows memory.** Open **Running apps** on the dashboard.
      Every row must show a memory figure — `42.9 MB`, not `0 B` or blank. That panel deliberately
      uses a *different*, richer call than the enforcer's; a blank column means the two were
      collapsed into one and the cheap scan is being asked for the expensive number.
- [ ] **Nothing got slower.** Watch `HostHealthService` in Task Manager for a minute or two. CPU
      should sit at or near zero between ticks. If anything, this change should have made it
      quieter — if it is *busier* than you remember, say so, because that is the opposite of what
      it was for.
- [ ] **An elevated app is still seen.** Run something as administrator and confirm it appears in
      **Running apps** and is still subject to its limit. Losing sight of elevated processes would
      be an evasion route, not a cosmetic bug.

## F. Resilience

- [ ] **Signing in survives a restart:** with the dashboard open and logged in on your phone,
      as admin run `taskkill /f /im host-health.exe` (it auto-restarts); reload the dashboard →
      you are **still signed in** (no password re-entry). Then **Log out** and restart the service
      again → you stay logged out (a logout must not come back).
- [ ] Auto-restart: as admin, `taskkill /f /im host-health.exe` → within a few seconds
      `sc query HostHealthService` shows RUNNING again (recovery).
- [ ] Reboot persistence: restart the PC, log **him** in → the dashboard is reachable again
      without anyone launching anything.

## G. Update & uninstall

- [ ] Re-run `nestwatch.exe install` (as admin) → it stops the service, updates the binary,
      restarts; your **port, curfew, and rules are preserved**, you set the password again.
- [ ] **Pre-flight does not refuse the upgrade.** This is the one to watch on the run above: with
      the service **running**, pre-flight must not report `port 8443 is already in use`. It used
      to — the port was held by the copy being replaced — and that refused every in-place upgrade.
      Verified by unit test, but only the real service proves the service-state read works.
- [ ] **A conflict on a *different* port still blocks.** With the service running normally on
      8443, run `install --port <a port something else is using>`. It must still refuse: the
      exemption above is only for the port our own service holds.
- [ ] **A refused install after an accepted fix tells the truth.** If pre-flight offers a fix, you
      accept it, and the install still stops on another blocker, the closing line must read
      "Apart from the fixes you accepted, nothing has been changed" — not "Nothing has been
      changed on this machine", which would be false.
- [ ] `nestwatch.exe uninstall` → service gone (`sc query` → 1060 does not exist), firewall rule
      removed, `C:\Program Files\HostHealth` removed. The data dir remains (config, cert, and the
      usage/time-request/budget-state files).
- [ ] `nestwatch.exe uninstall --purge` → also removes `C:\ProgramData\HostHealth` (all of it).
- [ ] **Uninstall → immediately reinstall** (and try it once with the Services window open): if
      it fails, the message must explain that a previous copy is still being removed and to close
      Services/Task Manager — not show a bare "os error 1072".
- [ ] **A corrupt config isn't silently discarded.** As admin, back up `config.json`, replace it
      with garbage (`echo not-json > config.json`), and run `install`. It must **refuse** and say
      your curfew/rules/routines would be reset. Restore the backup and confirm a normal reinstall
      still preserves them.
- [ ] **A failed update doesn't leave enforcement off.** Hard to stage deliberately; if an update
      ever does fail, confirm `sc query HostHealthService` still shows RUNNING (it restarts the
      previous version) and that the output says so.

### G1a. The resident helper no longer blocks update or uninstall

**Do every check in this block with HIM signed in** (fast-user-switch to your admin account rather
than signing him out). That is the entire point: with nobody logged in there is no helper, and all
of these pass whether or not the fix works. This is why the underlying bug survived — it cannot
reproduce on an idle test machine.

- [ ] **The helper is there to begin with.** Task Manager → Details, with *Command line* shown:
      exactly one `host-health.exe` running as HIM with `helper --watch`. Note its PID.
- [ ] **An in-place update actually replaces the binary while he is signed in.** Note the
      *Modified* timestamp on `C:\Program Files\HostHealth\host-health.exe`, then run `install` from
      a newer build. It must print `Stopped 1 resident watcher process(es) holding the binary open`,
      and the timestamp **must change**. Before this fix the copy failed with a sharing violation,
      the previous service was restarted, and the update silently did not apply.
- [ ] **The old helper is gone and exactly one new one replaced it.** After the update, Task Manager
      shows one `helper --watch`, with a **different PID** from the one you noted.
- [ ] **Stopping the service leaves no helper behind.** `sc stop HostHealthService`, wait ~40 s
      (longer than one 30-second emit), and confirm **no** `host-health.exe` is running as HIM. It
      used to survive forever, holding a desktop hook and writing into a dead pipe.
- [ ] **Restarting the service twice does not accumulate helpers.** `sc stop` / `sc start`, twice.
      Exactly one `helper --watch` at the end, never three.
- [ ] **Uninstall completes with him signed in.** `uninstall` must remove
      `C:\Program Files\HostHealth` **and print `Uninstalled.`** — the whole directory gone, not a
      note about a file it could not delete.
- [ ] **A blocked uninstall fails loudly.** Stage it: open an elevated prompt, `cd` *into*
      `C:\Program Files\HostHealth` (a current directory holds it open), and run `uninstall` from
      elsewhere. It must **exit non-zero**, name the directory it could not remove, quote the OS
      error, and tell you what to do. It must **not** print `Uninstalled.` — reporting success on a
      partial uninstall is the failure this check exists for.
- [ ] **Running the installed copy does not kill itself.** Run
      `C:\Program Files\HostHealth\host-health.exe install` — i.e. the installed binary installing
      over itself. It must complete normally, not terminate its own process.
- [ ] **A service still marked for deletion is reported now, not next time.** Open `services.msc`
      and leave it open (that holds a handle), then run `uninstall`. It must fail naming *the
      service registration (marked for deletion)* and tell you to close Services. Close it, re-run
      `uninstall` → clean. Without this the symptom appears one step later, as a bare
      `os error 1072` on the *next* install, which is where it used to be met.
- [ ] **A surviving firewall rule is caught.** `netsh advfirewall firewall show rule
      name=HostHealthService` after a successful uninstall must report no match. The uninstall
      verifies this itself now — deletion was previously fire-and-forget, so a failed `netsh` left
      the rule open and said nothing.

### G1b. The install remembers which version it was

- [ ] **First install writes the record.** `C:\ProgramData\HostHealth\installed.json` exists and
      names this build's version.
- [ ] **A second install of the same build says so.** Re-run `install` → `Reinstalling <v> over
      itself.`
- [ ] **An update names both versions.** Install a newer build → `Updating <old> -> <new>.`
- [ ] **A downgrade warns rather than proceeding quietly.** Install an *older* build over a newer
      one → a `WARNING:` naming both versions. It still installs; the point is that it says so.
- [ ] **A corrupt record is not read as a fresh machine.** `echo not-json > installed.json`, run
      `install` → it must print the `NOTE: an install record exists but could not be read` line, not
      stay silent as it does for a genuine first install. Silence there would mean a future data
      migration gets skipped on exactly the machines most likely to need it.
- [ ] **Uninstall takes the record with it**, even without `--purge`: after `uninstall`,
      `installed.json` is gone while `config.json` remains.
- [ ] **`doctor` notices a binary that was copied but never installed.** This is the one worth doing
      properly, because it is how someone concludes a fix is present when it is not. Copy a *newer*
      build to the Desktop and run `doctor` **from the Desktop copy**, elevated. It must warn that
      this binary is newer than the installed service and tell you to run `install` — not report a
      clean machine. Then run `install` and confirm `doctor` says the versions match.

---

## G2. Remote administration (only if you intend to use it)

Skip this whole section unless you plan to update the PC over the network. It opens an
administrative way in, and **none of it is safe while his account is a local administrator** —
check §0 first. Background: [REMOTE-UPDATE.md](REMOTE-UPDATE.md).

- [ ] **`doctor` says nothing about remoting when it is off.** Before enabling anything, run
      `nestwatch.exe doctor` and confirm there is no mention of ports 5985 or 5986.
- [ ] **The generated script is readable and complete.**
      `.\host-health.exe remote-setup > setup.ps1`, then open it. It must name **this PC** in
      `-DnsName` (not a placeholder), and contain the elevation check, the HTTPS listener, the
      deletion of the plaintext one, and the verification step.
- [ ] **It refuses to run un-elevated.** Run `.\setup.ps1` from a *normal* PowerShell → it must
      throw *"Run this in an elevated PowerShell"* and change nothing.
- [ ] **It completes elevated** and prints a certificate thumbprint and the export path.
- [ ] **Step 4 does not stall.** Time it: `Measure-Command { .\setup.ps1 }`, or just watch. The
      firewall step selects rules starting from the port filters; the obvious form runs one query
      per rule and can sit for a minute or more. If step 4 looks hung, **do not Ctrl-C** — step 1
      has already opened the plaintext listener that step 3 closes, so an abort there is the worst
      outcome available. Let it finish, then report the timing.
- [ ] **The script refuses to finish if a plaintext rule survived.** It now checks the firewall,
      not just the listeners. To see the check work, re-enable a 5985 rule
      (`Get-NetFirewallPortFilter -Protocol TCP | Where-Object { $_.LocalPort -eq 5985 } |
      Get-NetFirewallRule | Set-NetFirewallRule -Enabled True`) and re-run: it must throw and name
      the rules. Then re-run normally and confirm it passes.
- [ ] **Plaintext remoting is genuinely gone.** `winrm enumerate winrm/config/Listener` shows
      **HTTPS only**. Then, importantly, run `nestwatch.exe doctor`: it must **not** report
      *"listening WITHOUT encryption"*. If it does, stop and do not use remoting.
- [ ] **`doctor` notices remoting is on.** It should now warn *"remote management is enabled
      (HTTPS, port 5986)"* — the reminder that this is a way in you left open.
- [ ] **A remote session works, without a `-Skip` flag.** From your laptop, after importing the
      exported certificate and comparing its thumbprint:
      `New-PSSession -ComputerName <PC> -UseSSL -Credential (Get-Credential)`. If it only works
      with `-SkipCACheck`, the certificate trust step did not take — fix that rather than skipping.
- [ ] **It does not disturb him.** While the session is open, confirm the PC's screen is
      unchanged and he is not signed out. (This is the reason for remoting over Remote Desktop.)
- [ ] **An update over that session works end to end:** copy the new `.exe` with
      `Copy-Item -ToSession`, run `install`, then `version` and `doctor` — following the sequence
      in [REMOTE-UPDATE.md](REMOTE-UPDATE.md).
- [ ] **Teardown really tears down.** `.\host-health.exe remote-setup --off > teardown.ps1` and
      run it. Then `nestwatch.exe doctor` must go back to reporting nothing about remoting, and
      `Get-Service WinRM` must show Stopped with StartupType Manual.

---

## Troubleshooting

- **Can't connect from another device** → network is likely "Public"; set it to Private, or
  confirm the rule: `netsh advfirewall firewall show rule name=HostHealthService`. Also confirm
  both devices are on the same subnet (the rule is `remoteip=LocalSubnet`).
- **Screenshot is black / "no active console session"** → no user is logged in at the physical
  console (RDP / fast-user-switching / lock screen aren't captured). Log in at the machine.
- **Lock does nothing under the service** → same root cause as a black screenshot: the lock is
  launched into the active console session via the helper, so it needs a user logged in at the
  physical console. (In dev `run` mode it locks directly.)
- **Install stops with "must be run from an elevated console"** → exactly that: right-click
  PowerShell/Command Prompt → *Run as administrator*, then re-run. `install`/`uninstall` check for
  elevation before touching anything, so a non-elevated run fails fast and leaves nothing behind.
- **Install fails "Access is denied (os error 5)" writing config.json** (older builds, or if the
  check is bypassed) → the data dir got ACL-locked to Administrators and then the write ran under a
  non-elevated token. Fix: run from an **elevated** console. If a locked, empty
  `C:\ProgramData\HostHealth` was left behind, remove it first (elevated): `rmdir /s /q
  C:\ProgramData\HostHealth`.
- **"must be run from an elevated console" even in an Administrator window** → the account itself
  isn't an admin. Confirm with `net localgroup Administrators` (your username must be listed), or
  use an admin account.
- **Install fails "icacls … refusing to continue"** → the ACL step failed for another reason; the
  step is intentionally fatal so a half-hardened install never claims success.
- **`sc stop` works as him** → his account is an administrator. Make it a standard user
  (Settings → Accounts, or `net localgroup Administrators <user> /delete`).
