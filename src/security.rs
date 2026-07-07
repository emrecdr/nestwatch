//! HTTP-layer defense-in-depth: a network-scope gate and static security headers.
//!
//! These sit in front of every route so they apply uniformly — including the static UI, the
//! `/api/*` handlers, and error responses. Neither depends on the session, so they run before
//! any authentication work.

use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, Request};
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
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
fn is_lan(ip: IpAddr) -> bool {
    match ip {
        // RFC1918 (10/8, 172.16/12, 192.168/16) covers home LANs; loopback for dev/local.
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback(),
        // The server binds 0.0.0.0 (v4 only), so a v6 peer shouldn't occur; allow loopback only.
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// Content-Security-Policy for the embedded single-page UI.
///
/// `default-src 'none'` then allow only what the page uses. `'unsafe-inline'`/`'unsafe-eval'`
/// are required by the current Alpine.js build (it compiles inline attribute expressions and
/// there's an inline `<script>`); tightening to a nonce-free strict policy would mean adopting
/// the `@alpinejs/csp` build and externalizing the inline script — deferred. `img-src` allows
/// `blob:` (screenshot object URLs) and `data:` (DaisyUI's inline-SVG backgrounds).
const CSP: &str = "default-src 'none'; \
     script-src 'self' 'unsafe-inline' 'unsafe-eval'; \
     style-src 'self' 'unsafe-inline'; \
     img-src 'self' blob: data:; \
     connect-src 'self'; \
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
}
