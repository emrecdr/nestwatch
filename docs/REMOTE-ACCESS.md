# Reaching the dashboard from outside the house

Nestwatch is LAN-only on purpose, and stays that way: nothing here is a feature of the program,
and none of it is supported. What follows is what to do **if you decide you need it anyway** —
which arrangement is safe, which ones quietly aren't, and why the difference is not a matter of
taste.

The short version: **do not give Nestwatch a way out. Give yourself a way in.** Done that way the
monitored PC still makes no outbound connection, which is the promise the whole design rests on.

---

## Before anything: your child must not be a local administrator

The same prerequisite as [REMOTE-UPDATE.md](REMOTE-UPDATE.md), for the same reason, and it is not
a formality. Run this on the PC:

```powershell
Get-LocalGroupMember -SID S-1-5-32-544 | Select-Object Name
```

If his account is listed, **stop here.** A remote path into your network is worth having only if
the thing at the end of it cannot be switched off by the person it monitors. An administrator can
stop the service, and no tunnel design compensates for that.

---

## The constraint that decides every option

Nestwatch refuses any client whose source address is not on a private network, in the application
itself — before authentication, before the session layer, before anything (`security::is_lan`):

```rust
IpAddr::V4(v4) => v4.is_private() || v4.is_loopback(),
```

RFC1918 (`10/8`, `172.16/12`, `192.168/16`) plus loopback. Everything else gets a `403`.

This is deliberate belt-and-suspenders — the OS firewall is the outer gate, this is the inner one,
and it keeps working if the firewall rule is deleted or the network profile flips to Public. It
also means **an option only works if your traffic arrives wearing a private address.** That single
fact eliminates most of the popular answers, which is the intended direction: the tool fails
closed rather than quietly widening.

---

## Check this first: are you behind CGNAT?

If your ISP hands you a shared public address, nothing from outside can reach your house at all
and the first two options below are impossible. Thirty seconds settles it:

1. Look at the WAN/Internet address in your router's status page.
2. Compare it with what <https://whatismyip.com> reports.

**Different → you are behind CGNAT.** Skip to the subnet-router option. Same → you have a reachable
address, and option 1 is open to you.

---

## What works

### 1. WireGuard on your router — recommended

Your phone joins the home network over an encrypted tunnel and gets an address on it. Nestwatch
sees an ordinary LAN client and behaves exactly as it does from the sofa.

**Why this one.** It is a single UDP port, and WireGuard does not reply to unauthenticated packets
at all — no handshake, no error, nothing. A port scan cannot tell it is there. Compare an exposed
HTTPS or SSH port, which announces itself and invites guessing. There is no third party in the
path, and nothing is installed on the monitored PC.

**It needs no change to Nestwatch, and that is the point.** The certificate's SANs are this
machine's LAN address and hostname (`cert::reachable_hosts`), so over the tunnel you browse to the
*same* `https://192.168.x.y:8443` you always did — certificate matches, origin unchanged, the
`SameSite=Strict` cookie and the `Sec-Fetch-Site` check behave identically. An address the tunnel
itself assigns (typically `10.x`) is also RFC1918, so either topology passes the gate.

Many home routers have this built in. On a FRITZ!Box (FRITZ!OS 7.39+) it is
**Internet → Permit Access → VPN (WireGuard) → Add Connection**, which prints a QR code for the
phone app. ASUS, MikroTik, UniFi, OpenWrt and OPNsense/pfSense all have equivalents.

