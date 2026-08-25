# Nestwatch — Security Model

Nestwatch lets a parent, from another device on the **same home network**, log into a web
page and control a child's Windows PC (screenshot, list/kill apps, lock or shut down, set a
curfew, set screen-time/app-limit rules, change the password). Because those are powerful,
partly destructive actions, the security goal is narrow and concrete:

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
| See the live screen | `GET /api/screenshot?tier=preview\|full` |
| List running apps | `GET /api/processes` |
| Kill any app | `POST /api/processes/{pid}/kill` |
| Lock the screen | `POST /api/lock` |
| Power off the PC | `POST /api/shutdown` |
| Read / change the curfew | `GET`·`POST /api/curfew` |
| Read / change usage rules (budget, blocklist, per-app limits) | `GET`·`POST /api/rules` |
| Read the access log / usage history | `GET /api/audit`, `GET /api/usage` |
| Read the screen-time report (per-day totals and per-app minutes, up to a year back) | `GET /api/screentime` |
| See pending time requests | `GET /api/time-requests` |
| Approve / deny a time request (grants screen time) | `POST /api/time-requests/{id}/approve`·`deny` |
| Change the control password | `POST /api/password` |

`POST /api/password` keeps the parent logged in (rotating their session id) and **does** revoke
every other session (see §4).

## Who might try to reach it (adversaries in scope)

- **A stranger on the Wi-Fi** — a guest, a visiting friend of the child, a neighbour who
  learned the Wi-Fi password, or a compromised phone/IoT device on the LAN. This is the
  primary adversary.
- **The child (a standard, non-admin user of the PC).** Handled mainly by the *tamper
  resistance* model (SYSTEM service + ACLs) documented in the README; not repeated here.
- **Whoever could substitute the binary before it is installed** — a tampered release asset, a
  swapped download. Every other control in this document assumes the program being run is this
  program, so that assumption is worth an explicit check rather than an implicit one. See
  *Supply chain*.

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
  runtime. **Minimum 8 characters**, with no composition rules and a small blocklist of the
  passwords an attacker reaches first (`12345678`, `password123`, a repeated character, a short
  block repeated). NIST SP 800-63B Rev 4 (final, July 2025) prohibits requiring mixed character
  classes and requires a blocklist instead, so digits-only is accepted: eight digits is 10^8
  against a memory-hard hash behind the throttling below, which is not the weak link — `12345678`
  is, and that is what the blocklist stops.
- **On the length**: Rev 4 asks for 15 characters where a password is the *only* factor, which
  this is. Eight is a deliberate departure, because the exposure differs from the internet-facing
  systems that guidance is written for: an attacker must already be on the home network before
  the prompt is reachable at all, and then meets serialized Argon2id plus per-IP throttling. The
  practical failure here is a password too long to recall ending up on a sticky note beside the
  machine the child uses, which is a worse outcome than a short one the parent remembers.
  Recorded rather than quietly chosen, so the trade is visible.
- The verification is **serialized** (one at a time process-wide), which by itself caps online
  guessing to a handful per second regardless of anything else.
- **Per-IP rate limiting** (`src/auth.rs::LoginLimiter`): after repeated wrong passwords, only
  the *offending* source IP is throttled. A global lockout was deliberately avoided — it would
  let any device on the LAN lock the parent out (a denial-of-service), which OWASP warns
  against.
- There is a *second, separate* throttle for the unauthenticated child endpoint
  (`src/timereq.rs::SubmitLimiter`, 5/min/IP) that counts **every** submission, not just
  failures — see "The child's request-more-time surface" below.

### 4. Session
- On success the session id is rotated (anti-fixation) and stored in a cookie that is
  `Secure`, `HttpOnly`, and `SameSite=Strict`.
