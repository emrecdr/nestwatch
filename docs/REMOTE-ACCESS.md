# Reaching the dashboard from outside the house

**This document changed direction on 2026-09-02.** It used to say that off-LAN access was not a
feature, was not supported, and would stay that way. Remote reach is now wanted, and the decision
taken is **full dashboard parity over a tunnel terminated on the home router** — no second
always-on machine, no third-party relay, no domain name.

What follows is the research behind that, in the order it matters: the two prerequisites that can
rule the whole plan out before any work starts, what the LAN gate is actually holding up, why the
chosen transport keeps most of that intact, and the one thing it does not.

The old advice survives the change and is now the supported shape rather than the tolerated one:
**do not give Nestwatch a way out. Give yourself a way in.** Done that way the monitored PC still
makes no outbound connection, which is the promise the whole design rests on.

---

## Four prerequisites, and the first is not negotiable

Check all four before spending time on anything below. Two of them can end the plan, and the
fourth decides where the tunnel has to terminate — all four are minutes of work, and every one of
them is cheaper to answer now than to discover halfway through.

### 1. Your child must not be a local administrator

The same prerequisite as [REMOTE-UPDATE.md](REMOTE-UPDATE.md), for the same reason, and it is not
a formality. Run this on the PC:

```powershell
Get-LocalGroupMember -SID S-1-5-32-544 | Select-Object Name
```

If his account is listed, **stop here.** A remote path into your network is worth having only if
the thing at the end of it cannot be switched off by the person it monitors. An administrator can
stop the service, and no tunnel design compensates for that. Remote access makes this *more*
load-bearing, not less, because you will be physically present less often.

### 2. You must not be behind CGNAT — **blocking**

If your ISP hands you a shared public address, nothing from outside can reach your house and a
router-terminated tunnel is impossible. Thirty seconds settles it:

1. Look at the WAN/Internet address in your router's status page.
2. Compare it with what <https://whatismyip.com> reports.

**Different → you are behind CGNAT.**

If you would rather not log into the router, the shell answers it too, and gives you prerequisite 4
in the same breath:

```sh
curl -s https://api.ipify.org; echo          # what the internet sees
traceroute -n -m 4 8.8.8.8                   # what you are behind
```

An address in **`100.64.0.0` – `100.127.255.255`** is carrier-grade NAT, conclusively. So is a
`100.64/10` hop in the trace. If the first public hop shares a `/24` with your public address, that
is your ISP's gateway one step beyond your own router — which is what *not* being behind CGNAT looks
like. Every arrangement that tolerates CGNAT needs either a second
always-on machine or a third-party coordinator, and both were ruled out. This does not mean the
decision was wrong; it means it has to be reopened rather than worked around.

### 3. Your router must have a VPN server — **blocking**

"Router config only" means the router terminates the tunnel, so it has to be able to. In order of
preference:

