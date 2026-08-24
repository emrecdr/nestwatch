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
/// parent's session cookie to requests it makes here. Seven `/api` endpoints take no JSON body
/// (`.../kill`, `/shutdown`, `/lock`, `.../approve`, `.../deny`, `.../apply`, `.../delete`), so
/// they never trigger the `Content-Type: application/json` preflight that protects the rest —
/// a plain form POST reaches them with the cookie attached. The child owns an account on this
/// PC and can serve such a page from it.
///
/// `Sec-Fetch-Site` distinguishes `same-origin` from `same-site`, which is exactly the
/// distinction the cookie attribute can't make. Browsers forbid page scripts from setting any
/// `Sec-` header, so the value can't be forged from JavaScript.
///
/// **Policy.** Allow when the header is absent (a non-browser client — `curl`, a probe — or a
/// browser too old to send it; those carry no ambient cookie authority to abuse), or when it
/// says `same-origin` or `none` (`none` is a user-initiated load: a typed URL, a bookmark, the
/// pairing QR). Otherwise allow only a top-level navigation `GET`, so following a link to the
/// dashboard from a chat message still works, and reject everything else.
pub async fn require_same_origin(request: Request, next: Next) -> Result<Response, StatusCode> {
    let headers = request.headers();
    let site = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok());
    let mode = headers.get("sec-fetch-mode").and_then(|v| v.to_str().ok());
    let allowed = is_same_origin(site, mode, request.method());

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

/// The policy behind [`require_same_origin`], as a pure function of the two fetch-metadata
/// headers and the request method — so it is testable without standing up a router, the same
/// way [`is_lan`] is.
fn is_same_origin(site: Option<&str>, mode: Option<&str>, method: &Method) -> bool {
    match site {
        None | Some("same-origin") | Some("none") => true,
        // Cross-site or same-site: only a top-level navigation that can't carry a payload.
        // A form submission is *also* a navigation, which is why the method is checked too.
        Some(_) => mode == Some("navigate") && matches!(*method, Method::GET | Method::HEAD),
    }
}

/// Content-Security-Policy for the embedded single-page UI.
///
/// `default-src 'none'` then allow only what the page uses. `'unsafe-inline'`/`'unsafe-eval'`
/// are required by the current Alpine.js build (it compiles inline attribute expressions and
/// there's an inline `<script>`); tightening to a nonce-free strict policy would mean adopting
/// the `@alpinejs/csp` build and externalizing the inline script — deferred. `img-src` allows
/// `blob:` (screenshot object URLs) and `data:` (DaisyUI's inline-SVG backgrounds).
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
     script-src 'self' 'unsafe-inline' 'unsafe-eval'; \
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

    #[test]
    fn only_this_exact_origin_may_make_a_request_with_a_payload() {
        let post = Method::POST;

        // The gap `SameSite=Strict` leaves open: another port on this same host is *same-site*.
        assert!(!is_same_origin(Some("same-site"), Some("cors"), &post));
        assert!(!is_same_origin(Some("cross-site"), Some("cors"), &post));

        // A form submission is a navigation too, so "allow navigations" alone would reopen it.
        assert!(!is_same_origin(Some("same-site"), Some("navigate"), &post));
        assert!(!is_same_origin(Some("cross-site"), Some("navigate"), &post));

        // The dashboard's own calls, and a user-initiated load (typed URL, bookmark, QR).
        assert!(is_same_origin(Some("same-origin"), Some("cors"), &post));
        assert!(is_same_origin(Some("none"), Some("navigate"), &Method::GET));

        // Following a link to the dashboard from elsewhere must still work.
        assert!(is_same_origin(
            Some("cross-site"),
            Some("navigate"),
            &Method::GET
        ));

        // Non-browser clients (curl, probes) send no fetch metadata and carry no ambient
        // cookie authority for a third party to abuse.
        assert!(is_same_origin(None, None, &post));
    }
}