- Sessions **persist across restarts** (`sessions.json` in the ACL-hardened data dir) and slide
  on a 30-day inactivity window, so signing in is a one-time cost per device rather than a
  penalty for every reboot of an auto-restarting service. Two consequences worth stating plainly:
  - That file is a set of **long-lived bearer tokens**. It inherits the SYSTEM+Administrators-only
    ACL, so the child can't read it — but anything that copies the data directory (a backup, a
    disk image) copies live credentials. Treat it like the TLS key.
  - A reboot is **no longer** an implicit "log everyone out" lever, which it used to be.
- Changing the password (`POST /api/password`) re-hashes with Argon2id, persists, **signs every
  other device out**, and rotates the caller's own id so the parent stays logged in. Since a
  restart no longer clears sessions, this is the only way to revoke a leaked cookie before its
  30-day expiry — and it's what a worried parent will do, so it must actually work.
- **CSRF:** three layers. `SameSite=Strict` on the cookie; every state-changing endpoint that takes
  a JSON body also requires `Content-Type: application/json`, forcing a CORS preflight that fails
  closed; and an **origin check** on every request (`src/security.rs::require_same_origin`).
  - The origin check exists because the first two leave a real hole. A "site" is scheme +
    registrable domain and **excludes the port**, so a page served over HTTPS from another port on
    this same machine is *same-site* and the browser attaches the parent's session cookie to it.
    Seven `/api` endpoints take no JSON body (`.../kill`, `/shutdown`, `/lock`, `.../approve`,
    `.../deny`, `.../apply`, `.../delete`), so nothing forces a preflight for them and a plain HTML
    form reaches them. The child has an account on this PC and can serve such a page from it. This
    was **demonstrated**, not theorised: with the middleware removed, a same-site `POST` carrying
    the parent's cookie killed a process and returned `200` (`tests/origin.rs`).
  - `Sec-Fetch-Site` distinguishes `same-origin` from `same-site`, which is exactly what the cookie
    attribute cannot. Browsers forbid page scripts from setting any `Sec-` header, so it can't be
    forged. We allow `same-origin`, `none` (a typed URL, a bookmark, the pairing QR), and a
    top-level navigation `GET` (following a link still works) — and reject the rest. A cross-site
    **form POST** is also a navigation, so the `GET`-only condition is load-bearing; there's a test
    pinning it.
  - The header is absent from non-browser clients (`curl`, probes) and pre-2020 browsers, which are
    allowed through: they carry no ambient cookie authority for a third party to abuse, and failing
    closed would break every non-browser caller.

