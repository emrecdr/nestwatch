# Nestwatch — home remote control

> On the managed PC the on-disk footprint is deliberately bland (`HostHealth*` service,
> folders, and files) so nothing advertises the tool. "Nestwatch" is the parent-facing
> project name only.


A single self-contained Rust app that lets a parent, from any device on the **same home
network**, log into a web page and manage a child's Windows PC. No cloud, no accounts, no
telemetry — one password, and everything stays on your LAN.

**Remote control** — take a screenshot (with an optional near-live auto-refresh), see running
apps, close a specific app, **lock** the screen, or shut the machine down.

**Screen-time rules**, enforced by a background service that counts only *active* use (not idle,
locked, or logged-out time):
- a **daily budget**, optionally **different per day of week**, with a configurable action when
  it's spent — **lock** (default), **shut down**, or **warn-only** — and a warning the child sees
  first;
- an **app blocklist** (killed on sight), **per-app daily limits**, and **app groups** that share
  one pool (e.g. all games get 90 minutes *together*);
- a **curfew** — multiple windows, per day of week — during which the PC won't stay on;
- a one-switch **pause** for a free evening, and **named routines** (Homework / Weekend / …) you
  switch between with one click.

**Granting more time** — a **today's-usage** view (minutes used / remaining, per app and per
group) with **+15 / +30 / +60 bonus** buttons; a child **"request more time"** page at
`https://<this-pc>:8443/ask` that the parent approves or denies; and **offline time codes** you
generate and hand over, which the child redeems on their own PC even while you're away.

**Operations & safety** — a **usage history** and an **access log** on a **live-updating
dashboard**; password change and a `fingerprint` command to re-verify the certificate; HTTPS with
a verifiable fingerprint; and a **tamper-resistant SYSTEM service** a standard (non-admin) user
can't stop.

Single-user, LAN-only. No keylogging or covert data collection.

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

**Releases:** push a tag (`git tag v0.3.5 && git push --tags`) and `.github/workflows/release.yml`
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
