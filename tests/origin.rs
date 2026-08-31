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

/// The authority every fallback test below treats as "this server".
const HERE: &str = "192.168.1.5:8443";

/// A request from a browser that sends **no** fetch metadata — Safari before 16.4 — carrying an
/// `Origin` and arriving on `HERE`.
///
/// `h2` chooses how the authority reaches the server, and it is the whole point of this helper.
/// With `h2 = false` the request is HTTP/1.1 shaped: a path-only URI plus a `Host` header. With
/// `h2 = true` it is HTTP/2 shaped: the authority on the URI and **no `Host` header at all**,
/// which is what hyper hands the middleware once `:authority` is parsed. A check that consulted
/// only `Host` passes every `h2 = false` case here and admits every `h2 = true` one.
async fn legacy_browser_post(app: &Router, origin: &str, h2: bool) -> StatusCode {
    let mut b = Request::builder().method("POST");
    if h2 {
        b = b.uri(format!("https://{HERE}/api/lock"));
    } else {
        b = b.uri("/api/lock").header(header::HOST, HERE);
    }
    let cookie = login(app, PASSWORD).await.expect("login");
    app.clone()
        .oneshot(
            b.header(header::ORIGIN, origin)
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

/// The gap `Sec-Fetch-Site` cannot close, because it did not exist yet.
///
/// Fetch metadata shipped in Safari **16.4**, March 2023. An iPad Air 2 / iPad 5 / mini 4 tops out
/// below that permanently, and that is the device a household promotes to "the thing we check the
/// dashboard on". It sends the session cookie and no `Sec-` headers, so before the `Origin`
/// fallback this exact request — a form POST from a page the child serves on another port of the
/// same PC — was **admitted**, cookie and all.
///
/// Both transports, because the answer differed between them and only one is what a browser
/// actually negotiates.
#[tokio::test]
async fn a_browser_without_fetch_metadata_cannot_post_from_another_port() {
    let app = test_app();

    for h2 in [false, true] {
        let status =
            legacy_browser_post(&app, &format!("https://{}", "192.168.1.5:9000"), h2).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "a page on another port drove /api/lock with the parent's cookie (h2 = {h2})"
        );
    }
}

/// The other half of the same change: the parent's own old iPad must still work.
#[tokio::test]
async fn the_dashboards_own_post_still_works_without_fetch_metadata() {
    let app = test_app();

    for h2 in [false, true] {
        let status = legacy_browser_post(&app, &format!("https://{HERE}"), h2).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the dashboard's own POST was blocked on a browser without fetch metadata (h2 = {h2})"
        );
    }
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

/// A client that sends no fetch metadata **and no `Origin`** — `curl`, a health probe, the
/// Android client — is unaffected.
///
/// This used to say such callers "carry no ambient cookie authority for a third party to
/// abuse". That was true of `curl` and false of the browser half, which is why the `Origin`
/// fallback above now exists. What survives is the *other* reason, and it is the load-bearing
/// one: failing closed here would break every non-browser caller.
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