### 5. Browser hardening
- Every response carries a strict **Content-Security-Policy** (`default-src 'none'`, allowing
  only the same-origin script/style the page needs, plus `blob:`/`data:` images for
  screenshots and UI icons), `frame-ancestors 'none'` / `X-Frame-Options: DENY`
  (anti-clickjacking), `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, and a
  deny-all `Permissions-Policy`. HSTS is intentionally **not** set — with a self-signed cert a
  browser ignores it, and if it ever stuck it would make cert rotation an unrecoverable
  lockout.
- **`script-src` does not admit `'unsafe-inline'`.** Both served pages keep their JavaScript in
  `assets/app.js` and `assets/ask.js` rather than in the markup, so `'self'` covers them and a
  `<script>` appearing in the HTML cannot run. That is the directive worth having here, because
  the markup is where injected content would land. A source scan
  (`web::tests::no_inline_script_on_any_served_page`) fails the build if an inline script returns,
  since the browser refuses it silently.
- **`script-src` does not admit `'unsafe-eval'` either — it is `'self'` alone.** It used to, because
  Alpine's standard build compiles every attribute expression with `new Function`. The dashboard now
  ships Alpine's **CSP build**, which parses those expressions with its own parser and cannot reach
  a global at all, so nothing on either page can turn a string into running code. `x-data="app()"`
  became `Alpine.data("app", app)` for the same reason — a global is exactly what that build cannot
  see.
  The migration was 26 directives of 351: eleven template literals, one spread, and fourteen uses of
  `?.`/`??`, each moved into a getter or method. Those four constructs are the *only* ones the CSP
  parser rejects; property paths, ternaries, comparisons, method calls with arguments, assignment,
  `x-model` and array literals all still work in an attribute.
  Two of the four are undocumented, and were settled by probing the build rather than reading about
  it. `web::tests::no_alpine_expression_needs_more_than_the_csp_build_can_parse` fails the build if
  any returns — which matters more than most guards here, because a spread produces **no error at
  all**: the loop simply renders nothing, the same silent shape that once shipped a chart with
  thirty days of data and no bars.
- **What is still permitted, and why:** `style-src` keeps `'unsafe-inline'` — the `[x-cloak]` rule
  is an inline `<style>`, and Alpine writes `style` attributes for `x-show` and `:style`.
  `connect-src` allows `api.github.com`, for the update check behind a button the parent presses;
  nothing contacts it otherwise.

### 6. Auditing / visibility
- Security-relevant events are appended as JSON lines to `audit.jsonl` in the ACL-hardened data
  dir (`src/audit.rs`): login success/failure with **source IP**, rate-limited attempts, and
  the sensitive actions — screenshot, process kill, shutdown, **lock**, curfew change, **rules
  change, password change (and failed attempts), logout, routine save/apply/delete, and each
  time-request submit/approve/deny** (the child submit is logged with its source IP). The parent
  reviews
  recent events in the dashboard's **Recent access** panel or via `GET /api/audit`. This turns an
  otherwise invisible access into something you can see — a login from an unfamiliar IP at an odd
  hour stands out.
- **The live view is logged as a session, not as frames.** A full-resolution capture — the
  *Take screenshot* button, or **Expand** — writes one `screenshot_taken` line each, because there
  are few of them, a person asked for each, and that is the tier detailed enough to read a message
  over someone's shoulder. The live view's small preview frames are counted instead and written as
  a single `live_view` line at most every five minutes, carrying the number of frames it stands
  for.
  <br>This is a **security** property rather than a tidiness one. At the old cadence a per-frame
  line was 1,200 rows an hour; `audit.jsonl` rotates at 2 MiB and keeps exactly one backup, so
  roughly 57 hours of live viewing would evict the entire security history — every login, every
  kill, every password change — to make room for a timer. Of the fourteen places this codebase
  writes an audit line, thirteen are each bounded by one human action; the live preview was the
  only one a clock could drive, and so the only one whose volume nothing bounded. The coalesced
  line is also the more useful record: it says the screen was watched for forty minutes and looked
  at closely five times, rather than repeating one sentence 1,200 times.
- Further append-only logs live beside it with independent retention: `usage.jsonl` (usage
  history — session edges, countdowns, enforcement actions — read-only via `GET /api/usage`),
  `screentime.jsonl` (one rollup row per completed day, read-only via `GET /api/screentime`; kept
  in its own file so the higher-volume events in `usage.jsonl` cannot rotate the daily history
  away, whether that volume is incidental or deliberately generated),
  `time_requests.jsonl` (the event-sourced approval queue), and `time_codes.jsonl` (issued/
  redeemed time codes). A small `usage_state.json` sidecar holds the rules enforcer's running
  daily tally so a mid-day reboot doesn't reset the budget. It is saved on the enforcer's 30-second
  tick whenever the tally changed, which **bounds what a hostile reboot can win at under half a
  minute** — less than the reboot itself costs, so cutting the power is not a way to buy screen
  time. That bound is the reason the interval is short; a longer one would turn "reboot, gain the
  interval, repeat" into a real bypass. (Ticks where the tally didn't change — an idle or locked
  session accrues nothing — write nothing, which is a cost saving only and doesn't move the bound.) The security audit log records
  `time_code_issued` (minutes only — never the code) and `time_code_redeemed` (with source IP).
  All of these inherit the data dir's SYSTEM+Administrators-only ACL, and none contains secrets
  (no password, cookie, or hash).
- **Crash-safe writes.** `config.json` and the `usage_state.json` tally are written atomically
  (temp file → `fsync` → rename), so a power cut mid-write can't leave a truncated file. This
  matters most for `config.json`: a corrupt config would stop the service from starting and lock
  the parent out until reinstall.

---

## Pairing tokens (`GET /p/{token}`)

`install` and `nestwatch pair` print a QR whose URL grants a logged-in session when opened — so
the parent can reach the dashboard from a phone without typing an IP address or a passphrase.
This is deliberately a **password bypass**, so it's bounded tightly:

- **Single-use.** Redeeming unlinks the token file; `remove_file` is the atomic step, so of two
  concurrent scans exactly one can win.
- **Short-lived.** 15 minutes, then it's refused *and* deleted.
- **Only a hash is stored.** `pairing.json` holds a SHA-256, never the token, so reading the file
  (which needs SYSTEM/Administrators anyway) doesn't yield a usable token.
- **One at a time.** Minting overwrites any pending token, so `pair` can't leave two live QRs.
- **LAN-gated and throttled.** Same `require_lan_peer` layer as everything else, and a wrong
  token counts against the *same* per-IP login limiter — so the 80-bit token can't be ground at
  speed. It always redirects to `/`, never revealing whether a pairing is pending.
- **Not left behind.** `uninstall` clears any pending token.

**The residual risk, stated plainly:** the QR is displayed on a console *on the child's own PC*.
For those 15 minutes, someone standing at that screen could photograph and use it. The mitigation
is procedural — scan it yourself while you're at the machine, which consumes it immediately — and
the exposure is no worse than the elevated console session the install already required. If you'd
rather not have the window at all, ignore the QR and sign in with the password; the token simply
expires unused.

## The child's unauthenticated surfaces (by design)

Four routes are reachable **without a login**, by design, so the child can act from their own
(non-parent) session — they sit on the outer router, *before* `require_auth`:

- `GET /ask` — the child's page: how much time they have left, plus request/redeem forms.
- `GET /status` — the numbers behind that page. Deliberately narrow: `limited`, `budget_mins`,
  `used_mins`, `remaining_mins` and nothing else — no blocklist, no per-app limits, no app
  groups, no curfew window, no queue contents. A child is entitled to know their own limit; they
  are not entitled to a map of the rules to plan around. Rate-limited (30/min/IP) because each
  call reads a file on the shared blocking pool. There's a test asserting the response contains
  none of the rule fields.
- `POST /time-request` — submits `{minutes, reason}` to the parent's approval queue.
- `POST /redeem-code` — cashes in a parent-issued time code (see below).

(`GET /p/{token}` is also unauthenticated, but it's parent-facing — see *Pairing tokens* above.)

This is **not** a hole in the "everything is auth-gated" model, because each surface is bounded
on every axis:

- **LAN-gated** by the same `require_lan_peer` outer layer as the controls (`src/server.rs`) —
  an off-LAN client gets `403` here too.
- **Rate-limited** by *separate* per-IP `SubmitLimiter`s (`src/timereq.rs`, 5/min/IP) — one for
  requests, one for redemptions — that count **every** call (unlike the login limiter, which
  counts only failures), so a child can neither flood the parent's queue nor rapidly guess codes.
- **Request is powerless on its own**: `POST /time-request` only *enqueues a request* (always
  answering `{ok:true}`, leaking no queue state). No screen time is granted until the **parent
  approves it** (`POST /api/time-requests/{id}/approve`). Input is bounded (1–240 minutes; reason
  truncated to 200 chars; at most 5 pending requests).

### Time codes (`POST /redeem-code`)

A time code *does* grant screen time without a live parent action — that's the point (leave a
code for when you're away). It's safe because:

- **The code is the capability, and it's unguessable.** Codes are 8 Crockford-base32 characters
  (~1.1 trillion combinations) from the OS CSPRNG. At the 5/min rate limit, brute-forcing one is
  infeasible (millennia), and the limiter throttles rapid guessing regardless.
- **The parent hands the code over deliberately** — there's no interception threat; the parent
  chooses when and to whom to give it.
- **Single-use and bounded**: each code is worth 1–240 minutes, is consumed on first redemption
  (event-sourced `redeemed` line), and at most 50 can be outstanding.
- **Plaintext codes never leave the ACL'd data dir** (SYSTEM+Administrators only), so the child
  can't read the list; they're deliberately **not** written to the audit log either.
- **Minimal feedback**: redemption returns only `{ok, minutes}` on success or `{ok:false}` on a
  bad code — no other state.

Net: at worst, any LAN device can add up to 5 pending lines to a queue the parent reviews, or
redeem a code the parent already chose to hand out — it cannot see or change anything sensitive.

---

## How to verify your install is sound

1. **The binary itself, before you run it.** It will run as SYSTEM, so this comes first:
   ```powershell
   Get-FileHash nestwatch.exe -Algorithm SHA256 | Format-List   # match the .sha256 file
   gh attestation verify nestwatch.exe --repo emrecdr/nestwatch
   ```
   The hash detects a corrupted download and nothing more — it is published by the same workflow
   as the file, so anyone able to publish a release could publish a matching hash. The
   attestation is the one that establishes origin. See *Supply chain* below.
2. **Cert fingerprint** — the first time a browser warns, compare its certificate SHA-256 to
   the fingerprint `install` printed. They must match; if they don't, you may be talking to an
   impostor on the network.
3. **Firewall** — the network profile on the PC must be **Private** (not Public) for the
   LocalSubnet rule to apply. `install` warns if it couldn't add or read back the rule; heed
   that warning (the app-layer allowlist still protects you, but the firewall is the outer
   layer).
4. **Standard user** — confirm the child's Windows account is a *standard* user, not an
   administrator; the tamper resistance depends on it.
5. **Run `nestwatch doctor`** — it checks all of the above that can be checked automatically (the
   service, the listening port, the firewall rule and its scope, the network profile, certificate
   expiry, whether anything is actually being enforced, and who the local administrators are) and
   prints a fix under anything wrong. Run it elevated: the data directory is locked to
   Administrators, so an ordinary console can't read the config or certificate and those checks
   are reported as unknown rather than guessed at.
6. **Access log** — after logging in, open **Recent access** and confirm you only see your own
   sign-ins.
7. **Child page** — open `https://<this-pc>:<port>/ask` and confirm it shows only the request
   form: no controls, no screen, no data.

