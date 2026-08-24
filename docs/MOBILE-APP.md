# A mobile app for the dashboard — findings and recommendation

The dashboard is a web page. Reaching it means opening a browser, typing an address, and clicking
through a certificate warning. This records what was found when asking whether a **Flutter app for
Android and iOS** would be better, what it would cost, and the traps that are only visible before
you start.

**Nothing here has been built or run.** It is documentation research, checked against primary
sources — Apple's developer documentation, Google Play policy, Flutter and Dart source and issue
trackers, and the published source of comparable self-hosted clients. Every conclusion that rests
on inference rather than a document says so.

**Verdict: feasible, and the right shape is a native Flutter client** — not a wrapped web view and
not an installable web app. But the honest cost is a second user interface maintained forever, and
the honest limit is that it cannot notify you when you are away from home.

---

## Why not simply install the existing dashboard as a web app

This was the first recommendation, and it was **wrong**. It is recorded because the reasoning is
the same trap anyone would fall into.

An iOS home-screen web app does **not** share storage or state with Safari — session, cookies,
local storage and service workers all live in a separate context, and it is [not the same
application][pwa-context]. The certificate exception a parent grants in Safari therefore does not
travel into the installed app. With a self-signed certificate, the installed web app is left
looking at a server it does not trust and cannot be taught to.

That is specific to the certificate. Everything *else* about the web-app route is fine, which is
what makes it tempting.

A wrapped web view is worse than both: `webview_flutter` exposes no hook for overriding certificate
trust at all, and a thin wrapper around a website is the classic shape App Review rejects under
minimum-functionality. It manages to be both the most likely to be rejected and the least able to
do the job.

[pwa-context]: https://www.netguru.com/blog/how-to-share-session-cookie-or-state-between-pwa-in-standalone-mode-and-safari-on-ios

---

## Why Flutter specifically works here

**Because Dart does its own TLS.** `dart:io`'s HTTP client is built on a bundled BoringSSL and does
not go through Apple's URL Loading System. Apple's own wording is that *"the system enforces ATS
when you use the standard URL Loading System"* and that [ATS *"doesn't apply to calls your app makes
to lower-level networking interfaces"*][ats]. That request to move `dart:io` onto native secure
sockets has been [open since 2016][flutter2696] and has not landed, which is what confirms it is
still true.

So App Transport Security — the thing that makes self-signed certificates painful on iOS — never
engages, and the app can pin the certificate itself.

This also explains the strongest precedent rather than being contradicted by it. **Home Assistant's
iOS app is native Swift**, goes through `URLSession`, and therefore *is* subject to ATS — which is
why it cannot accept a self-signed certificate in-app and tells users to install the certificate at
the OS level instead. A Flutter app does not inherit that constraint.

⚠️ **Calibration:** no Apple or Flutter document states "ATS does not apply to Flutter's `dart:io`"
in those words. It is a sound inference from two verified facts, not a quoted one. **Check it on a
real device before committing** — the Simulator is not a valid test (see below).

[ats]: https://developer.apple.com/documentation/security/preventing-insecure-network-connections
[flutter2696]: https://github.com/flutter/flutter/issues/2696

### Three corrections to "Flutter bypasses ATS"

The conclusion holds; the slogan is wrong in ways that bite later.

1. **Flutter still reads `Info.plist`.** It parses the ATS configuration at build time and enforces
   a [network policy inside Dart][netpolicy], throwing `Insecure HTTP is not allowed by platform`.
   It covers **cleartext HTTP only**, so it does not affect an HTTPS-only server — but "Info.plist
   is irrelevant" is false and will mislead whoever implements this.
2. **The property belongs to `dart:io`, not to "Flutter".** Anything routed through `URLSession` is
   fully ATS-governed — a web view, a native plugin, or the `cupertino_http` package. On iOS 17+
   ATS additionally [refuses connections to bare IP addresses by default][localnet] and forbids
   loosening trust evaluation. **So: no `cupertino_http`, and no web view.**
3. **Local-network permission is a separate gate and is not bypassed.** See below.

