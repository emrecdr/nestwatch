# Nestwatch

**Screen-time limits, a bedtime curfew and remote control for a child's Windows PC — from any
device on your own home network. No cloud, no accounts, no telemetry, no keylogging.**

[![build](https://github.com/emrecdr/nestwatch/actions/workflows/ci.yml/badge.svg)](https://github.com/emrecdr/nestwatch/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/emrecdr/nestwatch?label=release)](https://github.com/emrecdr/nestwatch/releases/latest)
[![downloads](https://img.shields.io/github/downloads/emrecdr/nestwatch/total?label=downloads)](https://github.com/emrecdr/nestwatch/releases)
[![license](https://img.shields.io/github/license/emrecdr/nestwatch)](LICENSE)

[![platform](https://img.shields.io/badge/platform-Windows%2010%2F11-blue)](#requirements)
[![rust](https://img.shields.io/badge/rust-1.96.0-orange?logo=rust)](rust-toolchain.toml)
[![dependencies audited](https://img.shields.io/badge/dependencies-cargo--deny%20every%20push%20%2B%20weekly-success)](.github/workflows/ci.yml)

**[Install guide →](https://emrecdr.github.io/nestwatch/)** · [Releases](https://github.com/emrecdr/nestwatch/releases/latest) · [Security model](docs/SECURITY.md) · [Known limits](docs/OPEN-FINDINGS.md) · [Remote update](docs/REMOTE-UPDATE.md) · [Off-LAN access](docs/REMOTE-ACCESS.md)

---

## What it is

One self-contained Rust binary. It installs as a background service on the child's PC and serves
a small web dashboard over HTTPS to your phone or laptop **on the same Wi-Fi**. You set a daily
minute budget and a bedtime; it enforces them and shows you what actually happened.

Nothing leaves the house: no account to create, no vendor to trust, no data sent anywhere.
The monitored PC makes no outbound connection at all — it listens, and nothing else.

The single exception is worth stating precisely rather than hiding behind that sentence: the
dashboard has a **"check for a newer version"** button. Pressing it fetches the release list
**from the device you are reading the dashboard on** — your phone or laptop — not from the
child's PC, which still contacts nobody. Nothing is fetched when the page loads, and the check
sends nothing about your household beyond the request itself. Ignore the button and this
software never touches the internet.

**The two controls that hold** are the daily budget and the curfew — they cannot be dodged by
renaming a file or rebooting. App blocklists are habit-shaping rather than a wall, and the README
says so below rather than letting you find out later.

## Quick start

On the child's PC, from an **elevated** PowerShell:

```powershell
# 1. Download nestwatch.exe + nestwatch.exe.sha256 from the latest release, then verify:
Get-FileHash nestwatch.exe -Algorithm SHA256 | Format-List   # must match the .sha256 file
gh attestation verify nestwatch.exe --repo emrecdr/nestwatch # stronger: proves who built it

# 2. Right-click the .exe -> Properties -> tick "Unblock", then:
.\nestwatch.exe install     # sets the password, makes a TLS cert, prints a QR code

# 3. Scan the QR with your phone. You are in the dashboard, signed in.
nestwatch.exe doctor         # anything wrong? this says what, and how to fix it
```

Full walkthrough, including what to check afterwards: **[the install guide](https://emrecdr.github.io/nestwatch/)**.

## Requirements

- **Windows 10 version 1903 (build 18362) or newer**, or Windows 11, on the managed PC. The
  child's account must be a **standard user**, not an administrator — see
  [Tamper-resistance](#tamper-resistance--and-its-limits).
  <br>The floor is the screen capture: it uses an API that arrived in 1903. Everything else —
  screen-time limits, curfew, blocked apps, per-app limits — runs on older builds, and `install`
  reports the mismatch as a caution rather than refusing. Any Windows 10 still receiving updates
  is well past this.
- Any device with a browser on the same home network, for the parent.
- Nothing else. No runtime to install, no Node, no Python, no service account.

> "Nestwatch" is the parent-facing project name. The service, folders and files the installer
> creates on the managed PC are named plainly and unremarkably instead; `nestwatch.exe doctor`
> lists exactly what is installed and where.

## Features

Everything below is in the dashboard (the child sees only their own page at `/ask`). Verify each
one on the real machine with **[`docs/WINDOWS-TESTING.md`](docs/WINDOWS-TESTING.md)**.

| | |
|---|---|
| **Daily budget** | Minutes per day, optionally different per weekday. Counts only *active* use — not idle, locked or logged-out time. Survives reboots, resets at midnight. When spent: lock, shut down, or warn only. |
| **Curfew** | One or more time windows per weekday, **separate from the budget — granting extra screen time does not move bedtime.** Counts down on the child's screen, then shuts down — and re-issues if the shutdown is cancelled. When you want a later night, **Later bedtime tonight** (+15/+30/+60 on the Curfew card) pushes tonight's window back and then hands it straight back; it survives a reboot and needs no undoing. Because the two limits are independent, **each one tells you when the other will override it**: push bedtime back with the day's screen time already spent and it says the PC will still lock anyway; grant screen time during a window and it says bedtime will still shut it down. Neither reports a silent success it cannot deliver. |
| **Warnings** | 15, 5 and 1 minutes before both the budget and bedtime, so the limit is never a surprise. A budget shorter than a threshold never announces it; a mid-day restart doesn't replay warnings; granting extra time re-arms them. |
| **Screen-time report** | 7, 30 or 90 days as a chart, with a comparison against the period before. Click a column to drill into that day. Most-used lists cover the whole window — by app, by category, by time actually in front, and by browser page — so it answers "how much Roblox this month", not only "what happened last Tuesday". **Known game portals are badged by name** (now.gg, Poki, CrazyGames and the like) wherever page titles are listed; no badge means nothing was recognised, not that nothing was played. Days the service wasn't running show as **not measured**, never as zero, so a stopped enforcer can't look like a quiet week. **New apps are called out** — anything used for the first time, with the number of days of history behind the claim. |
| **Asking for more** | The child's page shows time left and can request more; you approve or deny. The countdown warnings on their desktop carry the address, so they know where to ask — except during a curfew window, where extra time cannot help and offering it would be a promise the tool can't keep. Single-use offline codes cover times you're away or the network is down. |
| **Remote control** | Screenshot the desktop (with live refresh at your choice of 2/5/15s), list and kill running apps, lock the screen, shut down with a warned countdown. Live frames are small and cheap; clicking **Expand** fetches a full-resolution one and keeps it full for as long as the big view is open, so a picture you opened to *read* stays readable. Your click always takes priority over a frame already being fetched. **Windows draws a yellow border around the screen while it is being captured** — the child can see when you are looking. |
| **App rules** | Blocklist, per-app daily limits, and groups sharing one pool. Habit-shaping, not a wall — see [Not included](#not-included). |
| **Modes** | Pause the whole rules enforcer with one toggle for a free evening (curfew still applies). Save the current setup as a named routine and reapply with a click. |
| **Routines that run themselves** | A routine can carry a **schedule** — "16:00 to 18:00, Mon–Fri" — and applies itself while that window is open, then hands back to your normal settings when it closes. This is how you say *"games are blocked during homework hour, but the PC stays on"*, which the curfew cannot express: bedtime's only move is to power the machine off. **Nothing is overwritten.** A schedule chooses which settings are in force at each moment rather than writing over your defaults, so your normal rules are still there when the window ends, editing them mid-window is not silently reverted, and there is no timer quietly rewriting your config. **Pause still beats everything** — an install you paused stays paused, even inside a scheduled window — and where two schedules overlap, the one higher in the list wins. Routines you already have keep working exactly as before: no schedule means manual-only, which is what every existing routine loads as. |
| **Integrations** | Apps that earn screen time, which you install and switch off from the dashboard. A paired app reports only *that* its threshold was met — **how many minutes that earns is set here, on this PC, per integration** — so a phone that is lost, spoofed or simply buggy cannot choose its own reward, and a report claiming 999 minutes still grants exactly what you configured. An integration you switch off cannot grant at all, in one toggle, without unpairing anything. It is data you toggle, never code this machine runs or a server it reaches out to — see [`docs/PLUGIN-SYSTEM.md`](docs/PLUGIN-SYSTEM.md). |
| **Refused today** | The limits don't only hold — you can see them holding. When the PC's clock is moved to shift the day boundary, when something tries to start the day over and wipe the tally, or when a shutdown is cancelled with `shutdown /a`, Nestwatch declines it and now **says so on the dashboard**. Until this, every one of those refusals went to a log file that needs an Administrator console on the child's PC to read — the record existed exactly where you couldn't reach it. The card appears only when there is something to show, which is not most days, and it survives a reboot. It reports **what the tool did, not what anyone meant by it**: a family that really crossed a time zone produces the same count as a clock moved on purpose, so it says "clock change ignored — screen time and bedtime kept using the trusted time" and leaves the intent to you. That wording is why it's safe to show your child too. |
| **Trust the setup** | `nestwatch doctor` reports whether the service is up, the port listening, the firewall rule right, the network private, the certificate valid, whether this Windows is new enough to take screenshots at all, and whether anything is actually being enforced. Every problem prints its fix. |
| **Dutch for the child** | The child's own surfaces — their `/ask` page and every notice that reaches their desktop: the remaining-time countdowns, the bedtime warning, the lock warning, and the notice Windows shows as it powers the machine off — can be set to Dutch; the parent's dashboard stays English. Set by the parent (`POST /api/language`), not detected from the browser: `Accept-Language` is set in the child's own browser, and the child does not get to choose the language of the notice telling them what is being watched. Defaults to English, so an install that never sets it is unchanged. |
| **Take your data with you** | `GET /api/export` returns every screen-time rollup this install still holds, as a downloadable JSON file. Verbatim — nothing reconciled, nothing filtered — so it can be checked against what the dashboard shows rather than merely believed. It is the copy to take before `uninstall --purge`, which deletes all of it and cannot be undone. **History has a ceiling.** The fact: each log keeps two generations of 2 MiB, and rotation deletes the older one — there is no retention setting, and nothing warns you when the oldest days fall off. How long that takes is **not** a property of the tool; it depends on how many apps and pages your child uses in a day, because that is what sets the size of a daily row. Modelled across both generations, two independent estimates agreed on the shape and differed by ~40% on the assumed name lengths: order of **decades** for light use, and **roughly two to three years** for a child hitting the 40-page cap every day. Treat those as a range, not a number. |
| **Visibility** | Today's usage with per-app bars — and, where the watcher is running, which apps were actually *in front* today and what the browser was showing, refreshed about once a minute rather than waiting for tomorrow's summary. Plus usage history and an access log of logins with source IP. |

**Resists clock tampering.** Changing the PC's time zone — which Windows lets a standard user do
with no prompt — cannot reset the day's tally or move the curfew window. Real daylight-saving
changes are still followed.

## How it works

One binary. On Windows it installs as a **SYSTEM service** (Session 0) that serves the web
UI + JSON API over self-signed HTTPS and runs two background enforcers — **curfew** and the
**usage rules** (screen-time budget, app blocklist, per-app limits). Because Session 0 has no
desktop, **screenshots and screen-lock are delegated to a short-lived helper** launched into
the interactive user session. Per-app *foreground* time needs a second helper that **stays
running** for the length of your child's session — a desktop-scoped Windows hook cannot be
installed from Session 0 at all, so this one is forced by the platform rather than chosen; it
reports over a pipe and decides nothing (see
[FOREGROUND-TRACKING.md](docs/FOREGROUND-TRACKING.md)). All OS access sits behind a
`SystemControl` trait, so the whole app also builds, runs, and is tested on macOS/Linux via a
`FakeControl`.

```
Browser (LAN) ──HTTPS──> SYSTEM service (Session 0) ── axum ── auth (argon2 + session)
                          │  ├─ curfew enforcer  (window/day → 15/5/1-min warnings → shutdown)
                          │  ├─ rules enforcer   (screen-time budget / blocklist / app limits;
                          │  │                     counts active use only, 15/5/1-min warnings
                          │  │                     → kill · lock · shutdown)
                          │  ├─ processes / kill / shutdown         [direct, Session 0 OK]
                          │  ├─ screenshot + lock ─→ helper in user session (WTSQueryUserToken +
                          │  │                        CreateProcessAsUserW) ─→ xcap ─→ JPEG
                          │  └─ foreground watcher ─→ RESIDENT helper in the child's session
                          │                           (SetWinEventHook + 5s poll) ─→ JSONL ─→ pipe
                          └─ SystemControl trait ─→ ServiceControl │ WindowsControl │ FakeControl
```

| Layer | Crates |
|---|---|
| Web / TLS | axum 0.8, axum-server 0.8, rustls 0.23 (**ring** provider), tower-sessions 0.15 |
| Assets | rust-embed 8 (embeds `assets/`) |
| Auth | argon2 0.5 (Argon2id) |
| OS ops | xcap 0.9 (screen, Windows-only dep), sysinfo 0.39 (processes), `shutdown /s` (power), `rundll32 …LockWorkStation` (lock) |
| Session | `WTSQuerySessionInformation` (is the child logged in / locked / idle — screen-time counts active use only), `WTSSendMessage` (on-desktop "time's almost up" warning) |
| Service / FFI | windows-service 0.8, windows 0.62 (WTS + CreateProcessAsUser) |
| Time | chrono 0.4 (local-time curfew windows + daily screen-time reset) |
| Cert | rcgen 0.14 |
| UI | Alpine.js 3.16 **CSP build**, Tailwind CSS v4.3, daisyUI 5.7 (built to `assets/app.css`). The CSP build is what lets `script-src` be `'self'` alone — no `'unsafe-inline'`, no `'unsafe-eval'`. Follows the viewing device's light/dark setting, with a switch to override it. |

## Tamper-resistance — and its limits

The design resists a **standard (non-admin) user**, which is how parental control is meant
to work:

- The SYSTEM service can't be stopped or deleted by a standard user (Task Manager shows
  "Access Denied"); it auto-restarts on failure.
- The binary lives in `C:\Program Files\HostHealth\` and the config, cert, and logs in
  `C:\ProgramData\HostHealth\`, both ACL-hardened to SYSTEM + Administrators only — a standard
  user can't read the password hash / TLS key / audit + usage logs, or delete the files.
- Low-profile service name; no window or tray icon.

**Hard limits (stated honestly):**
- If the child is a **local administrator**, no software-only tool can reliably resist them.
  Make sure their account is a standard user.
- This intentionally does **not** use rootkit/process-hiding techniques — those trip
  antivirus and destabilize the machine. The service is visible in Task Manager; it just
  can't be stopped without admin rights.

## Security

The goal is simple: **only the parent, from a device on the home LAN, can reach the controls,
and every access is logged.** In addition to the tamper-resistance above, that means:

- **Two network gates** — the Windows Firewall rule (LocalSubnet only, checked at install)
  *and* an app-layer allowlist that rejects any off-LAN client, so a missing firewall rule
  doesn't equal exposure.
- **Per-IP login throttling** — a stranger spamming wrong passwords throttles only themselves,
  never locks the parent out.
- **An origin check on every request** — the login cookie alone can't tell this dashboard apart
  from a page served on a *different port of the same PC*, which your child can do. Requests are
  checked against the browser's own report of where they came from, so such a page can't operate
  the controls. Links, bookmarks and the pairing QR still work normally.
- **HTTPS with a verifiable fingerprint**, strict browser security headers, and Argon2id
  password hashing.
- **An access log** — logins (with source IP) and sensitive actions are recorded and shown in
  the dashboard's *Recent access* panel, so an unfamiliar sign-in is visible.

See [`docs/SECURITY.md`](docs/SECURITY.md) for the full threat model, the trust boundaries, and
how to verify your install.

## Build

```bash
# 1) Build the UI CSS (build-time only; no runtime Node dependency)
cd web && npm install && npm run build && cd ..

# 2a) Build for the host (dev)
cargo build --release

# 2b) Build the real Windows .exe — via CI (recommended) or cross-compile:
#     CI: .github/workflows/ci.yml, windows-latest job -> nestwatch.exe artifact
#     Cross from macOS (needs: rustup target add x86_64-pc-windows-gnu; brew install mingw-w64):
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
  cargo build --release --target x86_64-pc-windows-gnu
```

**Checking the Windows-only code from macOS/Linux.** Most of this crate builds and tests
anywhere, but everything behind `#[cfg(windows)]` — the service, the session helper, the ACL and
firewall work — is invisible to a host-target build. Run this before every commit that touches it;
it compiles all of it, including the test targets, without needing a Windows machine:

```bash
# Separate target dir: sharing one with the host build makes both rebuild from scratch each time.
CARGO_TARGET_DIR=target/win-cross \
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
  cargo clippy --target x86_64-pc-windows-gnu --all-targets -- -D warnings
```

`--all-targets` is the point — a plain `build` skips the test code, which is where most
`#[cfg(windows)]` breakage actually shows up. Note what this does **not** prove: that the code
*links* says nothing about privileges, ACLs, WTS session state, service-control semantics, or
timing. Every serious bug this project has had lived in exactly that gap, which is what
[`docs/WINDOWS-TESTING.md`](docs/WINDOWS-TESTING.md) exists for.

You download and run **`nestwatch.exe`** — it's the same binary that `install` copies to
`C:\Program Files\HostHealth\host-health.exe` (the bland on-disk name) on the target.

**Releases:** push a tag (`git tag vX.Y.Z && git push --tags`) and `.github/workflows/release.yml`
builds `nestwatch.exe` + a SHA-256 and attaches them to a GitHub Release. See
[`CHANGELOG.md`](CHANGELOG.md) for what's in each version.

## First run

The [install guide](https://emrecdr.github.io/nestwatch/) is the step-by-step version, written to
be read on a phone while you stand at the PC. The short form is in [Quick start](#quick-start)
above.

Afterwards, work through **[`docs/WINDOWS-TESTING.md`](docs/WINDOWS-TESTING.md)** on the real
machine. It opens with seven checks that take about fifteen minutes and are worth more than the
rest of the list combined — several things here (the screen helper, the ACLs, the firewall rule,
the origin check in a real browser) can only be verified there.

### If you forget the password

There is no reset link and no recovery email — there is no account and no vendor, which is the
point. **The way back in is to run `install` again** from an elevated console on the child's PC:

```powershell
.\nestwatch.exe install     # sets a new password; keeps everything else
```

Your curfew, screen-time rules, app limits, routines and granted extra time are all preserved
(`install` merges over the existing settings), and the TLS certificate is reused as long as it
still covers the machine — so **devices you have already paired will not warn again**, and you do
not need to re-pair them. Only the password changes.

Two things worth knowing before you need this. It requires being **at the PC, with an
administrator account** — so it is not something you can do from a hotel. And you will hardly ever
type this password, because signed-in devices stay signed in for 30 days of inactivity: that is
convenient, and it is exactly why the password is easy to forget. Put it in a password manager at
install time.

## Command reference

```powershell
nestwatch.exe install     # password + TLS cert; copies binary, registers & starts the
                          # SYSTEM service, hardens ACLs; prints a QR to pair your phone
nestwatch.exe doctor      # check the install; report anything wrong and how to fix it
#                           (including if this binary is newer than the installed service)
nestwatch.exe pair        # print a fresh QR to sign in another phone/laptop
nestwatch.exe fingerprint # re-print the TLS cert SHA-256 (to verify a new device later)
nestwatch.exe uninstall   # remove service, firewall rule and files; --purge also removes data
nestwatch.exe version     # print this build's version (also --version / -V)
nestwatch.exe remote-setup # print a script that enables remote admin (--off to undo)

# install also accepts:
#   --port <N>        listen on a different port
#   --fix             apply pre-flight fixes without asking (for a headless install,
#                     where nobody is at the console to answer the prompt)
#   --reset-config    replace an unreadable config.json (install refuses otherwise, rather
#                     than silently resetting your curfew, rules and routines)
#   --new-cert        reissue the TLS certificate. Normally unnecessary: install reuses the
#                     existing one whenever it still covers this machine, precisely so a
#                     routine upgrade does NOT make every paired device warn again. Reach
#                     for this when the PC's addresses have changed, or when you want a
#                     fresh key — and expect to accept the warning once more on each device.
```

- **`install` checks everything first.** Before it changes anything — and before it asks for a
  password — it verifies the port is free, the Windows tools it needs are present, no leftover
  service is disabled or mid-deletion, the file isn't still marked as downloaded-from-the-internet,
  and the network is Private rather than Public. Anything that would stop the install is reported
  with nothing yet touched; anything that merely affects the result is reported and it continues.
  Where it can fix something itself it offers to, one at a time, defaulting to no.

- `install` copies the binary to `C:\Program Files\HostHealth\host-health.exe` and registers
  the auto-start, auto-restart LocalSystem service `HostHealthService`. Re-running it updates in
  place and preserves your port, curfew, and rules.
- `uninstall` removes **everything** `install` put on the machine: the service, the firewall rule,
  the binary directory, and any resident helper still running in a signed-in session (which holds
  the binary open, and was why an uninstall used to leave it behind). It then **checks** that each
  is actually gone and **fails, naming what remains**, rather than reporting success on a partial
  removal — you should never walk away believing the controls are gone while a service is still
  running. Your settings, certificate and history stay unless you add `--purge`, which is
  irreversible: it deletes the whole data directory, including every day of recorded screen
  time, the pending time requests and the certificate your devices already trust.
- **Upgrading from a build older than this one: re-run `install` once.** What stops a time-zone
  change from resetting the day's screen time (or moving the curfew window) is recorded *at install
  time* — both the UTC offset and, since this version, **which time zone the machine is in**. An
  install upgraded in place has neither, and falls back to plain local time; one upgraded from a
  version before the zone was recorded keeps the older, weaker offset-only check. Re-running
  `install` records both. Do the same if the PC genuinely moves to another time zone.
  <br>Worth doing rather than skipping: the offset-only check could be walked an hour in winter and
  **two hours in summer** by picking a different time zone, which is enough to push a 21:00 curfew
  to 23:00 every night. Comparing the zone itself closes that. See
  [`docs/SECURITY.md`](docs/SECURITY.md#resisting-the-childs-own-privileges).
- Silent install: set `NESTWATCH_PASSWORD` to skip the interactive prompt.
- **Updating without going to the PC:** [`docs/REMOTE-UPDATE.md`](docs/REMOTE-UPDATE.md) — how to
  do it over the network safely, why the usual advice for home networks is unsafe here, and why
  there is no auto-updater.
- `nestwatch.exe run` (interactive, no service) and `nestwatch.exe helper --capture <path>`
  also exist — the latter is what the service invokes in the user session for screenshots.

Config/cert live in `C:\ProgramData\HostHealth` (Windows) / `~/.config/nestwatch` (dev).

## Develop / test

On macOS or Linux the app uses `FakeControl` (synthetic processes, placeholder screenshot,
no-op shutdown), so you can run and click through everything:

```bash
cd web && npm ci && npm run build && cd ..   # once: compiles assets/app.css, which is gitignored
NESTWATCH_PASSWORD=dev-password cargo run -- install   # 8+ chars, or install refuses
cargo run -- run        # https://localhost:8443
cargo test              # unit + HTTP integration tests (run on any OS)
cd web && npm test      # the dashboard's own logic (node:test, no framework installed)
```

Skip the CSS build and the UI serves unstyled — `build.rs` warns, it is not an error. It also
warns when `assets/app.css` is older than the markup, the scripts, or `web/package-lock.json`,
because all four are inputs to it.

**Verification status**, in three tiers — worth stating precisely, because the middle one is
easy to under-use:

1. **Cross-platform core** (auth, routing, curfew logic + enforcement, handlers) — unit and
   integration tested, and verified live on macOS via `FakeControl`. The dashboard's own logic
   (version comparison, enforcement-staleness, chart heights, shared formatting) is covered by
   `web/test/`, which CI runs on both Linux and Windows. Its DOM and network paths are **not**
   covered — the bugs found by opening the page in a browser were all in that half.
2. **Windows code that needs no privileges** — genuinely *executed* on real Windows. CI runs
   `cargo test --all-targets` on a `windows-latest` runner, so a `#[cfg(windows)] #[test]` runs
   there on every push. That is stronger than the cross-compile check below, and anything that
   can be tested this way should be: `tests/spawn_paths.rs` uses it to confirm every system
   binary the code asks for actually exists.
**Typecheck the Windows-only code before pushing.** Most of `install.rs`, `doctor.rs`,
`session.rs` and `control/windows.rs` sits behind `#[cfg(windows)]`, so a host build never
compiles it and neither does host clippy — four separate breakages reached CI this way in one
sitting, each invisible locally. One command prevents all of them:

```bash
rustup target add x86_64-pc-windows-gnu     # once; needs `brew install mingw-w64` for ring's C
cargo check --target x86_64-pc-windows-gnu  # then, before every push that touches cfg(windows)
```

It typechecks the real code — verified by reintroducing a known breakage, which the host check
passed and this one caught. Use rustup's `cargo`, not a distro or Homebrew one: those ignore
`rust-toolchain.toml`, and the pinned toolchain is where the target is installed (`build.rs`
warns if the compiler and the pin disagree).

3. **Everything else Windows-only** — the SYSTEM service, the `CreateProcessAsUser` session
   helper, ACL hardening, WTS session state, SCM restart semantics, and recovery-mode boot
   paths. These are
   compile- and link-verified via the Windows target, and their **runtime behavior must be
   verified on an actual machine**: see [`docs/WINDOWS-TESTING.md`](docs/WINDOWS-TESTING.md).
   Every serious bug this project has had lived in this tier.

**Where the current release stands.** `v0.5.1` (2026-08-31) is the newest published release — a
single security fix over `v0.5.0` the same day, adding nothing that needs a Windows machine to
verify. Both shipped with tiers 1 and 2 green and **tier 3 unrun**: the 32 items in section H of the checklist — covering the bedtime extension, the
enforcer wake, the translated shutdown notices and the ask link — have not been executed on a
Windows machine. That is stated here rather than only in the changelog, because tier 3 is the tier
the sentence above says every serious bug has lived in, and a reader deciding whether to install
this is entitled to know which tier the newest features sit in.

Design problems that are known, judged real, and deliberately not scheduled are written down in
[`docs/OPEN-FINDINGS.md`](docs/OPEN-FINDINGS.md), along with the things reviews suggested that were
weighed and declined — so neither has to be rediscovered.

## Not included

- **Keylogging / covert monitoring** — never. This is overt parental control, not spyware.
- **Off-LAN access** — by design you must be on the home network. Want remote reach? Bring your
  own VPN: unsupported, and only *partly* compatible, because the app-layer allowlist admits
  RFC1918 (`10/8`, `172.16/12`, `192.168/16`) plus loopback and nothing else. A VPN that puts you
  on the home subnet works; a tunnel that gives you an address outside those ranges gets a `403`
  even though the tunnel itself is fine — the allowlist failing closed, not a bug.
  **[docs/REMOTE-ACCESS.md](docs/REMOTE-ACCESS.md)** covers which arrangements work, which ones
  quietly don't, and what each costs. Short version: give yourself a way in, never give the
  monitored PC a way out.
- **Live screen streaming** and a **multi-machine hub** — not built. The `SystemControl` trait
  leaves room to add streaming later without touching the web layer.
- **More than one child on the same PC** — not supported, and this is a shape rather than a gap.
  There is one daily budget, one curfew and one set of app rules per install, because nothing in
  the stored configuration has a *person* in it. So two children sharing a PC share one budget and
  the first to use it spends the second's evening, and your own account on that PC draws down your
  child's screen time while you do the taxes. One managed child per PC is the design; if two
  children share a machine, give them separate Windows accounts and know that the limits still
  apply to the machine, not to whoever is signed in.
  <br>This is called out here rather than left to be discovered because the failure is quiet: the
  budget is not wrong, it is measuring something other than what you assumed, and it looks exactly
  like a child who used more than they say. Reporting has the same boundary — see
  [`docs/OPEN-FINDINGS.md`](docs/OPEN-FINDINGS.md) `O6`.
- **Web/content filtering** — not built, and not planned in the blocking sense. There is code to
  break browser time out by **page title** — what a tab was called — but see the warning below
  before relying on it. It blocks nothing, records no addresses, and builds no browsing history;
  getting the actual domains would mean changing your children's browser DNS settings, and
  **[docs/FOREGROUND-TRACKING.md](docs/FOREGROUND-TRACKING.md)** records why that was declined.
- **Foreground-app-aware *limits*** (e.g. "earn time in a learning app") — limits count an app while
  it is **running**, on purpose: a game left idling in the background would otherwise stop consuming
  its limit. Focused time is measured *for the report only* and never decides when the PC locks —
  the helper that measures it runs as your child, so its figures are your child's to influence.
  **[docs/FOREGROUND-TRACKING.md](docs/FOREGROUND-TRACKING.md)** records the reasoning.
- ⚠️ **Focused-time and page-title measurement has never run on a real machine.** It is written, it
  compiles, and its arithmetic is tested — but the half that talks to Windows has not executed once,
  here or anywhere. **Until you have worked through §D2 of
  [`docs/WINDOWS-TESTING.md`](docs/WINDOWS-TESTING.md), treat those two columns as unproven.**
  This is called out here rather than left to the design notes because the failure is quiet: if the
  watcher does not run on your PC, the focused and browser columns are simply empty — which looks
  exactly like a child who used no browser, rather than like a feature that did not start. Nothing
  else is affected. Screen-time totals, curfew, and every limit work as they always have; this part
  only measures.
- **Notifications while you're away from home** — not possible here, and structurally rather than
  for want of work: a notification that reaches you outside the house needs a server outside the
  house, which is the one thing this design refuses. The dashboard is a web page and on a phone it
  stays one.
  <br>**A native client does exist**, and this section used to say it didn't.
  [nestwatch-mobile](https://github.com/emrecdr/nestwatch-mobile) is a Flutter app for Android and
  iOS that **pins this install's certificate** rather than asking you to click through a browser
  warning. Calibrate it honestly: it is a completed walking skeleton, each step proven against a
  live server, with an installable APK and a CI job that runs this repo's golden-file contract
  against it — but **no tagged release**. Buildable, not shipped. What a native client buys and
  what it cannot is **[docs/MOBILE-APP.md](docs/MOBILE-APP.md)**; the cost it does carry is a
  second interface to keep in step with this one, forever.
  <br>Worth knowing before reading the security model, because one decision there depends on it:
  `security::require_same_origin` deliberately declines OWASP's recommendation to fail closed when
  a request carries neither `Origin` nor fetch metadata — **that client is one of those requests**.
  Read as "there is no app", the exemption looks like it protects nothing but `curl`.