## What is recorded about the child, and what is not

The tally and the report answer "how long", and — since foreground tracking — "at what". This is
the most personal data the system holds, so it is worth stating exactly.

**Status: designed and built, never run.** The half of foreground tracking that touches Windows —
the watcher itself — has been compiled, linted and cross-checked, and has never executed on a real
machine. What follows describes what the code is written to record. Treat it as the design's
promise rather than an observation until
[WINDOWS-TESTING.md](WINDOWS-TESTING.md) has been walked through on the device.

**Per-app foreground time records process names.** The watcher emits a `foreground::Sample`, whose
`apps` map is normalized process names (`"roblox.exe"`) to seconds. No path, no command line, no
document name.

**Browser time is recorded by page title, and that is a deliberate trade.** The same sample's
`pages` map holds the *page title* a browser window was showing — `"Roblox"` from
`"Roblox - Google Chrome"` — for the intervals when a browser was in front. This is more personal
than a process name and less than a browsing history:

- **It is a title, never a URL and never a domain.** Nothing resolves, logs, or infers the address.
  Getting domains would mean writing browser policy into `HKLM` to disable each browser's own DNS
  resolver; that was considered and declined, because reconfiguring a child's browser belongs in
  front of a parent as a decision rather than inside an installer as a detail.
- **It is capped at 40 titles per report** (`foreground::MAX_PAGES`), heaviest first, and capped
  again everywhere the figures come to rest. The cap exists because titles are the
  highest-cardinality thing here and arrive from a process running as the child — without it, a
  script retitling a window in a loop grows the tally file without bound. It also means the record
  is a summary of where the time went, not a log of everything opened.

  Two companion limits exist for the same reason, and are worth stating because the obvious bound
  is not sufficient on its own. Checks on *what the numbers may say* — no app can claim more
  seconds than the tick lasted, and the sum cannot either — say nothing about **how many** of them
  there may be, so an adversary simply switches axes: instead of one app claiming 9,000 seconds,
  ten thousand apps claiming one second each, every value individually plausible.
  - **`foreground::MAX_APPS` (200)** bounds the *count* of executables held in memory and stored
    for a day. Set far above any real machine, because it is a backstop against a forged report
    rather than a product decision. Without it the persisted tally grew to 6,000 invented names
    under test.
  - **`foreground::MAX_LINE` (1 MiB)** bounds a single line read from the watcher pipe. A writer
    that never sends a newline would otherwise take the SYSTEM service's memory with it, and that
    service is what enforces the rules. Sized from the largest line an *honest* watcher can
    produce (170,170 bytes, measured and pinned by a test), because a limit set too low discards
    real samples as if forged.

  In each case the heaviest entries survive, so a flood costs the flood: an app with real hours
  behind it outweighs any number of one-second forgeries.