[netpolicy]: https://docs.flutter.dev/release/breaking-changes/network-policy-ios-android
[localnet]: https://developer.apple.com/documentation/bundleresources/information-property-list/nsapptransportsecurity/nsallowslocalnetworking

---

## The bug to avoid before writing a line of client code

**The pinning example in `dio`'s own documentation leaks the password.**

It pairs `badCertificateCallback = (cert, host, port) => true` with a separate `validateCertificate`
check. Reading `io_adapter.dart`: the request body is sent at lines 144 and 161, and
`validateCertificate` is not consulted until line 192. The handshake completes against **any**
certificate, so the parent's password and session cookie reach an attacker before the pin is ever
examined.

The comparison must happen **inside** `badCertificateCallback`, which runs during the handshake:

```dart
HttpClient pinnedClient(List<int> pinnedDerSha256) {
  // withTrustedRoots:false is load-bearing. badCertificateCallback fires only when a
  // certificate FAILS to authenticate — so with the default trust store in place, anyone
  // holding a publicly-trusted certificate for that address gets a connection silently.
  final client = HttpClient(context: SecurityContext(withTrustedRoots: false));
  client.badCertificateCallback =
      (cert, host, port) => _constantTimeEquals(sha256.convert(cert.der).bytes, pinnedDerSha256);
  return client;
}
```

The fingerprint format already lines up: `cert.rs` hashes the DER with SHA-256, and Dart's
`X509Certificate.der` is the same bytes. No conversion.

**Audit every dependency that opens a socket.** This is what defeated [Immich][immich] — Flutter,
self-hosted, on both stores — whose maintainer concluded *"there's no good way to do this in
flutter."* Their actual blocker was a *dependency* opening its own socket, not the language.
`Image.network` does not accept a custom client, and the screenshot view needs one.

And note the workaround every other self-hosted client relies on is **unavailable here**: telling a
user to install the certificate in iOS Settings does nothing, because Dart ignores the platform
trust store.

[immich]: https://github.com/immich-app/immich/discussions/2375

---

## Store review: lower risk than expected, but not where expected

Precedent settles the question that sounds hardest. **[Home Assistant][ha-store]** ships
user-accepted trust exceptions for self-signed certificates *and* `NSAllowsArbitraryLoads` — the
most justification-triggering key Apple has — and is live on the App Store. **Nextcloud iOS** does
textbook trust-on-first-use: it stores the leaf certificate and byte-compares thereafter, behind a
Yes / No / *Certificate details* prompt. Nestwatch would need less than either.

The risks that actually deserve attention are different ones:

**Apple 4.2.7 (remote desktop clients).** Screenshotting the child's PC reads to a reviewer as a
remote-desktop mirror. But the guideline *requires* a user-owned host computer, host and client on
the same LAN, software executing on the host, and account setup initiated from the host — which is
this architecture almost verbatim, down to `nestwatch install` setting the password on the PC.
Cite 4.2.7 in the review notes and the oddest thing about the app becomes the thing the rule asks
for. Keep screenshots read-only; if they ever become click-to-control, clause (b) starts to bind.

**Google Play's stalkerware policy.** Monitoring apps are permitted only when *"exclusively designed
and marketed for parents to monitor their children"*, and must declare
`<meta-data android:name="isMonitoringTool" android:value="child_monitoring" />` in **every** version
code — apps are rejected specifically for omitting it. Never market it for spouses or employees;
that is banned even with consent. Keep "spy", "stealth", "hidden" out of the listing.
⚠️ It is **unverified** whether the policy binds a viewer-only app whose monitored device is a
Windows PC — the text says "a device". Comply regardless; it is one line.