If your ISP's supplied box has no WireGuard server, run one on a small always-on machine instead
(a Raspberry Pi or NAS; [PiVPN](https://pivpn.io) is a one-script install) and forward one UDP
port to it.

### 2. Your router's own VPN

Same principle, if the box already has an IPsec or WireGuard server and you would rather not add
hardware. Judge it on the same two questions: does the traffic arrive on the home subnet, and does
the monitored PC have to run anything? A router VPN answers well on both.

### 3. Tailscale as a *subnet router* — the CGNAT answer

Run Tailscale on a **separate always-on machine** and advertise the home subnet from it. Tailscale
masquerades routed traffic to that machine's own LAN address by default, so requests reach
Nestwatch from `192.168.x.x` and pass the gate. It also traverses CGNAT, which is why it is here.

**Tailscale installed on the monitored PC does not work, and should not be made to.** It assigns
from the carrier-grade-NAT range `100.64.0.0/10`, which is not RFC1918, so the app-layer gate
returns `403`. Widening the gate to admit that range was considered and declined — see
[OPEN-FINDINGS.md](OPEN-FINDINGS.md) — because it would extend the trust boundary past the home
network for every install, to support one unsupported setup. The subnet-router arrangement is the
better one anyway: the tunnel daemon stays off the machine that is supposed to make no outbound
connection of its own.

**The cost, stated plainly.** SNAT collapses every remote visitor into one source address, so
`audit.jsonl` — which records *where* a login came from — shows the router for all of them, and
the per-IP login throttle shares one bucket across everyone arriving that way. Routed WireGuard
keeps you distinguishable. That is a real reason to prefer option 1 where it is available, beyond
the third-party question.

---

## What does not work, and why

| | |
|---|---|
| **Port-forwarding the dashboard** | Does not work *and* is the worst idea here. The request arrives with a public source address and `is_lan` returns `403` — so you would have to weaken the allowlist to enable your own most dangerous option. That failure direction is deliberate. Put a child's screenshots on the public internet behind one password and the only thing between them and the world is that password. |
| **Cloudflare Tunnel, ngrok, and similar** | The tunnel daemon runs on your side and dials out, and TLS terminates at a third party. Screenshots of your child pass through someone else's edge in the clear. This breaks the promise in the way that actually matters, not on a technicality. |
| **TeamViewer, AnyDesk, Chrome Remote Desktop** | All require an outbound broker connection **from the child's PC** — precisely the thing this project refuses to do — and they hand over full interactive control of the desktop rather than the scoped dashboard. |
| **Tailscale on the monitored PC** | `403` from the app-layer gate, and it makes that machine dial out. See above. |

---

## If you set up option 1 or 2

- **Dynamic DNS.** Your home address changes, so the phone needs a stable name to dial. FRITZ!Box
  includes MyFRITZ!; otherwise DuckDNS or No-IP.
- **Scope the peer with a firewall rule.** Note the distinction: `AllowedIPs` on the phone is
  split-tunnel convenience, and `AllowedIPs` on the server restricts *source*, not destination.
  Only a firewall rule limits what the peer may reach — ideally just the Nestwatch host and port.
- **One peer per device**, so losing one means revoking one.
- **Treat the phone as the key.** WireGuard has no passwords: possession of the private key *is*
  the authorisation. Losing the phone unlocked is losing a permanent route into your LAN and a
  window onto your child's screen. Revocation is deleting that peer from the router — find out how
  before you need it, not after.
- **The dashboard password still applies.** The tunnel gets you onto the network; it does not sign
  you in. Do not weaken the password because the network feels private now.

---

## What this costs, honestly

Every option above adds a way into your home network that did not exist before, to save you a walk
to the PC. That is a real trade and worth making deliberately:

- **A new listening service**, on the router or a Pi, that now needs its own updates. A VPN server
  nobody patches is worse than no VPN server.
- **The blast radius is the network, not the app.** A stolen WireGuard key does not just expose
  Nestwatch; it exposes everything on the home LAN, including whatever else has a weak password.
- **You will be physically present less often**, which raises the value of the tamper-resistance
  this all depends on — and returns you to the prerequisite at the top of this page.

If what you actually want is to install a new build without walking over, that is a narrower
problem with a narrower answer: [REMOTE-UPDATE.md](REMOTE-UPDATE.md), which stays inside the LAN.

---

## Status of this guide

Researched and written against the code as it stands; the `is_lan` behaviour, the certificate SANs
and the CGNAT exclusion were each checked in `src/`, and the Tailscale subnet-router behaviour
against Tailscale's own documentation. **None of it has been set up and used against this
project's own installation** — unlike [REMOTE-UPDATE.md](REMOTE-UPDATE.md), which describes a flow
the generated script performs. Treat the reasoning as sound and the walkthrough as untested.