- **It is expected to see private-browsing windows too**, because browsers do not suppress window
  titles there. Expected, not confirmed: this is one of the open questions in
  [FOREGROUND-TRACKING.md](FOREGROUND-TRACKING.md), and it has not been checked on a real browser.
  Worth knowing either way before assuming Incognito is unobserved — and worth telling your child,
  since this project is overt monitoring, not surveillance.
- **It is reported, never enforced.** No limit, lock or shutdown is ever decided by a page title.
  The watcher runs inside the child's session, so anything he can influence must not decide
  whether his machine locks.

`tests/privacy.rs` pins the shape of what is emitted: process names and page titles keyed to
second counts, and nothing else. A new field fails it, so the paragraph above cannot drift from
the code without somebody noticing.

**Two Windows APIs the dependency list makes look worse than they are.** `Cargo.toml` enables
`Win32_UI_Accessibility` and `Win32_UI_Input_KeyboardAndMouse`, both of which sound like the
keylogging this project refuses. They are:

- `SetWinEventHook` (Accessibility) — a notification when the *foreground window changes*. It
  reports which window came to the front, not anything inside it.
- `GetLastInputInfo` (KeyboardAndMouse) — the timestamp of the last input, used to tell "at the
  keyboard" from "away from it". It reports **how long ago** something was pressed, and cannot
  report what: the API returns a tick count and nothing else. Microsoft scopes it to the calling
  session, so it says nothing about other users either.

