//! HTTP-layer defense-in-depth: a network-scope gate and static security headers.
//!
//! These sit in front of every route so they apply uniformly — including the static UI, the
//! `/api/*` handlers, and error responses. Neither depends on the session, so they run before
//! any authentication work.

use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, Request};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;

/// Reject any client that is not on the local network **at the application layer**, so the
/// controls aren't reachable from off-LAN even if the OS firewall rule is missing, disabled,
/// or the network profile flips to Public. This is deliberate belt-and-suspenders: the
/// firewall (`install::configure_firewall`) is the outer gate; this is the inner one.
///
/// Requires the server to be built with `into_make_service_with_connect_info::<SocketAddr>()`
/// so the peer address is available. Since this is direct LAN TLS with no reverse proxy, the
/// TCP peer address is the true source — we never consult `X-Forwarded-For` (spoofable).
pub async fn require_lan_peer(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if is_lan(peer.ip()) {
        Ok(next.run(request).await)
    } else {
        tracing::warn!(peer = %peer.ip(), "rejected non-LAN client");
        Err(StatusCode::FORBIDDEN)
    }
}

/// Is `ip` on a private LAN (or loopback)? Loopback is allowed so the dev `run` mode and any
/// local health probe keep working. Public/routable addresses are rejected.
///
/// Note what this excludes: the **CGNAT range `100.64.0.0/10`** is not `is_private()`, and that
/// is the range Tailscale assigns. A parent tunnelling in over Tailscale is rejected here even
/// though the tunnel itself works. That's the intended fail-closed direction for a LAN-only
/// tool — widening it would extend the trust boundary past the home network for everyone — but
/// it is documented in the README so it doesn't read as a bug.
fn is_lan(ip: IpAddr) -> bool {
    match ip {
        // RFC1918 (10/8, 172.16/12, 192.168/16) covers home LANs; loopback for dev/local.
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback(),
        // The server binds 0.0.0.0 (v4 only), so a v6 peer shouldn't occur; allow loopback only.
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// Reject requests a browser tells us came from somewhere other than this exact origin.
///
/// **The gap this closes.** `SameSite=Strict` on the session cookie is scoped to the *site*,
/// and a site is scheme + registrable domain — **not the port**. So a page served over HTTPS
/// from another port on this same machine counts as same-site, and the browser attaches the
/// parent's session cookie to requests it makes here. **10 `/api` endpoints take no JSON body**
/// — `/lock`, `/re-anchor`, `/shutdown`, `.../kill`, `.../approve`, `.../deny`, `.../apply`,
/// `/routines/{name}/delete`, `/providers/{name}/delete` and `/sessions/{handle}/revoke` — so
/// they never trigger the `Content-Type: application/json` preflight that protects the rest —
/// a plain form POST reaches them with the cookie attached. The child owns an account on this
/// PC and can serve such a page from it.
///
/// `Sec-Fetch-Site` distinguishes `same-origin` from `same-site`, which is exactly the
/// distinction the cookie attribute can't make. Browsers forbid page scripts from setting any
/// `Sec-` header, so the value can't be forged from JavaScript.
///
/// # Two signals, because one of them is younger than the devices in the house
///
/// `Sec-Fetch-Site` is the primary check and decides every request that carries it. When it is
/// **absent** the request is judged on `Origin` instead, and the reason is a date: fetch metadata
/// shipped in Chrome 76 (2019) and Firefox 90 (2021) but in **Safari only at 16.4**, in March
/// 2023. An iPad Air 2, iPad 5 or iPad mini 4 cannot reach that version and never will, and that
/// is precisely the device a household promotes to "the thing we check the dashboard on". Such a
/// browser sends the session cookie and no fetch metadata, so for it the port-confusion gap above
/// would be wide open on a header this middleware never looked at.
///
/// A previous version of this comment justified admitting the header-less case as "a non-browser
/// client — `curl`, a probe — or a browser too old to send it; those carry no ambient cookie
/// authority to abuse". **The second half of that was false, and it was the half doing the work.**
/// An old browser has a full cookie jar; that is what makes it dangerous. The sentence was true of
/// `curl` and got extended to a case where it is exactly backwards. OWASP's CSRF guidance is
/// blunt about the shape: a fallback to origin verification is a *mandatory requirement* for any
/// fetch-metadata implementation, because some browsers do not send the `Sec-` headers.
///
/// `Origin` is the right fallback here rather than a token or a custom header, and the choice was
/// forced rather than preferred. WebKit has sent `Origin` on POST since **2008** (bug 20792), so
/// it covers the entire population fetch metadata misses. The custom-header defence OWASP also
/// lists would work for browsers and **would break the shipped Android client**, which reaches
/// this server through Dart's `HttpClient` setting only `Accept` and a cookie — see
/// `tests/origin.rs`.
///
/// # Policy
///
/// * `same-origin` / `none` — allow. (`none` is a user-initiated load: a typed URL, a bookmark,
///   the pairing QR.)
/// * `same-site` / `cross-site` — allow only a top-level navigation `GET`, so following a link
///   to the dashboard from a chat message still works.
/// * **absent** — fall back to [`origin_names_target`]. No `Origin` either means a non-browser
///   caller (`curl`, a probe, the Android client), which is admitted exactly as before; an
///   `Origin` that names somewhere else is refused unless it is a `GET`/`HEAD`, which mirrors the
///   navigation exemption above as closely as the missing `Sec-Fetch-Mode` allows.
///
/// Note what the last bullet deliberately is **not**: it does not fail closed when both headers
/// are missing. OWASP recommends blocking there, and this declines that recommendation on a
/// property of this product rather than of the web — the header-less caller here is a shipped
/// phone app, and every browser engine has sent `Origin` on unsafe methods for over fifteen
/// years, so blocking would cost a real client to close a window no current browser stands in.
pub async fn require_same_origin(request: Request, next: Next) -> Result<Response, StatusCode> {
    let headers = request.headers();
    let site = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok());
    let mode = headers.get("sec-fetch-mode").and_then(|v| v.to_str().ok());
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    let target = target_authority(&request);
    let allowed = is_same_origin(site, mode, request.method(), origin, target);

    if allowed {
        Ok(next.run(request).await)
    } else {
        tracing::warn!(
            method = %request.method(),
            uri = %request.uri(),
            "rejected a cross-origin request (Sec-Fetch-Site)"
        );
        Err(StatusCode::FORBIDDEN)
    }
}

/// Where this request actually arrived, as a `host[:port]` authority — the value an `Origin` is
/// compared against.
///
/// **Both sources are checked because both occur, and reading only one would be a guard that
/// tests green and protects nothing.** HTTP/1.1 puts the authority in the `Host` header. HTTP/2
/// replaces it with the `:authority` pseudo-header, which hyper lifts onto the request URI and
/// which **never appears in the header map at all** — so on an h2 connection a `Host`-only lookup
/// returns `None`, the comparison below is skipped, and every request is admitted. `h2` is in this
/// build (`axum` enables it by default and it is in `Cargo.lock`), and Safari has spoken HTTP/2
/// since Safari 9 — which is to say, on exactly the old iPads the fallback exists for.
fn target_authority(request: &Request) -> Option<&str> {
    request
        .uri()
        .authority()
        .map(axum::http::uri::Authority::as_str)
        .or_else(|| {
            request
                .headers()
                .get(header::HOST)
                .and_then(|v| v.to_str().ok())
        })
}

/// Does `origin` — a serialized origin, `https://host[:port]` — name `target`, an authority of
/// `host[:port]`?
///
/// The scheme is compared by requiring it, not by parsing it: this service is HTTPS-only, so
/// `http://…` and the opaque `null` (which a sandboxed frame or a redirected POST sends) are
/// mismatches by construction. Hosts are ASCII-case-insensitive; ports are not optional on either
/// side, because a browser omits a default port from *both* `Origin` and `Host`, so the two agree
/// or disagree together.
fn origin_names_target(origin: &str, target: &str) -> bool {
    origin
        .strip_prefix("https://")
        .is_some_and(|host| host.eq_ignore_ascii_case(target))
}

/// The policy behind [`require_same_origin`], as a pure function of the two fetch-metadata
/// headers, the method, and the origin/target pair — so it is testable without standing up a
/// router, the same way [`is_lan`] is.
fn is_same_origin(
    site: Option<&str>,
    mode: Option<&str>,
    method: &Method,
    origin: Option<&str>,
    target: Option<&str>,
) -> bool {
    match site {
        Some("same-origin") | Some("none") => true,
        // Cross-site or same-site: only a top-level navigation that can't carry a payload.
        // A form submission is *also* a navigation, which is why the method is checked too.
        Some(_) => mode == Some("navigate") && matches!(*method, Method::GET | Method::HEAD),
        None => match (origin, target) {
            // Nothing claims a source. `curl`, a health probe, and the Android client all land
            // here, and all three are admitted exactly as they were before this fallback existed.
            (None, _) => true,
            // We cannot say what our own authority is, so we cannot call anything a mismatch.
            // Reached by the test suite's `oneshot` requests, which have neither.
            (Some(_), None) => true,
            (Some(origin), Some(target)) => {
                origin_names_target(origin, target) || matches!(*method, Method::GET | Method::HEAD)
            }
        },
    }
}

/// Content-Security-Policy for the embedded single-page UI.
///
/// `default-src 'none'` then allow only what the page uses.
///
/// **`script-src` no longer admits `'unsafe-inline'`.** It used to, because both served pages
/// carried their JavaScript in an inline `<script>`. Those now live in `assets/app.js` and
/// `assets/ask.js`, so `'self'` covers them and the page can no longer execute a script that
/// arrives in the markup — which is the directive that matters most here, since the markup is
/// where injected content would land.
///
/// **`'unsafe-eval'` is gone too.** It was there because Alpine's standard build compiles every
/// attribute expression with `new Function`. The page now ships Alpine's **CSP build**, which
/// parses those expressions with its own small parser instead and reaches no globals at all — so
/// `script-src` is `'self'` and nothing on either page can evaluate a string as code.
///
/// The cost was 26 directives, moved into getters and methods on the component: 11 template
/// literals, 1 spread, and 14 uses of `?.`/`??`. Those four constructs are the only ones the CSP
/// parser rejects — property paths, ternaries, comparisons, method calls with arguments,
/// assignment, `x-model` and array literals all still work in an attribute.
///
/// Two of the four are undocumented, and were established by probing the build rather than reading
/// about it. `web::tests::no_alpine_expression_needs_more_than_the_csp_build_can_parse` holds the
/// line, and matters more than most guards here because a spread fails *silently* — no console
/// error, the loop simply renders nothing, which is how a chart shipped with no bars once before.
///
/// `style-src` keeps `'unsafe-inline'`: the `[x-cloak]` rule is still an inline `<style>`, and
/// Alpine writes `style` attributes for `x-show` and `:style`. `img-src` allows `blob:`
/// (screenshot object URLs) and `data:` (DaisyUI's inline-SVG backgrounds, and the favicon).
///
/// `connect-src` also allows `api.github.com`, for the version check behind the button in the
/// footer. That request is made by the *parent's* browser, on the parent's own device — the
/// monitored PC still never contacts anything, which is what "nothing leaves the house" claims.
/// It happens only on a click; nothing is fetched on load.
///
/// The cost is real but small: script running in this page could reach github.com. Script running
/// in this page already holds the parent's session and can drive every control here — take a
/// screenshot, kill a process, shut the machine down — so exfiltration to one more host is not
/// the marginal risk. `default-src 'none'` still blocks every other destination.
const CSP: &str = "default-src 'none'; \
     script-src 'self'; \
     style-src 'self' 'unsafe-inline'; \
     img-src 'self' blob: data:; \
     connect-src 'self' https://api.github.com; \
     base-uri 'none'; \
     form-action 'self'; \
     frame-ancestors 'none'; \
     object-src 'none'";

/// Deny every powerful browser feature — the dashboard uses none of them.
const PERMISSIONS_POLICY: &str = "accelerometer=(), autoplay=(), camera=(), display-capture=(), encrypted-media=(), \
     fullscreen=(), geolocation=(), gyroscope=(), magnetometer=(), microphone=(), midi=(), \
     payment=(), usb=()";

/// Stamp static security headers on every response (applied outermost, so even rejections and
/// 404s carry them). Deliberately no `Strict-Transport-Security`: with a self-signed cert the
/// browser ignores it, and if it ever stuck it would make cert rotation an unrecoverable
/// lockout — revisit only behind a genuinely trusted cert.
pub async fn set_security_headers(mut response: Response) -> Response {
    let h = response.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static(PERMISSIONS_POLICY),
    );
    // The two cross-origin isolation headers, and they are here for one specific reason rather
    // than for completeness: **they keep working when `Sec-Fetch-Site` is missing.** That is the
    // same population `require_same_origin`'s `Origin` fallback exists for — a browser predating
    // March 2023 — and these are enforced by the browser rather than asked of it, so they hold
    // even on a request the middleware decided to admit.
    //
    // `same-origin` is safe for both. The dashboard opens no cross-origin popups that need a
    // window handle back (COOP severs exactly that, which is `rel="noopener"` by another route),
    // and nothing here is meant to be embedded by another origin (CORP), which is the port-confusion
    // case again from the resource side.
    //
    // `Cross-Origin-Embedder-Policy` is deliberately **absent**. It would buy nothing here — it
    // gates cross-origin isolation for `SharedArrayBuffer`, which this page has no use for — and
    // it would constrain what the page may load later for no present benefit.
    h.insert(
        HeaderName::from_static("cross-origin-opener-policy"),
        HeaderValue::from_static("same-origin"),
    );
    h.insert(
        HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-origin"),
    );
    // `no-store` is the DEFAULT, not an override. The most sensitive bytes this service produces
    // are captures of a child's desktop, so anything that has not thought about caching must not be
    // stored — which is every `/api/*` response, and remains the right answer for them.
    //
    // It used to be unconditional, on the grounds that "every page is embedded in the binary and
    // served over a LAN, so there is no round trip worth saving". The round trip was not what it
    // cost: `no-store` forbids *storing*, so a parent's phone re-fetched all 324 KB of the UI on
    // every visit. `web::serve_asset` now sets its own `no-cache` + `ETag` for the embedded assets,
    // which still forbids serving anything stale, and this must not overwrite it.
    //
    // Written as "fill in if absent" rather than a path prefix on purpose: a prefix would be a
    // second place to keep in step with the router, and would silently mis-classify any future
    // route that does not match it. Opting out here requires a handler to say so explicitly.
    if !h.contains_key(header::CACHE_CONTROL) {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The policy allows exactly one host to be contacted, and no more.
    ///
    /// `connect-src` is what stops this page sending data anywhere, and it is the one directive
    /// that had to be widened -- for the footer's version check, which runs in the parent's
    /// browser rather than on the monitored PC. Pinned so a future edit cannot quietly add a
    /// second destination, and so `default-src 'none'` cannot be loosened into covering it.
    #[test]
    fn the_page_may_contact_itself_and_github_and_nothing_else() {
        let connect = CSP
            .split(';')
            .map(str::trim)
            .find(|d| d.starts_with("connect-src"))
            .expect("the policy must state connect-src explicitly");

        assert_eq!(
            connect, "connect-src 'self' https://api.github.com",
            "exactly one external host, and only over https"
        );
        assert!(
            CSP.trim_start().starts_with("default-src 'none'"),
            "everything not named must still be denied by default"
        );
        // The check is a click, not a page load, so no other host needs reaching -- and these
        // two in particular would mean tracking or a CDN.
        for forbidden in ["*", "http:", "data: https:", "googleapis", "cdn"] {
            assert!(
                !connect.contains(forbidden),
                "connect-src must not admit {forbidden:?}: {connect}"
            );
        }
    }

    /// Every external URL in the page is either admitted by `connect-src` or listed as a link.
    ///
    /// The test above pins the policy against a literal, which catches the policy being widened.
    /// It cannot catch the other direction: change the URL in `checkForUpdate()` and the policy
    /// stays as written, the assertion stays green, and the button fails at runtime — swallowed
    /// by its own catch block as "Could not reach GitHub from this device", which reads like a
    /// network problem rather than a policy one.
    ///
    /// Rather than trying to infer which URLs are fetched and which are navigated to — the page
    /// has both, and one of the links is assembled in JavaScript — every absolute URL must be
    /// accounted for one way or the other. A new one fails this test until somebody says which
    /// it is, which is the moment to think about it.
    ///
    /// Both files, because the page is served as two: the markup holds the navigation links and
    /// `app.js` holds the fetch. Scanning only the markup would have left the one URL this test
    /// exists for — the `api.github.com` call — unexamined, while the assertion stayed green.
    #[test]
    fn every_external_url_in_the_page_is_accounted_for() {
        const SOURCES: [(&str, &str); 3] = [
            ("index.html", include_str!("../assets/index.html")),
            ("app.js", include_str!("../assets/app.js")),
            ("ask.js", include_str!("../assets/ask.js")),
        ];

        /// Navigation targets: the parent's browser follows these, and `connect-src` does not
        /// govern navigation. Exact URLs, not hosts, so this cannot quietly widen.
        const LINK_ONLY: &[&str] = &["https://github.com/emrecdr/nestwatch/releases/latest"];

        let connect = CSP
            .split(';')
            .map(str::trim)
            .find(|d| d.starts_with("connect-src"))
            .expect("the policy must state connect-src explicitly");

        let mut found = 0usize;
        for (name, text) in SOURCES {
            let mut rest = text;
            while let Some(i) = rest.find("https://") {
                rest = &rest[i..];
                // `\r` included: `include_str!` embeds the file as checked out, and a Windows
                // checkout has CRLF, which would otherwise land inside the captured URL.
                let end = rest
                    .find(['"', '\'', '`', '<', ' ', '\n', '\r'])
                    .unwrap_or(rest.len());
                let url = &rest[..end];
                rest = &rest[end..];
                found += 1;

                if LINK_ONLY.contains(&url) {
                    continue;
                }
                let host = url
                    .trim_start_matches("https://")
                    .split('/')
                    .next()
                    .unwrap_or_default();
                assert!(
                    connect.contains(host),
                    "{name} references {url}, which connect-src does not admit and which is not \
                     listed as a navigation target:\n  connect-src: {connect}"
                );
            }
        }
        assert!(
            found >= 2,
            "found only {found} external URLs across the served files — the scan has drifted and \
             is checking nothing"
        );
    }

    #[test]
    fn lan_and_loopback_allowed_public_rejected() {
        assert!(is_lan("192.168.1.20".parse().unwrap()), "home LAN");
        assert!(is_lan("10.0.0.5".parse().unwrap()), "private 10/8");
        assert!(is_lan("172.16.4.4".parse().unwrap()), "private 172.16/12");
        assert!(is_lan("127.0.0.1".parse().unwrap()), "loopback (dev/local)");
        assert!(is_lan("::1".parse().unwrap()), "v6 loopback");

        assert!(!is_lan("8.8.8.8".parse().unwrap()), "public v4");
        assert!(
            !is_lan("172.32.0.1".parse().unwrap()),
            "just outside 172.16/12"
        );
        assert!(
            !is_lan("2606:4700:4700::1111".parse().unwrap()),
            "public v6"
        );
    }

    /// Shorthand for the fetch-metadata half: no `Origin`, no target, which is what a browser
    /// that sends `Sec-Fetch-Site` looks like to the fallback (it is never consulted).
    fn by_metadata(site: Option<&str>, mode: Option<&str>, method: &Method) -> bool {
        is_same_origin(site, mode, method, None, None)
    }

    /// Shorthand for the fallback half: no fetch metadata, judged on `Origin` against the
    /// authority the request arrived on.
    fn by_origin(origin: Option<&str>, target: Option<&str>, method: &Method) -> bool {
        is_same_origin(None, None, method, origin, target)
    }

    #[test]
    fn only_this_exact_origin_may_make_a_request_with_a_payload() {
        let post = Method::POST;

        // The gap `SameSite=Strict` leaves open: another port on this same host is *same-site*.
        assert!(!by_metadata(Some("same-site"), Some("cors"), &post));
        assert!(!by_metadata(Some("cross-site"), Some("cors"), &post));

        // A form submission is a navigation too, so "allow navigations" alone would reopen it.
        assert!(!by_metadata(Some("same-site"), Some("navigate"), &post));
        assert!(!by_metadata(Some("cross-site"), Some("navigate"), &post));

        // The dashboard's own calls, and a user-initiated load (typed URL, bookmark, QR).
        assert!(by_metadata(Some("same-origin"), Some("cors"), &post));
        assert!(by_metadata(Some("none"), Some("navigate"), &Method::GET));

        // Following a link to the dashboard from elsewhere must still work.
        assert!(by_metadata(
            Some("cross-site"),
            Some("navigate"),
            &Method::GET
        ));

        // Non-browser clients (curl, probes, the Android client) send no fetch metadata and no
        // `Origin`, and carry no ambient cookie authority for a third party to abuse.
        assert!(by_metadata(None, None, &post));
    }

    /// The population `Sec-Fetch-Site` cannot reach: a browser predating March 2023.
    ///
    /// Every case here arrives with **no** fetch metadata, which before this fallback existed was
    /// an unconditional allow. The attack and the two legitimate callers are separated by `Origin`
    /// alone, so each assertion below is load-bearing on its own.
    #[test]
    fn without_fetch_metadata_a_mismatched_origin_cannot_carry_a_payload() {
        let post = Method::POST;
        let here = Some("192.168.1.5:8443");

        // The attack, on an iPad that will never see Safari 16.4: a page the child serves from
        // another port of the same PC, submitting a form with the parent's cookie attached.
        assert!(
            !by_origin(Some("https://192.168.1.5:9000"), here, &post),
            "a page on another port drove a POST"
        );
        // Same host, plain HTTP — a second server the child runs. Refused on the scheme, which is
        // why the check requires `https://` rather than parsing round it.
        assert!(!by_origin(Some("http://192.168.1.5:8443"), here, &post));
        // A sandboxed frame, or a POST that followed a redirect.
        assert!(!by_origin(Some("null"), here, &post));
        // Somewhere else entirely.
        assert!(!by_origin(Some("https://evil.example"), here, &post));

        // The dashboard's own form/fetch POST from that same old browser must still work.
        assert!(
            by_origin(Some("https://192.168.1.5:8443"), here, &post),
            "the dashboard's own POST was blocked on a browser without fetch metadata"
        );
        // Hosts are case-insensitive; a browser may not normalise what the parent typed.
        assert!(by_origin(
            Some("https://PC-NAME:8443"),
            Some("pc-name:8443"),
            &post
        ));

        // The two shapes that must stay exactly as they were.
        assert!(
            by_origin(None, here, &post),
            "the Android client sends no Origin and must not be blocked"
        );
        assert!(
            by_origin(Some("https://192.168.1.5:9000"), None, &post),
            "with no target to compare against, nothing can be called a mismatch"
        );

        // A cross-origin GET carries no payload and cannot read the reply (no CORS headers are
        // ever sent), so it keeps the same exemption a cross-site navigation has above.
        assert!(by_origin(
            Some("https://192.168.1.5:9000"),
            here,
            &Method::GET
        ));
        assert!(!by_origin(
            Some("https://192.168.1.5:9000"),
            here,
            &Method::DELETE
        ));
    }

    /// The default port is omitted from `Origin` *and* from `Host`, so the two still agree.
    /// Pinned because "compare the port too" invites hand-normalising one side only.
    #[test]
    fn a_default_port_is_absent_from_both_sides_and_still_matches() {
        assert!(origin_names_target("https://nest.local", "nest.local"));
        assert!(!origin_names_target(
            "https://nest.local",
            "nest.local:8443"
        ));
        assert!(!origin_names_target(
            "https://nest.local:8443",
            "nest.local"
        ));
    }
}
