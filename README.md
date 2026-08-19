# Nestwatch — home remote control

> "Nestwatch" is the parent-facing project name. The service, folders and files the installer
> creates on the managed PC are named plainly and unremarkably instead; `nestwatch.exe doctor`
> lists exactly what is installed and where.


A single self-contained Rust app that lets a parent, from any device on the **same home
network**, log into a web page and manage a child's Windows PC. No cloud, no accounts, no
telemetry, no keylogging — one password, and everything stays on your LAN.

## Features

Every capability below is exposed in the dashboard (or, for the child, at `/ask`). For a
step-by-step way to verify each one on the real machine, follow
**[`docs/WINDOWS-TESTING.md`](docs/WINDOWS-TESTING.md)**.

**Remote control**
- **Screenshot** the primary monitor, with an optional **Live** auto-refresh toggle.
- **Running apps** list (heaviest first) and **kill** any process.
- **Lock** the screen (password required to resume).
- **Shut down** the machine (with a warned countdown).

**Daily screen-time budget** — enforced by a background service that counts only *active* use
(not idle, locked, or logged-out time), persists across reboots, and resets at midnight.
- **Daily limit** in minutes (`0` = no limit).
- **Per-day-of-week limits** — a different budget for each weekday (`0` = no limit that day).
- **Action when the budget is spent:** **Lock** (default), **Shut down**, or **Warn only**.
- **Countdown warnings to the child** at **15, 5 and 1 minutes left**, so the limit is never a
  surprise — plus a final on-screen warning before a Lock actually fires. Each fires once: a budget
  shorter than a threshold never announces it, a restart mid-day doesn't replay warnings already
  passed, and granting extra time re-arms them. Locking the screen and unlocking it doesn't earn
  another grace period, and a cancelled shut-down is re-issued immediately rather than offering
  another countdown to cancel.
- **Resists clock tampering** — changing the PC's time zone (which Windows lets a standard user do
  with no prompt) can't reset the day's tally or move the curfew window. Real daylight-saving
  changes are still followed.

**App controls**
> **These are speed bumps, not walls — the budget and curfew are the real controls.** Apps are
> matched by their filename, so a child who copies `chrome.exe` to `notes.exe` in their own folder
> is no longer blocked. There's no software fix for that at this level; if you need a genuine app
> wall, Windows **AppLocker** (Pro/Enterprise) or **Microsoft Family Safety** enforce by publisher
> or file hash in the kernel, and can't be dodged by renaming. Use these to steer everyday habits,
> and the daily budget plus curfew for the limits that actually have to hold.

- **Blocklist** — named apps killed on sight.
- **Per-app daily limits** — an app is killed once it exceeds its own minutes.
- **App groups** — several apps sharing **one** daily pool (e.g. all games get 90 min together);
  when the pool is spent, every member is killed.

**Curfew** — a "the PC shouldn't be on now" schedule, separate from the budget.
- One or more **time windows**, each with **per-day-of-week** selection.
- **Counts down to bedtime** on the child's screen at **15, 5 and 1 minutes**, then **shuts down**,
  and **re-issues** the shutdown if it's cancelled.

**Granting more time**
- **Parent bonus** buttons (**+15 / +30 / +60 min**) on the Today card.
- **The child's own page** at `/ask` shows **how much time they have left today** and a progress
  bar — so they can check without asking — plus a form to **request more time**, which the parent
  **approves or denies**. It deliberately shows totals only: no blocked-app names, no per-app
  limits, no curfew times.
- **Offline time codes** — the parent generates a single-use code; the child redeems it at `/ask`
  even while the parent is away or the network is down.

**Modes & presets**
- **Pause / resume** the whole rules enforcer with one toggle (a free evening) — curfew still
  applies.
- **Named routines** — save the current rules as a preset (Homework / Weekend / …) and apply one
  with a click.

**Trust the setup**
- **`nestwatch doctor`** — one screen: is the service up, is the port listening, is the firewall
  rule right, is the network Private, how long has the certificate got, is anything actually being
  enforced, and who are the local administrators. Every problem prints its fix.

**Visibility**
- **Today's usage** — minutes used / remaining, plus per-app and per-group bars.
- **Screen-time report** — the last 30 days as a chart, with per-app minutes for apps that have a
  limit, a running total and a comparison against the previous period. Days the service wasn't
  running are drawn as **not measured**, never as zero, so a stopped enforcer can't be mistaken
  for a quiet week. It counts time the PC was unlocked with an app *running* — not focused
  attention, and not per-account — so the figures aren't comparable to a phone's screen time.