| What the router has | Verdict |
|---|---|
| **A WireGuard server** | The one to want. See option 1 below for why it is not merely a preference. |
| An IPsec or OpenVPN server | Works. Judge it on the same two questions: does the traffic arrive on the home subnet, and does the monitored PC have to run anything? |
| Neither | **No path under this decision.** Running [PiVPN](https://pivpn.io) on a Raspberry Pi or NAS is the usual answer and it is a second always-on machine — the option that was declined. Replacing the router is the other. Both reopen the decision. |

**Expect "neither", because the common arrangement produces it.** An ISP-supplied box plus your own
router behind it — the [double-NAT case](#4-count-your-routers--a-second-one-breaks-this-quietly)
below — typically means *neither* box can terminate a tunnel. ISP units generally offer port
forwarding and no VPN server at all; consumer routers vary, and several popular lines have no
WireGuard **server** in stock firmware, only a VPN *client*, which is the opposite of what is needed
here. Checked while writing this: Xiaomi/MiWiFi stock firmware has no WireGuard server — it is
reachable on that hardware only by flashing OpenWrt, which is firmware replacement rather than
router configuration, and carries its own risk.

So "check the model" is not a formality. **Look for a VPN *server* page, not a VPN entry in the
menu**, and confirm it offers WireGuard, IPsec or OpenVPN in the inbound direction. Where the answer
is neither box, the honest options are a second always-on machine, replacing the inner router with
one that has a server, or — if the house has a public address — the embedded endpoint in option 4,
which is the only one that needs no new hardware.

### 4. Count your routers — a second one breaks this quietly

Two NAT layers in series is ordinary: the ISP's box, plus your own router behind it. It does not
stop remote access, but it decides **where the tunnel has to terminate**, and when it is wrong the
failure does not look like a topology problem. Traced on a real home network while writing this:

```sh
traceroute -n -m 4 8.8.8.8
```
```
 1  192.168.31.1     <- your gateway
 2  192.168.178.1    <- a SECOND router, and the one holding the public address
 3  62.163.230.1     <- the ISP
```

**Every RFC1918 address before the first public hop is a router you are behind.** One is normal.
Two means the PC and the tunnel endpoint are on *different private subnets*, and the tunnel has to
terminate on the box that holds the public address — the outer one — because that is the only one
the internet can reach.

**Why this is worth its own check: the failure is invisible to every gate in this program.** A peer
arriving on the outer subnet (`192.168.178.x`) is RFC1918, so `is_lan` admits it. The firewall rule
is scoped to `LocalSubnet`, which is the *inner* subnet, so it rejects the packet — and if that rule
is absent or the profile has flipped, the request simply never arrives, because nothing routes into
the inner subnet at all. Nothing logs "wrong subnet". The parent sees a tunnel that connects and a
dashboard that does not load, which reads as the tool being broken.

Two ways out, and they are the router's job rather than Nestwatch's:

* **Put the PC's subnet on a route.** A static route on the outer router for the inner subnet, and
  the inner router not translating addresses (bridge or access-point mode). Then one subnet exists
  and everything above applies unchanged.
* **Terminate the tunnel on the inner router**, if it has a VPN server, and forward one UDP port to
  it from the outer box. The peer then lands on the PC's own subnet directly.

Collapsing to a single router is the better fix where it is possible, because double NAT costs
something every day and buys nothing here.

---

## The constraint that decides every option

Nestwatch refuses any client whose source address is not on a private network, in the application
itself — before authentication, before the session layer, before anything
(`security::require_lan_peer`):

```rust
IpAddr::V4(v4) => v4.is_private() || v4.is_loopback(),
```

RFC1918 (`10/8`, `172.16/12`, `192.168/16`) plus loopback. Everything else gets a `403`. This is
deliberate belt-and-suspenders — the OS firewall is the outer gate, this is the inner one, and it
keeps working if the firewall rule is deleted or the network profile flips to Public.

It also means **an option only works if your traffic arrives wearing a private address**, which
eliminates most of the popular answers. That is the intended fail-closed direction, and it is the
reason the recommendation below needs no change to Nestwatch at all.

### What that one line is holding up

This is the part that was missing from earlier versions of this document, and it is the reason the
transport choice matters more than it looks. Over time the rest of the codebase borrowed the LAN
gate as a *premise*. Five separate security arguments depend on an attacker being unable to reach
the port. None of them live near the gate. **None of them fails a test when the gate opens.**

Each was checked by reading the cited line; three are the project's own words.

| | The argument | Where |
|---|---|---|
| **1** | The eight-character password minimum: *"This password guards a LAN-only service behind an Argon2id hash with per-IP throttling — an attacker has to already be on the home network to try it at all."* | `src/auth.rs`, on `MIN_PASSWORD_LEN` |
| **2** | All-digit passwords allowed: *"eight digits is 10^8, and against Argon2id behind per-IP throttling that is not the weak link."* | `src/auth.rs`, on `guessable` |
| **3** | Six-character time codes: *"That rate limit is the primary defence, not a secondary one… Loosening or removing the limiter therefore changes the security of time codes directly."* The bucket is keyed on **source IP**. | `src/api.rs`, on `redeem_code` |
| **4** | `/ask`, `/status`, `POST /time-request` and `POST /redeem-code` are **unauthenticated**. Their comments name the protection exactly: *"LAN-gated (outer router → require_lan_peer) and per-IP rate-limited."* | `src/server.rs` |
| **5** | A device paired for the dashboard holds the parent's entire authority, and only geography bounds it. **Narrowed by `8d5f5c3`**, which gave pairing a scope: `Scope::Integration` reaches two endpoints, `Scope::Dashboard` still reaches everything. **Closed by `be1c07e`/`f6687d8`**, which gave a session a device identity and a per-device revoke, so one lost phone no longer costs every device. | `docs/SECURITY.md`, `src/pairing.rs`, `auth::integration_may_reach`, `sessionstore::revoke` |

`SECURITY.md` puts *"exposure to the public internet (the tool is LAN-only by design)"* out of
scope. That sentence is the assumption the whole threat model was written under, and it is worth
naming as an assumption rather than a boundary.

---

## Why the router tunnel keeps four of those five intact

The table above is the **general** case — what happens if the boundary is deleted, which is what
port-forwarding the dashboard would do. A tunnel terminated on the router does not delete it. It
**substitutes a cryptographic boundary for a topological one.**

WireGuard does not reply to unauthenticated packets at all — no handshake, no error, nothing. An
attacker who is not a configured peer cannot elicit a single packet, and a port scan cannot tell
the tunnel is there. The dashboard is never on the public internet; it is on a network whose
membership is decided by a keypair instead of by a wall socket.

So arguments 1 through 4 hold, restated:

- *"an attacker has to already be on the home network"* becomes *"…or holds a peer key"*, which is
  a stronger statement, not a weaker one.
- The per-IP limiter keeps bounding what it bounded, because a remote attacker cannot reach
  `/redeem-code` to be throttled in the first place. **This is the one that would have broken
  worst under an exposed port**: the throttle is keyed on source address, so internet
  reachability would not loosen the limiter in code — it would make the key cheap to vary, which
  has the same effect while changing no line the limiter is written on.
- The four unauthenticated child routes stay behind a gate that still means something.

**Argument 5 does not survive, and full parity makes it the story.** It was never about the network
— it is about what a single device holds once it is through, which no tunnel improves.

**It is smaller than it was.** `8d5f5c3` closed `O89` by scoping pairing at mint time: an
integration token reaches `POST /api/extra-time` and `GET /api/usage/today` and nothing else, and
its grants are attributed to the integration whatever the request body claims. A third-party app on
the child's device is no longer inside the whole table. **What that does not narrow is the case this
document is about** — the parent's own phone pairs for the *dashboard*, so it still carries
everything, and remote access is what removes the geographic bound on it.

---

## What full parity puts on one device

Under the decision taken, one phone carries three things at once:

- **The WireGuard private key.** WireGuard has no passwords; possession of the key *is* the
  authorisation.
- **A session cookie**, valid for 30 days of inactivity (`Expiry::OnInactivity`, `server.rs`).
- **Everything a `Scope::Dashboard` session can do** — the live screen, the complete recorded
  history through `GET /api/export`, shutdown, the provider registry, and the control password
  itself. Scoping bounds an *integration*; it does not bound the parent's own phone, which is the
  device this section is about.

Today the bound on that is geography: whoever takes the phone has to be in the house for it to be
worth anything. Remote access removes the bound, so something else has to replace it.

Two gaps compounded this when the document was written, and **both are now closed** — they are kept
here because they are the reason the work was ordered the way it was:

- **There was no per-device session identity.** A session carried two keys and neither said which
  device held it. `GET /api/sessions` now lists devices by handle.
- **There was no per-device revocation.** `clear_all()` was the only lever, so revoking one lost
  phone signed out every device including the one in your hand. `sessionstore::revoke` now ends one
  alone.

What does **not** change is the concentration itself: a dashboard pairing is still unscoped, so the
phone still holds everything while you hold it. Revocation limits the damage after a loss; it does
not reduce what the device carries before one.

`SECURITY.md` is already explicit that a child holding a paired device is **not** handled. A
household phone is a plausible thing for a child to reach, and remote access means that device no
longer has to be in the house to be useful. Per-device revocation is a parenting feature here as
much as a security one.

**This is the work worth doing, and it is worth doing whether or not remote access ever ships.**

---

## What works

### 1. WireGuard on your router — recommended

Your phone joins the home network over an encrypted tunnel and gets an address on it. Nestwatch
sees an ordinary LAN client and behaves exactly as it does from the sofa.

**Why this one.** A single UDP port, silent to anything that is not already a peer (see above).
Compare an exposed HTTPS or SSH port, which announces itself and invites guessing. No third party
in the path, and nothing installed on the monitored PC.

**It needs no change to Nestwatch, and that is the point.** The certificate's SANs are this
machine's LAN address and hostname (`cert::reachable_hosts`), so over the tunnel you browse to the
*same* `https://192.168.x.y:8443` you always did — certificate matches, origin unchanged, the
`SameSite=Strict` cookie and the `Sec-Fetch-Site` check behave identically. An address the tunnel
itself assigns (typically `10.x`) is also RFC1918, so either topology passes the gate.

Many home routers have this built in. On a FRITZ!Box (FRITZ!OS 7.39+) it is
**Internet → Permit Access → VPN (WireGuard) → Add Connection**, which prints a QR code for the
phone app. ASUS, MikroTik, UniFi, OpenWrt and OPNsense/pfSense all have equivalents.

### 2. Your router's own VPN

Same principle, if the box already has an IPsec or WireGuard server and you would rather not add
hardware. Judge it on the same two questions: does the traffic arrive on the home subnet, and does
the monitored PC have to run anything? A router VPN answers well on both.

### 3. Tailscale as a *subnet router* — the CGNAT answer, now out of scope

Run Tailscale on a **separate always-on machine** and advertise the home subnet from it. Tailscale
masquerades routed traffic to that machine's own LAN address by default, so requests reach
Nestwatch from `192.168.x.x` and pass the gate. It also traverses CGNAT.

**Recorded, not recommended, under the current decision** — it needs the second always-on machine
that was ruled out. It is the answer to reach for if prerequisite 2 or 3 above fails, and the
reason it is still here.

**Tailscale installed on the monitored PC does not work, and should not be made to.** It assigns
from the carrier-grade-NAT range `100.64.0.0/10`, which is not RFC1918, so the app-layer gate
returns `403`. Widening the gate to admit that range was considered and declined twice — see
[DECLINED-OPTIONS.md](DECLINED-OPTIONS.md). The five arguments above are why that decline should be
read as load-bearing rather than fussy.

### 4. A WireGuard endpoint inside Nestwatch — costed, and the answer where 3 fails

Nestwatch listens on UDP itself; the parent's phone is a peer; traffic arrives from RFC1918 and
passes the gate unchanged. It converts the forbidden move — port-forwarding the dashboard — into a
defensible one, because what gets forwarded is silent to everything that is not a peer.

**It does not break the promise this project rests on.** *Do not give Nestwatch a way out; give
yourself a way in.* An inbound listener is a way **in**: the monitored PC still makes no outbound
connection of its own. And it does not falsify the five arguments above, for the same reason the
router tunnel does not — a peer is cryptographically authenticated before a byte reaches the HTTP
stack, and arrives wearing a private address.

**Where prerequisite 2 passes and prerequisite 3 fails — a public address, and neither router able
to terminate a tunnel — this is the only option that needs no new hardware and no firmware
replacement.** The routers are asked for nothing but a forwarded UDP port, which even ISP-supplied
boxes do. That combination is common, so this is not an exotic fallback.

#### What it costs, measured rather than estimated

| | |
|---|---|
| **RAM** | Dominated by Wintun's two shared-memory rings: `2 × (capacity + 64 KiB)`. Capacity is a power of two between `WINTUN_MIN_RING_CAPACITY` (**128 KiB**) and `WINTUN_MAX_RING_CAPACITY` (64 MiB). At the minimum that is **~384 KiB**; at a comfortable 1 MiB it is ~2.1 MiB. This service's heaviest payload is a preview screenshot of about 23 KiB, so the minimum is the right starting point. Per-peer session state is kilobytes. **Under 1 MB in total.** |
| **CPU** | Idle with no peer: one UDP socket receive, effectively nothing. Idle with a peer: a 32-byte keepalive every 25 s. Active: ChaCha20-Poly1305 runs at gigabytes per second per core, against a dashboard session measured in tens of kilobytes per second. Userspace WireGuard's real ceiling is per-packet syscall overhead, in the hundreds of Mbps — orders of magnitude above anything this serves. |
| **Binary** | Measured, not guessed: `boringtun` added **~33 KiB** to a release binary. That probe exercised the key path only, so read it as a lower bound; the crate is small and the full protocol state machine will not change the order of magnitude. |
| **Dependencies** | **22 crates** this tree does not already carry, including `curve25519-dalek`, `chacha20poly1305`, `x25519-dalek` and `nix`. All reputable, and it is still a real increase in the audit surface of a security tool with a supply-chain gate in CI. |

#### The costs that are not RAM

**It ends the single-binary install.** `wintun.dll` ships beside the executable, and the README's
install story is currently one file. That is the largest product cost here and it is not
recoverable by tuning anything.

**The licence is clean, and this was checked rather than assumed** — it decides whether the option
is possible at all, because this project is MIT. Wintun's *source* is GPLv2, but the **prebuilt
signed binaries carry their own terms permitting redistribution alongside a third-party
application**, with no fee and **no copyleft obligation on the accompanying software**. The single
condition is that the application use the driver only through the documented `wintun.h` API.
Microsoft's kernel-driver signing is satisfied by WireGuard LLC's signature, so no EV certificate
and no attestation cost falls on this project.

**Where the DLL lives is a security decision, and this repository already ruled on it.** Wintun's
own documentation says to install it *"side-by-side with your application"*. `syspath.rs` exists
precisely because, in Windows' search order, **the application's own directory outranks
`System32`** — that module was written to stop a look-alike beside the executable being run with
administrator rights. The same rule applies here and is not optional:

* Load it **by absolute path from the ACL-hardened install directory**, which `install` already
  locks to SYSTEM + Administrators with Users read-and-execute only. Never by bare name.
* The **service** loads it. `install` and `doctor` run elevated from wherever the parent left
  `nestwatch.exe` — a directory the child may be able to write — which is the exact window
  `syspath` documents.

**It is not the thing `PLUGIN-SYSTEM.md` refuses, and the difference has to be written down.** That
document rejects *"native dynamic loading (`.dll` the service `dlopen`s)"* as its first
architecture, on the constraint that no foreign code may run as SYSTEM from a source the child can
influence. A Microsoft-signed driver shim, placed by the installer into a directory the child
cannot write, is not that. Left unstated, the next reader will either revert this as a violation of
a rule the project already made, or — worse — cite it as precedent for loading something that *is*
child-influenceable.

**One coupling to the shared install path**, and it is the reason this cannot be purely additive:
the firewall rule is `remoteip=LocalSubnet`, and a tunnel subnet is not the local subnet. A peer
would be admitted by `is_lan` and dropped by the firewall, with nothing logging the disagreement —
the app-layer gate is never reached, and the firewall does not explain itself. Two gates written
against the same assumption at different widths, agreeing today only because every client really is
on the local subnet. Widening one and not the other yields a silent failure rather than a refusal,
and the narrower of the two is `#[cfg(windows)]`, so no host test can see it.

#### How to make it genuinely optional

The question is whether a household that does not want this pays anything for it. Mostly no, and
the exceptions are worth being precise about.

**Free:** the DLL is loaded at runtime, not linked, so its absence makes the feature unavailable
and changes nothing else. The tunnel adapter, the private key and the UDP socket all come into
existence only when the feature is switched on, and the key is a new secret that belongs under the
same ACL treatment as the config.

**Not free:** the 22 crates are in the shipped artifact whether or not anyone enables the feature.
A Cargo feature flag only removes them if two builds are published, which costs release complexity
and hands users a choice they will get wrong. There is currently no `[features]` section in
`Cargo.toml` at all.

**The one genuine change in exposure:** with the feature on, the monitored PC accepts inbound UDP
from the internet. WireGuard answers nothing that is not already a peer, so a scanner cannot tell
the port is open — but it is a reachable code path on the child's machine that does not exist
today. That is the honest cost, it is per-install, and the feature must be **off by default**.

**Make "off" provable rather than assumed.** A test asserting that no socket is bound and no adapter
exists while the feature is disabled. This codebase has been caught more than once by a check that
ran, reported success, and demonstrated nothing.

---

## What does not work, and why

| | |
|---|---|
| **Port-forwarding the dashboard** | Does not work *and* is the worst idea here. The request arrives with a public source address and `is_lan` returns `403` — so you would have to weaken the allowlist to enable your own most dangerous option. That failure direction is deliberate. Put a child's screenshots on the public internet behind one password and the only thing between them and the world is that password. **Note the distinction this document now draws:** forwarding a *silent UDP tunnel endpoint* (options 1 and 4) is a different proposition and must not inherit this refusal by association. |
| **Cloudflare Tunnel, ngrok, and similar** | The tunnel daemon runs on your side and dials out, and TLS terminates at a third party. Screenshots of your child pass through someone else's edge in the clear. This breaks the promise in the way that actually matters, not on a technicality. |
| **TeamViewer, AnyDesk, Chrome Remote Desktop** | All require an outbound broker connection **from the child's PC** — precisely the thing this project refuses to do — and they hand over full interactive control of the desktop rather than the scoped dashboard. |
| **Tailscale on the monitored PC** | `403` from the app-layer gate, and it makes that machine dial out. See above. |
| **A relay operated by this project** | The most usable option by a distance, and it inverts the product's identity. It also means operating infrastructure carrying children's screen data, with the liability and data-protection obligations that follow. For a single-maintainer project this is the largest commitment available and the least reversible. |
| **Self-hosted mesh (Headscale, Nebula)** | Both are genuinely sovereign — no third party, own CA — and both fail the same test: the parent must run infrastructure. Nebula wants a CA to provision, host certificates to issue and rotate, and a lighthouse to keep available; Headscale is aimed at people with in-house operational capability. Strictly harder than option 1 for a family. |

---

## The plan

Ordered because each step is a precondition for the next, not a preference about what to do first.

**0 · Settle the two blocking prerequisites.** CGNAT and the router's VPN capability, above. Both
are minutes of checking and either can end the plan.

**1 · Give a session a device identity that can be revoked alone — `O77`. DONE** (`be1c07e`,
`f6687d8`). This was the security story under full parity, because the tunnel handles the network
and the phone holds everything else. It landed in two halves: `8d5f5c3` gave pairing a scope, so the
*authority split* exists, and then identity — `GET /api/sessions` lists signed-in devices by
**handle, never session id**, and `POST /api/sessions/{handle}/revoke` ends one alone
(`sessionstore::revoke`). `clear_all()` is no longer the only lever, so revocation is surgical
rather than total. The ordering held: this really was the precondition for everything else.

**2 · Productize the tunnel, and teach `doctor` to check it. HALF DONE, and the other half was
declined on the merits** (`2e72216`). The SNAT check is in: `doctor::masquerading_router` reads the
access log and tells a parent when their router has made it unable to tell devices apart. That is
the half true for *every* router, which is why this document put it above the others.

**Peer-configuration generation was deliberately not built**, and the reasoning is better than this
document's original framing. Generating a WireGuard peer config needs the router's key and endpoint,
which Nestwatch cannot know and would have to be told — so it is a templating feature wearing a
security feature's clothes, correct for one router and untestable for the rest. This document asked
for it; that ask was wrong, and the half that survived is the half that generalises.

**3 · Compensate on the session for the factors a domain would have given.** Declining a domain
name is a legitimate call; it avoids a registration, a renewal, and DNS credentials on the
machine. It does mean passkeys, HSTS and the installed web app all stay closed, so the control
password remains the only knowledge factor and now guards a full-parity surface reachable from
anywhere. The compensations available without a domain are all on the session rather than the
credential: a shorter expiry for sessions used through the tunnel, re-authentication before the
destructive and disclosive actions rather than before every action, and revocation the parent can
find *before* they need it. The 30-day window was chosen when a stolen cookie meant someone in the
house; that premise is what changed.

**DONE in the part that mattered most** (`3071152`, #21): the idle window now has a ceiling. A
session used daily used to live forever, because `Expiry::OnInactivity` resets on every request —
so "30 days" bounded neglect, never possession. The absolute cap is read from the device record
added in step 1 rather than tracked separately (`auth.rs`), which is the two steps composing rather
than accumulating.

---

## What the research settled

Recorded so none of it is re-proposed as though it had been overlooked. Checked 2026-09-02.

- **Passkeys are impossible against this service as it identifies itself today**, and this is
  structural rather than a maturity question. WebAuthn's Relying Party ID must be a *valid domain
  string*; IP addresses are excluded, and the working group settled that deliberately
  ([w3c/webauthn#1358](https://github.com/w3c/webauthn/issues/1358)). Nestwatch is reached at
  `https://192.168.x.y:8443`, so a passkey cannot be registered against it at all.
- **A hostname is what unlocks four things at once.** Passkeys, `Strict-Transport-Security`
  (deliberately absent today, with the comment *"revisit only behind a genuinely trusted cert"*),
  the installed web app that `DECLINED-OPTIONS.md` refused for exactly this reason, and the end of
  the click-through-the-warning habit. One change, four unblocked. Declined here for now; the
  chain is recorded because the next person to want any one of them will find the other three.
- **A real certificate for a private-IP host is available without inbound anything.** DNS-01 proves
  control of a *zone*, not reachability of a host: a publicly-resolvable name whose A record points
  at `192.168.1.42` can hold a Let's Encrypt certificate. The cost is owning a domain and holding
  DNS API credentials on the machine — a new secret the ACL model would have to cover.
- **Let's Encrypt IP-address certificates went generally available on 2026-01-15** and do **not**
  help. It is the obvious thing to reach for; issuance requires a *public* IP, and the monitored PC
  has an RFC1918 address.
- **No comprehensive self-hosted alternative exists.** A survey of the field turns up DNS filters
  (AdGuard Home, Blocky) and Linux-only time limiters. Every comprehensive competitor is a cloud
  service, and all of them treat remote access as table stakes. That is the pressure behind calling
  this necessary, and it is real.

---

## If you set up option 1 or 2

- **Ask whether your router routes or masquerades the tunnel traffic.** This was a footnote when
  off-LAN access was unsupported and it is not one now. If the router SNATs, every remote visitor
  collapses into one source address: `audit.jsonl` records the router instead of the device, and
  the per-IP limiter shares a single bucket across everyone arriving that way. The audit log's
  entire job is to make access visible, and this is the configuration that blinds it. Routed
  WireGuard keeps peers distinguishable; prefer it, and check rather than assume.
- **Dynamic DNS.** Your home address changes, so the phone needs a stable name to dial. FRITZ!Box
  includes MyFRITZ!; otherwise DuckDNS or No-IP. (This names the *router*, and is not the hostname
  that would unlock passkeys — that one has to name the PC and carry a certificate.)
- **Scope the peer with a firewall rule.** Note the distinction: `AllowedIPs` on the phone is
  split-tunnel convenience, and `AllowedIPs` on the server restricts *source*, not destination.
  Only a firewall rule limits what the peer may reach — ideally just the Nestwatch host and port.
- **One peer per device**, so losing one means revoking one. This matters more under full parity,
  and it is the half of revocation the router *can* do today while Nestwatch cannot.
- **Treat the phone as the key.** Losing it unlocked is losing a permanent route into your LAN, a
  window onto your child's screen, and every control in the dashboard — a dashboard pairing is
  unscoped by design. **Revoking it is now two separate actions, and you need both**: the device in
  the dashboard's own list (`GET /api/sessions` → revoke, which ends the session), and the peer at
  the router (which ends the network route). Neither does the other's job. Find out how to do both
  before you need to, not after.
- **The dashboard password still applies.** The tunnel gets you onto the network; it does not sign
  you in. Do not weaken the password because the network feels private now — and note that under
  full parity it is guarding more, from further away, than when its eight-character minimum was
  chosen.

---

## What this costs, honestly

Every option above adds a way into your home network that did not exist before. That is a real
trade and worth making deliberately:

- **A new listening service**, on the router, that now needs its own updates. A VPN server nobody
  patches is worse than no VPN server.
- **The blast radius is the network, not the app.** A stolen WireGuard key does not just expose
  Nestwatch; it exposes everything on the home LAN, including whatever else has a weak password.
- **One device becomes a single point of total compromise**, in the way this document sets out
  above. That is the cost specific to *full parity*, and the one the plan's step 1 exists to buy
  back.
- **You will be physically present less often**, which raises the value of the tamper-resistance
  this all depends on — and returns you to prerequisite 1.

If what you actually want is to install a new build without walking over, that is a narrower
problem with a narrower answer: [REMOTE-UPDATE.md](REMOTE-UPDATE.md), which stays inside the LAN.

---

## Status of this guide

Sources are cited **by symbol rather than by line number**, deliberately. The first version of this
document cited lines; three of the five drifted within two commits while every quoted sentence
stayed word-for-word intact, which sends a reader to the wrong place and makes correct prose look
stale. A symbol moves with its comment.

The claims about this codebase were verified by reading the cited symbol on `0feb242`, and
the five arguments above are quoted from the source rather than paraphrased. The external findings
were checked against primary sources on 2026-09-02 and are cited inline. The Tailscale
subnet-router behaviour was checked against Tailscale's own documentation.

**The plan above has been implemented; the walkthroughs have not been tested.** Those are separate
claims and the difference matters. Steps 1 and 3 are in, step 2 is half in and half declined
(above), and `main` was green at `0feb242`. What remains untested is everything involving an actual
router: **no arrangement in this document has been set up and used against this project's own
installation** — unlike [REMOTE-UPDATE.md](REMOTE-UPDATE.md), which describes a flow the generated
script performs. Treat the reasoning as sound and every walkthrough as unverified.

**Both findings this document was written against are now closed.** `O89` — a paired device holding
the parent's whole authority — was closed by `8d5f5c3` while this page was being written, and the
`SECURITY.md` paragraph it quoted was rewritten with it. `O77` — a leaked session revocable only by
signing every device out — was closed by `be1c07e`/`f6687d8`. Both are **deleted** from
`OPEN-FINDINGS.md` per that file's rule, so a citation pointing at either will find nothing; they
are named here as history, not as open work.

**Prerequisites 2 and 3 remain unverified for any specific household**, and they are still the two
things that can make all of the above unreachable. Nothing implemented here changes that: the code
is ready for a tunnel that nobody has yet confirmed this network can carry.
