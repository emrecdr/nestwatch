# Nestwatch — home remote control

> On the managed PC the on-disk footprint is deliberately bland (`HostHealth*` service,
> folders, and files) so nothing advertises the tool. "Nestwatch" is the parent-facing
> project name only.


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
- **On-screen warning to the child** before a Lock actually fires.

**App controls**
- **Blocklist** — named apps killed on sight.
- **Per-app daily limits** — an app is killed once it exceeds its own minutes.
- **App groups** — several apps sharing **one** daily pool (e.g. all games get 90 min together);
  when the pool is spent, every member is killed.

**Curfew** — a "the PC shouldn't be on now" schedule, separate from the budget.
- One or more **time windows**, each with **per-day-of-week** selection.
- Warns, then **shuts down**, and **re-issues** the shutdown if it's cancelled.

**Granting more time**
- **Parent bonus** buttons (**+15 / +30 / +60 min**) on the Today card.
- **Child "request more time"** page at `/ask` → the parent **approves or denies** it.
- **Offline time codes** — the parent generates a single-use code; the child redeems it at `/ask`
  even while the parent is away or the network is down.

**Modes & presets**
- **Pause / resume** the whole rules enforcer with one toggle (a free evening) — curfew still
  applies.
- **Named routines** — save the current rules as a preset (Homework / Weekend / …) and apply one
  with a click.

**Visibility**
- **Today's usage** — minutes used / remaining, plus per-app and per-group bars.
- **Usage history** — daily screen-time and enforcement events.
- **Access log** — logins (with source IP) and every sensitive action.
- **Live dashboard** — the Today view and pending requests refresh automatically; a navbar badge
  shows the pending-request count.

**Account & safety**
- Single **password** login (Argon2id); **change the password** from the dashboard.
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
                          │  ├─ curfew enforcer  (window/day → warned shutdown)
                          │  ├─ rules enforcer   (screen-time budget / blocklist / app limits;
                          │  │                     counts active use only, warns child → kill · lock · shutdown)
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

You download and run **`nestwatch.exe`** — it's the same binary that `install` copies to
`C:\Program Files\HostHealth\host-health.exe` (the bland on-disk name) on the target.

**Releases:** push a tag (`git tag vX.Y.Z && git push --tags`) and `.github/workflows/release.yml`
builds `nestwatch.exe` + a SHA-256 and attaches them to a GitHub Release. See
[`CHANGELOG.md`](CHANGELOG.md) for what's in each version.

## Use (on the Windows machine, from an elevated/Administrator console)

```powershell
nestwatch.exe install     # password + TLS cert; copies binary, registers & starts the
                          # SYSTEM service, hardens ACLs
nestwatch.exe uninstall   # stop + delete the service
nestwatch.exe fingerprint # re-print the TLS cert SHA-256 (to verify a new device later)
```

- `install` copies the binary to `C:\Program Files\HostHealth\host-health.exe` and registers
  the auto-start, auto-restart LocalSystem service `HostHealthService`.
- Reach it from your phone/laptop at `https://<his-pc-ip-or-hostname>:8443`; accept the
  one-time self-signed-certificate warning; log in; set the curfew window in the dashboard.
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

**Verification status:** the cross-platform core (auth, routing, curfew logic + enforcement,
handlers) is unit/integration-tested and verified live on macOS. The Windows-only code
(SYSTEM service, `CreateProcessAsUser` session helper, ACL hardening) is compile- and
link-verified via the Windows target, but its **runtime behavior must be verified on an
actual Windows machine** — see [`docs/WINDOWS-TESTING.md`](docs/WINDOWS-TESTING.md) for a
step-by-step on-device checklist.

## Not included

- **Keylogging / covert monitoring** — never. This is overt parental control, not spyware.
- **Off-LAN access** — by design you must be on the home network. Want remote reach? Bring your
  own VPN (WireGuard/Tailscale) — unsupported but compatible; the app itself stays LAN-only.
- **Live screen streaming** and a **multi-machine hub** — not built. The `SystemControl` trait
  leaves room to add streaming later without touching the web layer.
- **Web/content filtering** and **foreground-app-aware limits** (e.g. "earn time in a learning
  app") — not yet. Both need Windows-specific work that must be verified on real hardware; today's
  limits count an app as used while it's *running*, not only while it's focused.