- **Usage history** — daily screen-time and enforcement events.
- **Access log** — logins (with source IP) and every sensitive action.
- **Live dashboard** — the Today view and pending requests refresh automatically; a navbar badge
  shows the pending-request count.

**Setup & access**
- **Scan-to-pair** — `install` prints a QR code; scan it and you're in the dashboard, signed in,
  without typing an address or a password on a phone. Single-use, expires in 15 minutes.
  `nestwatch pair` mints a fresh one for the next device.

**Account & safety**
- Single **password** login (Argon2id); **change the password** from the dashboard — which also
  signs every other device out.
- **Stay signed in** — sessions survive reboots and service restarts, with a 30-day
  "remember this device" window, so you don't retype a passphrase on a phone after every restart.
- **LAN-only** — a Windows firewall rule *and* an app-layer allowlist.
- **HTTPS** with a verifiable self-signed certificate; `nestwatch fingerprint` re-prints its
  SHA-256 so you can verify a new device later.
- **Tamper-resistant SYSTEM service** a standard (non-admin) user can't stop.

## How it works

One binary. On Windows it installs as a **SYSTEM service** (Session 0) that serves the web
UI + JSON API over self-signed HTTPS and runs two background enforcers — **curfew** and the
**usage rules** (screen-time budget, app blocklist, per-app limits). Because Session 0 has no
desktop, **screenshots and screen-lock are delegated to a short-lived helper** launched into
the interactive user session. All OS access sits behind a `SystemControl` trait, so the whole
app also builds, runs, and is tested on macOS/Linux via a `FakeControl`.

