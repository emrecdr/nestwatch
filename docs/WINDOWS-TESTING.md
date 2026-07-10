# On-device Windows test checklist

The cross-platform logic is covered by automated tests + CI. This checklist covers the
**Windows-only runtime behavior** that can only be verified on the real machine: the SYSTEM
service, the `CreateProcessAsUser` session helper, ACL hardening, and the firewall rule.

Run through it once on his PC after installing.

---

## 0. Prerequisites

- [ ] **His account is a *standard* user, not an administrator.** Check:
      `net localgroup Administrators` — his username must **not** be listed. (Tamper-resistance
      is meaningless against a local admin.)
- [ ] **The PC's network is set to *Private*** (Settings → Network → properties). The firewall
      rule is scoped to `private,domain`; on a "Public" network it won't match.
- [ ] You have `nestwatch.exe` (from CI artifact, a release tag, or a cross-build).
- [ ] You're at an **elevated (Administrator) console** for install/uninstall.

## A. Install

- [ ] `nestwatch.exe install` (or `install --port <N>`) completes and prints a **TLS SHA-256
      fingerprint** — write it down.
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
      (`audit.jsonl`, `usage.jsonl`, `time_requests.jsonl`, `usage_state.json`, plus `.jsonl.1`
      rotation backups) are unreadable and undeletable by the child.
- [ ] Cannot modify/delete the binary: `del "C:\Program Files\HostHealth\host-health.exe"`
      → **Access is denied**.
- [ ] In Task Manager → Details, `host-health.exe` runs as **SYSTEM**; "End task" → Access denied.

## C. Remote access, cert, login (from your phone/laptop on the same Wi-Fi)

- [ ] Browse to `https://<his-pc-ip>:<port>` — it **loads** (proves the firewall rule works; if
      it times out, see Troubleshooting).
- [ ] Browser shows a one-time "not trusted" warning. View the cert → its **SHA-256 matches**
      the fingerprint from step A. Proceed.
- [ ] Login page shows the bland **"Host Health"** header (not "Nestwatch").
- [ ] Wrong password → rejected; after ~5 quick wrong tries → **locked out** briefly (429).
- [ ] Correct password → dashboard shows **"🪺 Nestwatch"**.

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

## E. Curfew (the enforcement feature)

- [ ] Set a window that includes **now** (e.g. now-1min → now+10min), warn = 60s, Save.
- [ ] Within ~30s the PC shows the shutdown countdown. **Cancel the test:** disable curfew in the
      dashboard → within ~30s the pending shutdown is **aborted** (no power-off). Verify:
      `shutdown /a` as admin should say "no shutdown in progress" (we already aborted it).
- [ ] **Re-assert test:** re-enable the covering window; when the countdown starts, as HIM run
      `shutdown /a`. Within ~30s the countdown should **restart** (curfew re-issues it). This is
      the anti-bypass behavior.
- [ ] Set a normal bedtime window (e.g. 22:00→07:00) and leave it for real use.

## E2. Screen-time rules & time requests

- [ ] In **Screen-time & app limits**, set a tiny **Daily limit** (e.g. 1 min), action **Lock
      screen**, warn 30s, Save. After ~1–2 min of use the screen **locks** (and re-locks if he
      unlocks). Set it back to 0 (off) afterwards.
- [ ] Add a **Blocked app** (e.g. `notepad.exe`), Save; launch Notepad → within ~30s it's
      **killed**. Remove it afterwards.
- [ ] Add a **Per-app limit** (e.g. `notepad.exe` = 1 min), Save; run Notepad → after ~1 min
      it's killed on sight, while other apps keep running. Remove it afterwards.
- [ ] **Budget survives a restart:** with a small daily limit set, accrue a little usage, then
      `taskkill /f /im host-health.exe` (it auto-restarts) → the used-minutes tally is **not**
      reset (it's persisted in `usage_state.json`); enforcement resumes from where it was.
- [ ] From his browser, open `https://<his-pc-ip>:<port>/ask`, request e.g. 15 minutes →
      you see it under **More-time requests** in the dashboard → **Approve** → the granted
      minutes are added to today's budget (and appear in **Usage history**).

## F. Resilience

- [ ] Auto-restart: as admin, `taskkill /f /im host-health.exe` → within a few seconds
      `sc query HostHealthService` shows RUNNING again (recovery).
- [ ] Reboot persistence: restart the PC, log **him** in → the dashboard is reachable again
      without anyone launching anything.

## G. Update & uninstall

- [ ] Re-run `nestwatch.exe install` (as admin) → it stops the service, updates the binary,
      restarts; your **port, curfew, and rules are preserved**, you set the password again.
- [ ] `nestwatch.exe uninstall` → service gone (`sc query` → 1060 does not exist), firewall rule
      removed, `C:\Program Files\HostHealth` removed. The data dir remains (config, cert, and the
      usage/time-request/budget-state files).
- [ ] `nestwatch.exe uninstall --purge` → also removes `C:\ProgramData\HostHealth` (all of it).

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
- **Install fails "icacls … refusing to continue"** → run from an elevated console; the ACL step
  is intentionally fatal so a half-hardened install never claims success.
- **`sc stop` works as him** → his account is an administrator. Make it a standard user
  (Settings → Accounts, or `net localgroup Administrators <user> /delete`).
