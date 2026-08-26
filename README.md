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
| **Curfew** | One or more time windows per weekday, separate from the budget. Counts down on the child's screen, then shuts down — and re-issues if the shutdown is cancelled. |
| **Warnings** | 15, 5 and 1 minutes before both the budget and bedtime, so the limit is never a surprise. A budget shorter than a threshold never announces it; a mid-day restart doesn't replay warnings; granting extra time re-arms them. |
| **Screen-time report** | 7, 30 or 90 days as a chart, with a comparison against the period before. Click a column to drill into that day. Most-used lists cover the whole window — by app, by category, by time actually in front, and by browser page — so it answers "how much Roblox this month", not only "what happened last Tuesday". **Known game portals are badged by name** (now.gg, Poki, CrazyGames and the like) wherever page titles are listed; no badge means nothing was recognised, not that nothing was played. Days the service wasn't running show as **not measured**, never as zero, so a stopped enforcer can't look like a quiet week. **New apps are called out** — anything used for the first time, with the number of days of history behind the claim. |
| **Asking for more** | The child's page shows time left and can request more; you approve or deny. Single-use offline codes cover times you're away or the network is down. |
| **Remote control** | Screenshot the desktop (with live refresh at your choice of 2/5/15s), list and kill running apps, lock the screen, shut down with a warned countdown. Live frames are small and cheap; clicking **Expand** fetches a full-resolution one and keeps it full for as long as the big view is open, so a picture you opened to *read* stays readable. Your click always takes priority over a frame already being fetched. **Windows draws a yellow border around the screen while it is being captured** — the child can see when you are looking. |
| **App rules** | Blocklist, per-app daily limits, and groups sharing one pool. Habit-shaping, not a wall — see [Not included](#not-included). |
| **Modes** | Pause the whole rules enforcer with one toggle for a free evening (curfew still applies). Save the current setup as a named routine and reapply with a click. |
| **Trust the setup** | `nestwatch doctor` reports whether the service is up, the port listening, the firewall rule right, the network private, the certificate valid, whether this Windows is new enough to take screenshots at all, and whether anything is actually being enforced. Every problem prints its fix. |
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
- **A phone app** — not built. The dashboard is a web page, and on a phone it stays one.
  **[docs/MOBILE-APP.md](docs/MOBILE-APP.md)** records what a native app would and wouldn't buy:
  it would remove the certificate warning, it could not notify you while you're away from home
  (that needs a cloud service this design refuses), and it would mean a second interface to keep
  in step with the first, forever.
