# Nestwatch — Security Model

Nestwatch lets a parent, from another device on the **same home network**, log into a web
page and control a child's Windows PC (screenshot, list/kill apps, shut down, set a curfew).
Because those are powerful, partly destructive actions, the security goal is narrow and
concrete:

> **Only the parent, from a device on the home LAN, can reach the controls — and every access
> is recorded so it's visible.**

This document is the threat model and the list of protections. It is scoped to a home LAN and
a single parent; it deliberately does **not** try to be safe against a determined attacker who
already has administrator rights on the PC, or against the wider internet.

---

## What an authenticated session can do (the "prize")

One valid login unlocks all of it, so the whole model reduces to *who can get an
authenticated session*:

| Capability | Endpoint |
|---|---|
| See the live screen | `GET /api/screenshot` |
| List running apps | `GET /api/processes` |
| Kill any app | `POST /api/processes/{pid}/kill` |
| Power off the PC | `POST /api/shutdown` |
| Read / change the curfew | `GET`·`POST /api/curfew` |
| Read the access log | `GET /api/audit` |

## Who might try to reach it (adversaries in scope)

- **A stranger on the Wi-Fi** — a guest, a visiting friend of the child, a neighbour who
  learned the Wi-Fi password, or a compromised phone/IoT device on the LAN. This is the
  primary adversary.
- **The child (a standard, non-admin user of the PC).** Handled mainly by the *tamper
  resistance* model (SYSTEM service + ACLs) documented in the README; not repeated here.

Out of scope: an attacker with local Administrator on the PC (no software-only measure is
reliable against that), and exposure to the public internet (the tool is LAN-only by design).

---

## Trust boundaries & layered protections

Access to the controls passes through several independent layers, so a failure in one does not
open the door on its own.

### 1. Network scope — two independent gates
- **Windows Firewall rule** (`install`): inbound TCP allowed only from `LocalSubnet` on
  Private/Domain profiles. This blocks off-subnet traffic before the app even sees it. The
  installer reads the rule back after adding it and **warns loudly** if it didn't apply (this
  is non-fatal because the next gate, below, is the real guarantee).
- **App-layer LAN allowlist** (`src/security.rs::require_lan_peer`): the server itself rejects
  any client whose source IP is not private/loopback, returning `403` before any
  authentication work. This is deliberate defense-in-depth: even if the firewall rule is
  missing, disabled, or the network profile flips to *Public*, the controls are not reachable
  from off-LAN. The peer address comes from the TCP socket (`ConnectInfo`), never from a
  spoofable `X-Forwarded-For` header (there is no reverse proxy).

### 2. Transport — TLS with a verifiable identity
- All traffic is HTTPS (rustls, TLS 1.2+). The password and screenshots never travel in clear.
- The certificate is **self-signed**, so the browser shows a one-time trust warning. To tell
  the real server from a LAN impostor, `install` prints the certificate's **SHA-256
  fingerprint** — verify it once against what the browser shows (trust-on-first-use). Certs
  are valid for **825 days** (the maximum Apple accepts) and carry the `serverAuth` usage, so
  they work on iPhones/Macs as well as desktops.
- **Known residual risk:** a parent trained to click through the warning could be
  man-in-the-middled by an attacker on the LAN presenting their own self-signed cert. The
  fingerprint check is the mitigation; a fully warning-free fix (a trusted certificate) is
  tracked as future work and is out of the LAN-only scope.

### 3. Authentication
- A single password, stored only as an **Argon2id** hash (memory-hard), verified off the async
  runtime. Minimum 10 characters at install.
- The verification is **serialized** (one at a time process-wide), which by itself caps online
  guessing to a handful per second regardless of anything else.
- **Per-IP rate limiting** (`src/auth.rs::LoginLimiter`): after repeated wrong passwords, only
  the *offending* source IP is throttled. A global lockout was deliberately avoided — it would
  let any device on the LAN lock the parent out (a denial-of-service), which OWASP warns
  against.

### 4. Session
- On success the session id is rotated (anti-fixation) and stored in a cookie that is
  `Secure`, `HttpOnly`, and `SameSite=Strict` (the CSRF defense for the state-changing POSTs).
  Sessions are in-memory and expire on 12 h inactivity; a reboot logs the parent out.

### 5. Browser hardening
- Every response carries a strict **Content-Security-Policy** (`default-src 'none'`, allowing
  only the same-origin script/style the page needs, plus `blob:`/`data:` images for
  screenshots and UI icons), `frame-ancestors 'none'` / `X-Frame-Options: DENY`
  (anti-clickjacking), `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, and a
  deny-all `Permissions-Policy`. HSTS is intentionally **not** set — with a self-signed cert a
  browser ignores it, and if it ever stuck it would make cert rotation an unrecoverable
  lockout.

### 6. Auditing / visibility
- Security-relevant events are appended as JSON lines to `audit.jsonl` in the ACL-hardened data
  dir (`src/audit.rs`): login success/failure with **source IP**, rate-limited attempts, and
  the sensitive actions (screenshot, kill, shutdown, curfew change). The parent can review the
  most recent events in the dashboard's **Recent access** panel or via `GET /api/audit`. This
  turns an otherwise invisible access into something you can see — a login from an unfamiliar
  IP at an odd hour stands out. The log never contains secrets (no password, cookie, or hash).

---

## How to verify your install is sound

1. **Cert fingerprint** — the first time a browser warns, compare its certificate SHA-256 to
   the fingerprint `install` printed. They must match; if they don't, you may be talking to an
   impostor on the network.
2. **Firewall** — the network profile on the PC must be **Private** (not Public) for the
   LocalSubnet rule to apply. `install` warns if it couldn't add or read back the rule; heed
   that warning (the app-layer allowlist still protects you, but the firewall is the outer
   layer).
3. **Standard user** — confirm the child's Windows account is a *standard* user, not an
   administrator; the tamper resistance depends on it.
4. **Access log** — after logging in, open **Recent access** and confirm you only see your own
   sign-ins.

## Residual risks (honest limits)

- **Self-signed MITM** if the fingerprint is never verified (see §2).
- **A device that has the Wi-Fi password is "on the LAN."** The allowlist scopes to the local
  network, not to specific devices; the password is what gates control from there, so use a
  strong one.
- **Local administrator on the PC** can defeat any of this — out of scope by design.
