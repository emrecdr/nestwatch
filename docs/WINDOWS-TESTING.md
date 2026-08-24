# On-device Windows test checklist

The cross-platform logic is covered by automated tests, and CI runs the Windows tests on a real
`windows-latest` runner. This checklist covers what neither can reach: behaviour that needs
**privileges, a logged-in user, or the machine's own configuration**. That means the SYSTEM
service and SCM restart behaviour, the `CreateProcessAsUser` session helper, ACL hardening, the
firewall rule and network profile, WTS session state, recovery-mode boot paths — and the
handful of things that
depend on a **real browser** rather than a test client, such as the origin check in §C.

Run through it once on his PC after installing.

## Short on time? Do these seven first

The full list is 104 items, which is why it keeps not happening. These seven are the ones whose
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
4. **Lock actually locks the screen** (§D) — same mechanism as the screenshot, opposite direction.
   If screenshots work and lock doesn't, that narrows the fault sharply.
5. **He cannot stop the service or read the data directory** (§B) — two commands as him. This is
   the tamper-resistance claim; everything else assumes it holds.
6. **Every dashboard card still works from a real browser** (§C) — the origin check
   fails *silently*, as buttons that do nothing rather than an error. A test client cannot catch
   this; only a browser can.
7. **A day the PC was off shows as "not measured", not as a zero** (§D) — if those two states
   look alike, a stopped enforcer reads exactly like a well-behaved week, which is the failure the
   feature exists to prevent. The chart *has* now been seen rendered, in Chrome on macOS with
   seeded data covering all three states — which is how the bug that made it draw nothing at all
   through 0.2.3 was found. It has still never been seen on Windows, with real data, on a phone.

Everything below is worth doing eventually. Nothing below is worth doing before these.

**One addition since this list was written.** Foreground tracking (§D2) is new code that has never
executed anywhere — not on this machine, not on any machine. It is not in the seven above because
nothing depends on it: it measures and reports, it never enforces, so if all of it is broken the
locks and limits behave exactly as they do today. That is also why it is safe to leave until after
the seven. But it is the largest untested surface in the build, so when you do reach it, expect
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
- [ ] **Live** toggle on the Screen card → the screenshot refreshes every few seconds without
      clicking; toggling it off (or logging out) stops the refresh.
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

- [ ] **`nestwatch helper --watch` is running as him.** Task Manager → Details, while he is signed
      in. If it is absent, nothing below can pass and the report will show no focused minutes at
      all — which correctly renders as *not measured* rather than as zero.
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
- [ ] **Time codes:** in the dashboard **Time codes** card, generate a 20-min code → on `/ask`,
      enter it under **Have a code?** → "Added 20 minutes!" and today's budget rises; the code
      disappears from the active list, and re-entering it says "not valid" (single-use). A random
      wrong code is rejected. (As HIM, `type C:\ProgramData\HostHealth\time_codes.jsonl` →
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
