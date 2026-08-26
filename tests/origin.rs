//! Cross-origin request rejection (`Sec-Fetch-Site`).
//!
//! These pin the gap that `SameSite=Strict` structurally cannot close. A "site" is scheme +
//! registrable domain and **excludes the port**, so a page served over HTTPS from another port
//! on the child's own PC is *same-site* with the dashboard — the browser attaches the parent's
//! session cookie to requests it makes here.
//!
//! That only becomes an actual capability because seven `/api` endpoints take no JSON body, so
//! they never trigger the `Content-Type: application/json` preflight that fails closed for
//! everything else. A plain HTML form is enough to reach them.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt;

mod common;
use common::{PASSWORD, login, test_app};

/// Every `/api` endpoint that takes **no JSON body**, and is therefore reachable by a plain form
/// POST. This list is the reason the middleware exists.
///
/// Be honest about what it does here: the middleware runs *outside* the router, so it rejects
/// before routing and a made-up path is refused exactly like a real one — these assertions would
/// pass with every URI fictional. The list is documentation of the vulnerable surface, and a
/// tripwire if the layer is ever moved per-route (where the paths *would* start to matter). It is
/// not kept in sync with the router automatically, and nothing fails if an eighth is added.
const BODYLESS_POSTS: &[&str] = &[
    "/api/processes/1002/kill",
    "/api/shutdown",
    "/api/lock",
    "/api/time-requests/abc/approve",
    "/api/time-requests/abc/deny",
    "/api/routines/homework/apply",
    "/api/routines/homework/delete",
];

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    metadata: Option<(&str, &str)>,
) -> StatusCode {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    if let Some((site, mode)) = metadata {
        b = b
            .header("sec-fetch-site", site)
            .header("sec-fetch-mode", mode);
    }
    app.clone()
        .oneshot(b.body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

/// A `POST` carrying the fetch metadata a browser would attach, with the parent signed in.
async fn post(app: &Router, uri: &str, cookie: &str, site: &str, mode: &str) -> StatusCode {
    send(app, "POST", uri, Some(cookie), Some((site, mode))).await
}

/// A `POST` from a client that sends no fetch metadata at all — `curl`, a probe, an old browser.
async fn post_without_metadata(app: &Router, uri: &str, cookie: &str) -> StatusCode {
    send(app, "POST", uri, Some(cookie), None).await
}

/// A top-level `GET` navigation: following a link, opening a bookmark, scanning the pairing QR.
async fn navigate(app: &Router, uri: &str, site: &str) -> StatusCode {
    send(app, "GET", uri, None, Some((site, "navigate"))).await
}

/// The attack: a page on another port of the same host, with the parent signed in.
#[tokio::test]
async fn a_same_site_page_cannot_drive_the_bodyless_post_endpoints() {
    let app = test_app();
    let cookie = login(&app, PASSWORD).await.expect("login");

    for uri in BODYLESS_POSTS {
        let status = post(&app, uri, &cookie, "same-site", "cors").await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{uri} accepted a same-site POST carrying the parent's cookie"
        );
    }
}

/// The subtle one. An HTML form submission **is** a top-level navigation, so a policy that
/// allowed every navigation in order to keep links working would allow the exact attack it was
/// written to stop. Only `GET`/`HEAD` navigations are exempt.
///
/// One endpoint is enough here: the middleware runs *outside* the router, so the path can't
/// affect the verdict — this is checking the policy is wired, not which routes exist. The full
/// `BODYLESS_POSTS` sweep lives on the same-site test above, where the list earns its keep by
/// documenting the vulnerable surface.
#[tokio::test]
async fn a_cross_site_form_post_is_rejected_even_though_it_is_a_navigation() {
    let app = test_app();
    let cookie = login(&app, PASSWORD).await.expect("login");
    let status = post(&app, "/api/lock", &cookie, "cross-site", "navigate").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// The dashboard's own `fetch()` calls must still work end-to-end, or the fix has broken the
/// product. Asserts `OK` rather than merely "not 403", so this fails if the request is stopped
/// anywhere in the stack.
#[tokio::test]
async fn the_dashboards_own_same_origin_requests_still_work() {
    let app = test_app();
    let cookie = login(&app, PASSWORD).await.expect("login");

    for uri in ["/api/lock", "/api/processes/1002/kill"] {
        let status = post(&app, uri, &cookie, "same-origin", "cors").await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the dashboard's own {uri} was blocked"
        );
    }
}

/// Following a link to the dashboard (from a chat message, a bookmark, the pairing QR) is a
/// cross-site or user-initiated top-level `GET`. Blocking those would make the QR unusable.
#[tokio::test]
async fn a_link_or_qr_scan_to_the_dashboard_still_opens() {
    let app = test_app();

    for site in ["cross-site", "none"] {
        let status = navigate(&app, "/", site).await;
        assert_ne!(
            status,
            StatusCode::FORBIDDEN,
            "a {site} navigation to the dashboard was blocked"
        );
    }

    // The child's own page is reached the same way.
    let status = navigate(&app, "/ask", "none").await;
    assert_ne!(status, StatusCode::FORBIDDEN, "/ask was blocked");
}

/// A client that sends no fetch metadata — `curl`, a health probe, a browser too old to send it
/// — is unaffected. Those carry no ambient cookie authority for a third party to abuse, and
/// failing closed here would break every non-browser caller.
///
/// **One of those callers is now a shipped product.** The Android client in `nestwatch-mobile`
/// talks to this server with Dart's `HttpClient`, which sends no `Sec-Fetch-*` headers at all —
/// so the `None` arm of `security::is_same_origin` is the only reason it is admitted, on every
/// request it makes. That is worth naming here because the change that would break it looks
/// like hardening: tightening `None` to reject reads as closing a hole, passes every other test
/// in this file, and silently kills the phone app. Both verbs are covered below because the app
/// uses both — it polls `/api/time-requests` and posts approvals to it.
#[tokio::test]
async fn a_client_that_sends_no_fetch_metadata_is_unaffected() {
    let app = test_app();
    let cookie = login(&app, PASSWORD).await.expect("login");

    let status = post_without_metadata(&app, "/api/lock", &cookie).await;
    assert_eq!(status, StatusCode::OK, "a bodyless POST was blocked");

    // The mobile client's most frequent call, and the one a background poll makes while nobody
    // is watching it fail.
    let status = send(&app, "GET", "/api/time-requests", Some(&cookie), None).await;
    assert_eq!(status, StatusCode::OK, "a metadata-less GET was blocked");
}