Neither installs a keyboard hook, and nothing anywhere reads key state. The design reasoning is in
[FOREGROUND-TRACKING.md](FOREGROUND-TRACKING.md); this section exists so the answer is also where
someone checking the privacy claim will look.

## Outbound connections

There are none from the monitored PC. It listens on one port and contacts nothing — no update
check, no telemetry, no licence call, no crash reporting. `src/` contains no HTTP client; the only
outgoing socket anywhere is `doctor`'s probe of `127.0.0.1` to confirm the service bound its port.
A test pins this by asserting the Content-Security-Policy names exactly one external host and that
`default-src` stays `'none'`.

That one host is `api.github.com`, and the distinction matters: it is reachable **from the
dashboard page**, which runs in the parent's browser on the parent's own device. The button that
uses it ("check for a newer version") fetches the release list from *there*, not from the child's
PC. Nothing is requested on page load, so opening the dashboard contacts nobody, and declining to
press it leaves the behaviour exactly as before.

Why it was not built the obvious way: a version check *in the service* would have the monitored
machine contact GitHub, revealing the household's address and roughly when that PC is awake — a
presence signal about a child's computer, sent to a third party, for a convenience. The
information is the same either way; the difference is which machine is observable, and that is
the whole claim.

There is no auto-updater either, for a separate reason: it would be a path that writes an
executable and runs it as SYSTEM, which is a component class with a poor record — including an
unauthenticated RCE as SYSTEM in Microsoft's own WSUS (CVE-2025-59287) and several local
privilege-escalation flaws worded as *an authorised local attacker elevates privileges*, which
describes the child on this machine. [REMOTE-UPDATE.md](REMOTE-UPDATE.md) covers installing a new
build over the network instead, and why the usual home-network remoting advice (NTLM over WinRM
HTTP with `TrustedHosts`) is specifically unsafe against an adversary who is already on the LAN.

