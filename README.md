# Nestwatch — home remote control

> On the managed PC the on-disk footprint is deliberately bland (`HostHealth*` service,
> folders, and files) so nothing advertises the tool. "Nestwatch" is the parent-facing
> project name only.


A single self-contained Rust app that lets a parent, from any device on the **same home
network**, log into a web page and control a child's Windows PC:

- take a screenshot, see running apps, close a specific app, shut the machine down;
- enforce a **curfew** — a closed time window during which the PC auto-shuts-down (with a
  warning countdown);
- run as a **tamper-resistant SYSTEM service** a standard (non-admin) user can't stop.

Single-user, LAN-only. No keylogging or covert data collection.

## How it works

One binary. On Windows it installs as a **SYSTEM service** (Session 0) that serves the web
UI + JSON API over self-signed HTTPS and runs the enforcement (curfew, process kill,
shutdown). Because Session 0 has no desktop, **screenshots are delegated to a short-lived
helper** launched into the interactive user session. All OS access sits behind a
`SystemControl` trait, so the whole app also builds, runs, and is tested on macOS/Linux via
a `FakeControl`.

```
Browser (LAN) ──HTTPS──> SYSTEM service (Session 0) ── axum ── auth (argon2 + session)
                          │  ├─ curfew enforcer (local-time window → warned shutdown)
                          │  ├─ processes / kill / shutdown        [direct, Session 0 OK]
                          │  └─ screenshot ─→ helper in user session (WTSQueryUserToken +
                          │                    CreateProcessAsUserW) ─→ xcap ─→ PNG
                          └─ SystemControl trait ─→ ServiceControl │ WindowsControl │ FakeControl
```

| Layer | Crates |
|---|---|
| Web / TLS | axum 0.8, axum-server 0.8, rustls 0.23 (**ring** provider), tower-sessions 0.15 |
| Assets | rust-embed 8 (embeds `assets/`) |
| Auth | argon2 0.5 (Argon2id) |
| OS ops | xcap 0.9 (screen), sysinfo 0.39 (processes), `shutdown /s` (power) |
| Service / FFI | windows-service 0.8, windows 0.62 (WTS + CreateProcessAsUser) |
| Time | chrono 0.4 (local-time curfew window) |
| Cert | rcgen 0.14 |
| UI | Alpine.js 3.15, Tailwind CSS v4.3, daisyUI 5.6 (built to `assets/app.css`) |

## Tamper-resistance — and its limits

The design resists a **standard (non-admin) user**, which is how parental control is meant
to work:

- The SYSTEM service can't be stopped or deleted by a standard user (Task Manager shows
  "Access Denied"); it auto-restarts on failure.
- The binary lives in `C:\Program Files\HostHealth\` and the config/cert in
  `C:\ProgramData\HostHealth\`, both ACL-hardened to SYSTEM + Administrators only — a standard
  user can't read the password hash or delete the files.
- Low-profile service name; no window or tray icon.

**Hard limits (stated honestly):**
- If the child is a **local administrator**, no software-only tool can reliably resist them.
  Make sure their account is a standard user.
- This intentionally does **not** use rootkit/process-hiding techniques — those trip
  antivirus and destabilize the machine. The service is visible in Task Manager; it just
  can't be stopped without admin rights.

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

## Use (on the Windows machine, from an elevated/Administrator console)

```powershell
nestwatch.exe install     # password + TLS cert; copies binary, registers & starts the
                          # SYSTEM service, hardens ACLs
nestwatch.exe uninstall   # stop + delete the service
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
actual Windows machine**.

## Not included (by design)

Live screen streaming, keylogging/covert monitoring, multi-machine hub, audit logs. The
`SystemControl` trait leaves room to add live streaming later without touching the web layer.