Two smaller notes: an ATS justification is **not** required for `NSAllowsLocalNetworking` (it does
not appear on Apple's justification list at all), and MDM is guideline 5.5, not 5.4 — it does not
apply, and `FamilyControls` should not be requested since it governs restricting *iOS* devices.

For "the app does nothing without a server": Apple accepts a demo video or supplied hardware, and
the simplest answer is to run an instance on a public host with a real certificate and put the URL
and password in the review notes. No documented rejection of a self-hosted client for this was found.

[ha-store]: https://apps.apple.com/us/app/home-assistant/id1099568401

---

## What the app cannot do

**It cannot notify you when you are away from home.** A LAN-only server cannot reach Apple's or
Google's push services, both of which require the *server* to make an outbound TLS connection —
and the whole design forbids the monitored PC making outbound connections. Away from home the app
gets the same `403` a browser does.

iOS additionally cannot hold a background socket, and a local-network operation attempted in the
background while permission is still undetermined is **denied silently, without even recording the
denial**. Android can approach instant delivery with a foreground service, but Android 15 caps a
`dataSync` service at 6 hours in 24 with a fatal exception on overrun.

**So the honest promise on iOS is "pending requests appear when you open the app", not "you will be
notified."** The existing offline time codes already cover the away-from-home case better than a
notification would, and the [remote-access](REMOTE-ACCESS.md) route — a VPN that puts your phone on
the home subnet — restores full function without the PC reaching out.

---

## Discovery: use the QR, not mDNS

`pairing.rs` already encodes `https://{host}:{port}/p/{token}` into a scannable QR: address, port,
**and** a token that lands the parent already signed in. mDNS would buy a worse version of that
while costing an iOS local-network prompt that the QR route avoids entirely, plus a Bonjour
entitlement, plus a responder on the server that does not exist — and it fails in ordinary homes,
since multicast does not survive many mesh systems, guest networks, or AP isolation.

**Put the certificate fingerprint in the QR — but only when the app is actually being built.**

It converts first contact from trust-on-*first-use* into *verified* first use: the app rejects an
impostor outright instead of asking a human to compare 64 hexadecimal characters, which nobody
does. As a URL fragment it is invisible to the server and to the existing browser flow.

It was **deliberately not done yet**, and the reasoning is worth keeping because it looks like a
free win. Today the only thing that scans that QR is a phone camera, which opens a **browser** —
and a browser cannot read a certificate fingerprint from JavaScript, so the value would sit there
inert. Worse, a page that displayed the expected fingerprint would prove nothing: an impostor
serves its own page showing its own. The check is only meaningful in a client that pins. Deferring
costs nothing, because the pairing URL is regenerated at every `install` and `pair`, so a later
build simply emits the newer one.

**A cheaper fix for a real problem:** the certificate bakes in IP addresses at install time, so a
DHCP lease change breaks the browser today. Because pinning is name-independent, an app can sweep
the local range for whichever host presents the pinned certificate — unspoofable by construction,
and no multicast.

---

## If it gets built

In order, smallest commitment first:

1. **Fingerprint into the pairing QR** (server-side, small, testable here).
2. **Audit every dependency that opens a socket** for whether a custom `HttpClient` can be injected.
   This is the step that decides whether the project is possible, and it is where Immich failed.
3. **The thinnest client that is worth having**: scan QR → verify pin → log in → persist the session
   cookie in secure storage. Then three screens — pending time requests with approve/deny, today's
   usage, and the screenshot. Leave rules, routines and the audit log in the browser; they are
   configuration, done rarely, and each one added is a second interface to keep in step.
4. **Android foreground-service notifications**, which is the only platform where they can work.
5. **Store paperwork from day one** — `isMonitoringTool`, the 4.2.7 note, a privacy policy naming
   screenshots of the child's desktop as that child's personal data.

The cost to keep in view: the dashboard is roughly 1,900 lines against 24 API routes. A native
client re-implements as much of that as it exposes, forever, and every future feature ships twice.
That is the real argument for keeping the app small and leaving configuration in the browser.

---

## Unverified

- That ATS does not apply to `dart:io` — inferred from two verified facts, not documented as such.
  **Test on real iOS hardware, not the Simulator**, which does not implement local-network privacy
  at all.
- Whether Play's stalkerware declaration binds an app whose monitored device is a Windows PC.
- How Home Assistant, Nextcloud and Plex actually satisfy the "reviewer needs a server" requirement
  — none has published it.
- The exact minimum-functionality rejection wording for wrapper apps; developer reports paraphrase
  it consistently but no primary text was found.
- Every code sample here is written from API contracts and has not been compiled or run.