## Supply chain

Every layer above assumes the program running is this program, built from this source. That
assumption is the one thing no runtime control can check, so it is established at build time.

- **Releases carry a signed build-provenance attestation** (SLSA v1, via `actions/attest`),
  binding the exact artifact to this repository, this workflow and this commit. Verified with
  `gh attestation verify nestwatch.exe --repo emrecdr/nestwatch`. It fails closed: the lookup is
  keyed on the file's own digest, so a modified binary is not a signature that fails to match —
  it is a file GitHub was never asked to sign, and there is nothing to forge.
- **The published `.sha256` is not a substitute.** It is generated by the same workflow that
  publishes the binary, so it proves the download was not corrupted in transit and nothing about
  where the file came from. Both are listed in the install guide; only one establishes origin.
- **Dependencies are gated on every push and once a week**, not at release time: `cargo deny
  check advisories sources licenses bans`, resolved against the shipped Windows target so the
  check can be blocking with no ignore list. The licence allow-list excludes GPL/AGPL/LGPL, which
  is what keeps the dependency tree compatible with the MIT licence this ships under.
- **The weekly run exists because the push-triggered one cannot see time passing.** An advisory
  published against a version already pinned changes nothing in the repository, so nothing
  triggers and nothing runs. For a tool installed once and then left alone for months, that made
  the detection window "however long since the last push".
- **Actions that hold power are pinned to commits, not tags** — the one that executes downloaded
  binaries, the one that can publish releases, and the one that holds the signing identity. A
  moved tag on any of those would be arbitrary code execution or an unauthorised publish.
- **Pinning is paired with Dependabot**, because a pinned commit never moves on its own,
  including past an advisory. The pin removes the moved-tag risk; the updates stop it becoming
  a frozen one. Dependabot **alerts** and **automated security fixes** are both enabled on the
  repository, so an advisory opens a pull request by itself — and that pull request runs the gate
  above immediately, which is the fast path. The weekly schedule is the backstop for whatever
  that misses.
- **Not claimed:** the binary is not code-signed with an Authenticode certificate, so Windows
  SmartScreen will warn on first run and the file needs unblocking. Buying a signing certificate
  for a tool with one user is not proportionate — the attestation is the stronger check anyway,
  and it is free.

## Resisting the child's own privileges

Tamper resistance against a standard user is mostly the SYSTEM service + ACLs described in the
README. One case needed its own defense, because Windows grants the privilege by default:

- **Changing the time zone** (`SeTimeZonePrivilege`) is granted to the **Users** group with no UAC
  prompt. Every time-based decision here — when the daily budget resets, whether the curfew window
  is open — read the OS clock, so flipping between two zones a day apart reset the day's tally on
  every flip (repeatable every 30 seconds) and moved wall time out of the curfew window, which
  makes the enforcer *cancel* a pending shutdown.
- Time is now anchored to **UTC** — which that privilege cannot move; changing the clock itself
  needs `SeSystemtimePrivilege`, which standard users don't hold — plus an offset recorded at
  install (`tz_offset_mins`). The OS offset is still followed while it stays within an hour of the
  anchor, so genuine DST transitions work; larger jumps are ignored and logged. The comparison is
  always against the stored anchor, never the previous reading, so induced drift is bounded at one
  hour and can't be walked forward.
- Independently, the enforcer refuses more than one day rollover per 12 monotonic hours, so the
  tally survives even if the clock is wrong for some other reason.