```
Browser (LAN) ──HTTPS──> SYSTEM service (Session 0) ── axum ── auth (argon2 + session)
                          │  ├─ curfew enforcer  (window/day → 15/5/1-min warnings → shutdown)
                          │  ├─ rules enforcer   (screen-time budget / blocklist / app limits;
                          │  │                     counts active use only, 15/5/1-min warnings
                          │  │                     → kill · lock · shutdown)
                          │  ├─ processes / kill / shutdown         [direct, Session 0 OK]
                          │  └─ screenshot + lock ─→ helper in user session (WTSQueryUserToken +
                          │                           CreateProcessAsUserW) ─→ xcap ─→ PNG
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
| UI | Alpine.js 3.15, Tailwind CSS v4.3, daisyUI 5.6 (built to `assets/app.css`) |

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

## First run (start here)

Before you begin, two things the tool depends on and can't fix for you:

- **The child's Windows account must be a *standard* user, not an administrator.** Check with
  `net localgroup Administrators` — their name must not be listed. A local admin can stop the
  service, and no software-only tool can prevent that.
- **The PC's network must be set to *Private*.** The firewall rule is scoped to private/domain
  networks; on a "Public" network it silently never matches and you just can't connect.

`nestwatch doctor` (below) checks both and tells you how to fix them.

**1. Download and unblock it.** Grab `nestwatch.exe` from the
[Releases page](https://github.com/emrecdr/nestwatch/releases). Then — this step saves a scare —
**right-click the file → Properties → tick "Unblock" → OK.** That clears the
"downloaded from the internet" mark, so Windows won't show the *"Windows protected your PC"*
dialog at all.

If you skip it and that blue dialog appears: the only visible button is **Don't run**; click the
small **More info** link, then **Run anyway**. (The binary is unsigned — a code-signing
certificate costs money a hobby project doesn't have. Verify what you downloaded instead:
`Get-FileHash .\nestwatch.exe -Algorithm SHA256` should match the published `nestwatch.exe.sha256`.)

**2. Install it.** Start menu → type `powershell` → **right-click → Run as administrator**
(an elevated window opens in `System32`, so `cd` first). Installing requires elevation and will
stop with a clear message if you forget.

```powershell
cd $env:USERPROFILE\Downloads
.\nestwatch.exe install
```

You'll be asked to set a control password — this is the password for the dashboard, not your
Windows password. At least 10 characters; a short sentence works well.

**3. Scan the QR code.** `install` prints one. Scan it with your phone's camera (on the same
Wi-Fi) and you land in the dashboard **already signed in** — no typing an IP or a password on a
phone. Your browser will warn once that the certificate isn't trusted: that's expected, because
the certificate is made by your own PC rather than bought from a company. Continue past it.

The QR is single-use and expires after 15 minutes. Run `nestwatch.exe pair` any time for a fresh
one — that's also how you add a second phone or a laptop.

**4. Set a limit.** A fresh install enforces **nothing** — it only counts screen time. The
dashboard says so ("Nothing set up yet"). Set a daily limit, or turn on Curfew, or both.

**5. Check it worked.**

```powershell
.\nestwatch.exe doctor
```

One screen: is the service running, is the port listening, is the firewall rule right, how long
the certificate has left, whether anything is actually being enforced, and who the local
administrators are. Anything wrong prints the fix underneath it.

**6. Show your child their page.** `https://<pc-address>:8443/ask` — it shows how much time they
have left today and lets them ask for more (or redeem a code you've given them). The dashboard's
*Time codes* card has a Copy-link button for exactly this.

## Command reference

```powershell
nestwatch.exe install     # password + TLS cert; copies binary, registers & starts the
                          # SYSTEM service, hardens ACLs; prints a QR to pair your phone
nestwatch.exe doctor      # check the install; report anything wrong and how to fix it
nestwatch.exe pair        # print a fresh QR to sign in another phone/laptop
nestwatch.exe fingerprint # re-print the TLS cert SHA-256 (to verify a new device later)
nestwatch.exe uninstall   # stop + delete the service (add --purge to remove data too)
nestwatch.exe version     # print this build's version (also --version / -V)

# install also accepts:
#   --port <N>        listen on a different port
#   --reset-config    replace an unreadable config.json (install refuses otherwise, rather
#                     than silently resetting your curfew, rules and routines)
```

- `install` copies the binary to `C:\Program Files\HostHealth\host-health.exe` and registers
  the auto-start, auto-restart LocalSystem service `HostHealthService`. Re-running it updates in
  place and preserves your port, curfew, and rules.
- **Upgrading from a build older than this one: re-run `install` once.** The clock anchor that stops a time-zone
  change from resetting the day's screen time (or moving the curfew window) is recorded *at install
  time*, so an install upgraded in place doesn't have one and falls back to plain local time.
  Re-running `install` records it. Do the same if the PC genuinely moves to another time zone.
- Silent install: set `NESTWATCH_PASSWORD` to skip the interactive prompt.
- `nestwatch.exe run` (interactive, no service) and `nestwatch.exe helper --capture <path>`
  also exist — the latter is what the service invokes in the user session for screenshots.

Config/cert live in `C:\ProgramData\HostHealth` (Windows) / `~/.config/nestwatch` (dev).

## Develop / test

On macOS or Linux the app uses `FakeControl` (synthetic processes, placeholder screenshot,
no-op shutdown), so you can run and click through everything:

```bash
NESTWATCH_PASSWORD=devpass cargo run -- install
cargo run -- run        # https://localhost:8443
cargo test              # unit + HTTP integration tests (run on any OS)
```

**Verification status**, in three tiers — worth stating precisely, because the middle one is
easy to under-use:

1. **Cross-platform core** (auth, routing, curfew logic + enforcement, handlers) — unit and
   integration tested, and verified live on macOS via `FakeControl`.
2. **Windows code that needs no privileges** — genuinely *executed* on real Windows. CI runs
   `cargo test --all-targets` on a `windows-latest` runner, so a `#[cfg(windows)] #[test]` runs
   there on every push. That is stronger than the cross-compile check below, and anything that
   can be tested this way should be: `tests/spawn_paths.rs` uses it to confirm every system
   binary the code asks for actually exists.
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
  own VPN — unsupported, and only *partly* compatible: the app-layer allowlist admits RFC1918
  (`10/8`, `172.16/12`, `192.168/16`) plus loopback, and nothing else. A VPN that puts you on the
  home subnet (WireGuard routing you into `192.168.x.x`, or your router's own VPN) works.
  **Tailscale does not** — it assigns from the carrier-grade-NAT range `100.64.0.0/10`, which is
  not RFC1918, so you'll get a `403` even though the tunnel itself is fine. That's the allowlist
  failing closed rather than a bug; widening it would extend the trust boundary past the home
  network for every install.
- **Live screen streaming** and a **multi-machine hub** — not built. The `SystemControl` trait
  leaves room to add streaming later without touching the web layer.
- **Web/content filtering** and **foreground-app-aware limits** (e.g. "earn time in a learning
  app") — not yet. Both need Windows-specific work that must be verified on real hardware; today's
  limits count an app as used while it's *running*, not only while it's focused.