- **The anchor is recorded at install time.** An install upgraded in place from before this existed
  has no anchor and falls back to plain local time; re-run `install` to anchor it. Deliberate:
  guessing an offset for a machine that may have genuinely moved would be worse than not guessing.

Two related enforcement gaps closed the same way — by not letting a dodged action reset the
enforcer's state: locking the screen (`Win+L`) no longer earns a fresh grace period, and a
`shutdown /a` no longer earns a fresh cancellable countdown (the re-issue has no delay).

- **System binaries are invoked by absolute path** (`src/syspath.rs`), never by bare name. Rust
  resolves a bare `"shutdown"` by searching **the directory of the current executable before
  `System32`** (it does not search the working directory). For the installed service that
  directory is `C:\Program Files\HostHealth\`, which is ACL-locked, so the service itself was
  never exposed. The reachable case was `install` and `doctor`, which run **elevated** from
  wherever the parent left `nestwatch.exe`: a `netsh.exe` or `icacls.exe` planted beside it in a
  child-writable folder (`C:\Users\Public\Downloads`, a shared folder, a USB stick) would have run
  as administrator. Resolving through `GetSystemDirectoryW` removes the search entirely rather
  than relying on which folders happen not to be writable.

## Residual risks (honest limits)

- **Self-signed MITM** if the fingerprint is never verified (see §2).
- **A device that has the Wi-Fi password is "on the LAN."** The allowlist scopes to the local
  network, not to specific devices; the password is what gates control from there, so use a
  strong one.
- **The child's `/ask` / `/time-request` endpoint is reachable without a login** — intentionally,
  so the child can request time. It is LAN-gated, rate-limited (5/min/IP), input-bounded, leaks
  no state, and grants nothing without parent approval. The residual exposure is that any LAN
  device can add up to 5 pending request lines to the queue; the parent simply denies spam.
- **The app blocklist and per-app limits are evadable by renaming.** Matching is on the process
  image name, so copying `chrome.exe` to `notes.exe` in a writable folder escapes every app rule.
  This is inherent to name-based matching from user space: matching on full path or file hash
  raises the bar slightly but loses to a re-copy, and nothing short of kernel-level enforcement
  (AppLocker/WDAC by publisher or hash, or Microsoft Family Safety) actually closes it. Treat app
  rules as habit-shaping, and rely on the **daily budget and curfew** — which are not name-based —
  for limits that must hold. The README says the same thing to the parent.
- **Screen-time figures are per-machine, not per-account.** The tally follows whoever is at the
  console, so a parent signing in to do their own work adds to the child's totals. This is
  conservative for enforcement — it cannot be dodged by switching users — and misleading for the
  report, which says so on the card. Attributing per-account is possible (the console session's
  username is already read and discarded) but changes the `SystemControl` trait; tracked as O6.
- **Enforcement still counts an app while it is *running*, not while it is focused.** Focused
  minutes are now *measured* and reported alongside the running figure (see below), but nothing
  is enforced on them, and that is deliberate rather than unfinished: the watcher runs inside the
  child's own session, so a figure he can influence must never decide whether his PC locks. So an
  app left open in the background still consumes its per-app limit and its group's pool, which
  makes per-app limits impractical for anything that auto-starts. A usability limit rather than a
  bypass — and it pushes parents toward the total budget, which is the control that resists
  tampering best anyway.
- **Enforcement may not cover every way the machine can be started.** Whether it does is an open
  question about the specific device, not a settled property of this software, so do not assume a
  reboot cannot get around it. The specifics, the fix, and the check are tracked outside this
  public repository — see `docs/private/OPERATIONAL-FINDINGS.md`.
- **A wedged enforcer is reported but not repaired.** A panic restarts the service, but a tick that
  hangs leaves enforcement off until someone looks at the dashboard or runs `doctor`. See O4.
- **Local administrator on the PC** can defeat any of this — out of scope by design.
