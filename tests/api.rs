//! HTTP-level integration tests. They drive the real router (via `tower`'s `oneshot`)
//! backed by `FakeControl`, so they run on any OS with no real side effects — this is the
//! payoff of the `SystemControl` abstraction.

use std::net::SocketAddr;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt; // for `oneshot`

use nestwatch::server::build_router;

mod common;
use common::{
    PASSWORD, ScratchDir, app_with, app_with_audit_file, body_json, get, login, post_json,
    state_with, test_app, test_config, test_state,
};

#[tokio::test]
async fn api_requires_auth() {
    let app = test_app();
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/processes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn wrong_password_is_rejected() {
    let app = test_app();
    assert!(login(&app, "not-the-password").await.is_none());
}

#[tokio::test]
async fn session_endpoint_reflects_auth_state() {
    let app = test_app();

    // Anonymous.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/session")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(body_json(res).await["authenticated"], json!(false));

    // Authenticated.
    let cookie = login(&app, PASSWORD).await.expect("login should succeed");
    let res = app
        .oneshot(
            Request::builder()
                .uri("/session")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(body_json(res).await["authenticated"], json!(true));
}

/// Fetch a capture, returning its content-type and body.
async fn shot(app: &axum::Router, cookie: &str, query: &str) -> (String, Vec<u8>) {
    let res = get(app, &format!("/api/screenshot{query}"), Some(cookie)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let mime = res
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    (mime, bytes.to_vec())
}

#[tokio::test]
async fn screenshot_returns_jpeg() {
    let app = test_app();
    let cookie = login(&app, PASSWORD).await.unwrap();

    let (mime, bytes) = shot(&app, &cookie, "").await;
    assert_eq!(mime, "image/jpeg");
    // SOI marker. JPEG has no ASCII signature, so this is the whole of what identifies one.
    assert_eq!(&bytes[..2], &[0xFF, 0xD8], "JPEG magic bytes present");
}

/// The tier must reach the capture and change what comes back.
///
/// Asserting on **size** rather than on decoded dimensions is deliberate: bytes-on-the-wire is the
/// entire point of the tier, and a test that only checked the pixel count would still pass if the
/// preview were downscaled and then encoded at a quality that undid the saving.
///
/// The fake's source frame is 1280x720 — larger than the preview box on purpose. It used to be
/// 320x180, and at that size `encode_shot` returns a frame smaller than the box untouched, so both
/// tiers produced identical bytes and this test could not have failed however broken the plumbing.
#[tokio::test]
async fn the_preview_tier_is_far_smaller_than_the_full_one() {
    let app = test_app();
    let cookie = login(&app, PASSWORD).await.unwrap();

    let (_, full) = shot(&app, &cookie, "?tier=full").await;
    let (_, preview) = shot(&app, &cookie, "?tier=preview").await;

    assert!(
        preview.len() * 2 < full.len(),
        "preview ({} B) must be dramatically smaller than full ({} B), not incidentally so — \
         if these are close the tier is not reaching the capture",
        preview.len(),
        full.len()
    );
}

/// Absent and unrecognised both mean full, so a typo costs bandwidth rather than silently handing
/// a parent a blurry picture at the moment they asked for a sharp one.
#[tokio::test]
async fn an_absent_or_unknown_tier_falls_back_to_full() {
    let app = test_app();
    let cookie = login(&app, PASSWORD).await.unwrap();

    let (_, full) = shot(&app, &cookie, "?tier=full").await;
    for query in ["", "?tier=", "?tier=PREVIEW", "?tier=medium"] {
        let (_, got) = shot(&app, &cookie, query).await;
        assert_eq!(
            got.len(),
            full.len(),
            "`/api/screenshot{query}` must return the full tier"
        );
    }
}

/// A capture of a child's desktop must never be written to a browser's disk cache.
#[tokio::test]
async fn a_capture_is_not_cacheable() {
    let app = test_app();
    let cookie = login(&app, PASSWORD).await.unwrap();

    let res = get(&app, "/api/screenshot", Some(&cookie)).await;

    assert_eq!(
        res.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
}

#[tokio::test]
async fn curfew_get_and_validation() {
    let app = test_app();
    let cookie = login(&app, PASSWORD).await.unwrap();

    // GET returns the default (disabled) curfew.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/curfew")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res).await["enabled"], json!(false));

    // POST with a malformed time is rejected (400) before anything is persisted.
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/curfew")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"enabled": true, "start": "25:99", "end": "07:00", "warn_secs": 60})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn processes_list_then_kill() {
    let app = test_app();
    let cookie = login(&app, PASSWORD).await.unwrap();

    // List includes a known fake process.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/processes")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let list = body_json(res).await;
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "notepad.exe")
    );

    // Kill an existing PID → 200.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/processes/1005/kill")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // Kill a non-existent PID → 404.
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/processes/999999/kill")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn off_lan_client_is_forbidden() {
    // A public source IP must be rejected by the app itself, before auth — even for the
    // login page — so a missing firewall rule doesn't equal exposure.
    let app = build_router(test_state())
        .layer(MockConnectInfo(SocketAddr::from(([203, 0, 113, 7], 5555))));
    let res = app
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn security_headers_are_present() {
    let res = test_app()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let h = res.headers();
    assert!(
        h.get(header::CONTENT_SECURITY_POLICY).is_some(),
        "CSP present"
    );
    assert_eq!(h.get("x-frame-options").unwrap(), "DENY");
    assert_eq!(h.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(), "nosniff");
}

#[tokio::test]
async fn usage_requires_auth_and_returns_array() {
    let app = test_app();
    // Unauthenticated → 401.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/usage")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Authenticated → 200 with an array (empty, since the log is disabled in tests).
    let cookie = login(&app, PASSWORD).await.unwrap();
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/usage")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(body_json(res).await.is_array());
}

#[tokio::test]
async fn usage_today_requires_auth_and_returns_summary() {
    let app = test_app();
    // Unauthenticated → 401.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/usage/today")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Authenticated → 200 with the summary shape (no budget configured in tests → 0 / null).
    let cookie = login(&app, PASSWORD).await.unwrap();
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/usage/today")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    assert_eq!(body["budget_mins"], 0);
    assert!(body["remaining_mins"].is_null());
    assert!(body["per_app"].is_array());
}

#[tokio::test]
async fn screentime_requires_auth_and_defaults_to_thirty_days() {
    let app = test_app();

    // Unauthenticated must not reach it.
    let res = get(&app, "/api/screentime", None).await;
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "screen-time history must sit behind require_auth"
    );

    let cookie = login(&app, PASSWORD).await.expect("login");
    let res = get(&app, "/api/screentime", Some(&cookie)).await;
    assert_eq!(res.status(), StatusCode::OK);

    let body = body_json(res).await;
    assert_eq!(
        body["days"].as_array().unwrap().len(),
        30,
        "default window is 30 days"
    );
}

#[tokio::test]
async fn screentime_days_is_clamped() {
    let app = test_app();
    let cookie = login(&app, PASSWORD).await.expect("login");

    for (requested, expected) in [("0", 1usize), ("9999", 365usize)] {
        let uri = format!("/api/screentime?days={requested}");
        let res = get(&app, &uri, Some(&cookie)).await;
        assert_eq!(res.status(), StatusCode::OK);

        let body = body_json(res).await;
        assert_eq!(
            body["days"].as_array().unwrap().len(),
            expected,
            "days={requested} must clamp to {expected}"
        );
    }
}

#[tokio::test]
async fn extra_time_requires_auth_and_validates_range() {
    let app = test_app();

    // Unauthenticated → 401.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/extra-time")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"minutes":30}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let cookie = login(&app, PASSWORD).await.unwrap();

    // Zero minutes → 400. Over-range (>240) → 400. Neither reaches the persistence path, so this
    // test never writes the real config; the successful grant + persistence lives in
    // `rules_persist.rs`, which redirects the data dir.
    for bad in [r#"{"minutes":0}"#, r#"{"minutes":9999}"#] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/extra-time")
                    .header(header::COOKIE, &cookie)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(bad))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn time_codes_parent_endpoints_require_auth_and_issue() {
    let app = test_app();

    // Parent list/issue require auth.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/time-codes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let cookie = login(&app, PASSWORD).await.unwrap();

    // Issue returns an 8-char code (the disabled store still mints one, just doesn't persist).
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/time-codes")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"minutes":30}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    assert_eq!(body["minutes"], 30);
    // Six characters, on the wire as well as in the type — this is the string a child reads off
    // a note and retypes, so its length is part of the endpoint's contract, not an internal
    // detail. See `timecode`'s module doc for why six is safe: the redeem throttle is what
    // makes guessing infeasible, not the length.
    assert_eq!(body["code"].as_str().unwrap().len(), 6);

    // Out-of-range minutes → 400.
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/time-codes")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"minutes":0}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn redeem_code_is_lan_gated_not_auth_gated() {
    // The child redeem endpoint takes no cookie (loopback is on the LAN allowlist). With the
    // disabled store no code is active, so it answers 200 {ok:false} — leaking nothing and never
    // touching the real config.
    let res = test_app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/redeem-code")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"code":"ABCD1234"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res).await["ok"], json!(false));
}

#[tokio::test]
async fn routines_require_auth() {
    let app = test_app();
    for (method, uri) in [
        ("GET", "/api/routines"),
        ("POST", "/api/routines"),
        ("POST", "/api/routines/Homework/apply"),
        ("POST", "/api/routines/Homework/delete"),
    ] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
}

#[tokio::test]
async fn rules_get_and_validation() {
    let app = test_app();
    let cookie = login(&app, PASSWORD).await.unwrap();

    // GET returns the default rules (no budget).
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/rules")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res).await["daily_budget_mins"], json!(0));

    // POST with an over-large warn is rejected (400).
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/rules")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "daily_budget_mins": 120, "warn_secs": 9999 }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn lock_endpoint_ok() {
    let app = test_app();
    let cookie = login(&app, PASSWORD).await.unwrap();
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/lock")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res).await["ok"], json!(true));
}

// Helper: POST /api/password with the given body, returning the response.
async fn post_password(app: &Router, cookie: &str, body: Value) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/password")
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn password_change_requires_auth() {
    // No cookie → blocked by require_auth before the handler runs.
    let res = test_app()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/password")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "current": PASSWORD, "new": "a-brand-new-pass" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn password_change_rejects_wrong_current() {
    let app = test_app();
    let cookie = login(&app, PASSWORD).await.unwrap();
    let res = post_password(
        &app,
        &cookie,
        json!({ "current": "not-the-password", "new": "a-brand-new-pass" }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn password_change_rejects_short_new() {
    let app = test_app();
    let cookie = login(&app, PASSWORD).await.unwrap();
    let res = post_password(
        &app,
        &cookie,
        json!({ "current": PASSWORD, "new": "short" }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

// Helper: POST /time-request from a given mock peer IP.
async fn post_time_request(app: &Router, body: Value) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/time-request")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn time_request_is_lan_gated_but_not_auth_gated() {
    // No cookie, loopback peer → accepted (proves it's outside require_auth).
    let app = test_app();
    let res = post_time_request(&app, json!({ "minutes": 30, "reason": "homework" })).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res).await["ok"], json!(true));
}

/// An unauthenticated `POST /logout` must write nothing to the audit log.
///
/// Regression: `/logout` needs no cookie and no body and isn't throttled, and it used to audit
/// unconditionally — measured at ~11,500 lines in 3 seconds, which rolls the 2 MiB log and its
/// single backup off disk in about 20 seconds, destroying the record of every real login, kill
/// and shutdown. Same defect previously found in `login` and in the pairing endpoint.
#[tokio::test]
async fn unauthenticated_logout_cannot_flood_the_audit_log() {
    let audit = app_with_audit_file("logout");
    let app = &audit.app;
    let log = &audit.path;

    for _ in 0..200 {
        let res = post_json(app, "/logout", None, json!({})).await;
        assert_eq!(res.status(), StatusCode::OK);
    }

    let lines = std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .count();
    assert_eq!(
        lines, 0,
        "200 cookie-less logouts wrote {lines} audit lines; they must write none"
    );
}

/// `/status` is the child's own screen-time view. It must work with no cookie (they're not the
/// parent) and must expose only totals — never the rules themselves, or it becomes a map for
/// planning around them.
#[tokio::test]
async fn child_status_is_unauthenticated_and_leaks_no_rules() {
    let mut config = test_config();
    config.rules.daily_budget_mins = 90;
    config.rules.blocklist = vec!["minecraft.exe".into()];
    config.rules.app_limits.insert("chrome.exe".into(), 30);

    let app = app_with(state_with(config));
    let res = get(&app, "/status", None).await;
    assert_eq!(res.status(), StatusCode::OK, "no cookie should be needed");

    let body = body_json(res).await;
    assert_eq!(body["limited"], json!(true));
    assert_eq!(body["budget_mins"], json!(90));
    assert!(body["remaining_mins"].is_number());

    let raw = body.to_string();
    for secret in ["minecraft", "chrome", "blocklist", "app_limits", "curfew"] {
        assert!(
            !raw.contains(secret),
            "child status must not reveal `{secret}`: {raw}"
        );
    }
}

/// With no budget configured, say so explicitly rather than reporting a meaningless `0` left.
#[tokio::test]
async fn child_status_reports_no_limit_when_none_is_set() {
    let app = test_app();
    let res = get(&app, "/status", None).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(body_json(res).await["limited"], json!(false));
}

#[tokio::test]
async fn child_status_rejected_off_lan() {
    let app = build_router(test_state())
        .layer(MockConnectInfo(SocketAddr::from(([203, 0, 113, 7], 5555))));
    let res = get(&app, "/status", None).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn time_request_rejected_off_lan() {
    let app = build_router(test_state())
        .layer(MockConnectInfo(SocketAddr::from(([203, 0, 113, 7], 5555))));
    let res = post_time_request(&app, json!({ "minutes": 30 })).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn time_request_validates_minutes() {
    let app = test_app();
    let res = post_time_request(&app, json!({ "minutes": 0 })).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn time_request_is_rate_limited() {
    // The default SubmitLimiter allows 5/min per IP; the 6th from the same mock peer → 429.
    let app = test_app();
    for _ in 0..5 {
        let res = post_time_request(&app, json!({ "minutes": 10 })).await;
        assert_eq!(res.status(), StatusCode::OK);
    }
    let res = post_time_request(&app, json!({ "minutes": 10 })).await;
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn parent_time_request_endpoints_require_auth() {
    let app = test_app();
    for (method, uri) in [
        ("GET", "/api/time-requests"),
        ("POST", "/api/time-requests/abc/approve"),
        ("POST", "/api/time-requests/abc/deny"),
    ] {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED, "{method} {uri}");
    }

    // Authenticated GET returns an (empty) array.
    let cookie = login(&app, PASSWORD).await.unwrap();
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/time-requests")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(body_json(res).await.is_array());
}

/// The response must name the tier it actually served, rather than leaving the client to assume it
/// got what it asked for.
///
/// [`ShotTier::from_arg`] maps unknown **and** absent to `Full`, so a typo in the client's query
/// string — `?tier=preveiw` — silently returns a full frame on a two-second timer while the client
/// records "preview". `as_arg`'s own doc names the failure: "no error, no failing test, just the
/// cost back". A header is what turns that from an assumption into an answer, and it is also what
/// lets the overlay's `shotTier !== "full"` check mean anything.
#[tokio::test]
async fn a_capture_names_the_tier_it_actually_served() {
    let app = test_app();
    let cookie = login(&app, PASSWORD).await.unwrap();

    for (query, expect) in [
        ("?tier=preview", "preview"),
        ("?tier=full", "full"),
        ("", "full"),
        ("?tier=preveiw", "full"),
    ] {
        let res = get(&app, &format!("/api/screenshot{query}"), Some(&cookie)).await;
        assert_eq!(
            res.headers()
                .get("x-shot-tier")
                .map(|v| v.to_str().unwrap().to_string()),
            Some(expect.to_string()),
            "`/api/screenshot{query}` served the {expect} tier and must say so on the wire"
        );
    }
}

/// A capture the **live timer** asked for must be coalesced, whatever tier it carries.
///
/// The audit used to key on tier as a proxy for "who asked": full meant a person had pressed a
/// button, so one line each stayed bounded by human action, exactly as `SECURITY.md` describes.
/// That proxy broke when live frames started following the visible surface — with the full-size
/// view open the timer now requests `full` every two seconds. Audited one-for-one that is ~1,800
/// rows an hour, and `audit.jsonl` rotates at 2 MiB keeping one backup, so it would evict the
/// entire security history — every login, every kill, every password change — to make room for a
/// timer. That is the precise failure the preview coalescer was built to prevent, arriving through
/// the other tier. The audit now keys on **who asked**, which is what it always meant.
#[tokio::test]
async fn a_timer_driven_capture_is_coalesced_whatever_tier_it_carries() {
    let audit = app_with_audit_file("shotaudit-live");
    let app = &audit.app;
    let cookie = login(app, PASSWORD).await.unwrap();

    for _ in 0..5 {
        let res = get(app, "/api/screenshot?tier=full&live=1", Some(&cookie)).await;
        assert_eq!(res.status(), StatusCode::OK);
    }

    let log = std::fs::read_to_string(&audit.path).unwrap_or_default();
    assert_eq!(
        log.matches("screenshot_taken").count(),
        0,
        "a frame nobody asked for by hand must never be audited one-for-one:\n{log}"
    );
}

/// A live session must actually *leave a record*, carrying how many frames it stands for.
///
/// The negative half — that timer frames write no `screenshot_taken` line — was covered first, and
/// on its own it is satisfied by writing nothing at all. Deleting the `if let` in `api::screenshot`
/// so the coalescer's count is computed and discarded left every test in this binary green: it is
/// `observe`'s return value that carries the count, `observe` is not `#[must_use]`, so the value
/// can be dropped in silence and the log simply stops recording that the screen was watched.
///
/// That is the failure worth fearing here. A parent reading the audit log to answer "was anyone
/// looking at this machine?" would get the same empty answer whether nobody looked or the line was
/// never written — and it is the coalesced line, not the per-frame one, that is supposed to carry
/// that fact.
#[tokio::test]
async fn a_live_session_writes_one_coalesced_line_carrying_its_frame_count() {
    let audit = app_with_audit_file("shotaudit-count");
    let app = &audit.app;
    let cookie = login(app, PASSWORD).await.unwrap();

    for _ in 0..5 {
        let res = get(app, "/api/screenshot?tier=preview&live=1", Some(&cookie)).await;
        assert_eq!(res.status(), StatusCode::OK);
    }

    let log = std::fs::read_to_string(&audit.path).unwrap_or_default();
    assert_eq!(
        log.matches("live_view").count(),
        1,
        "five timer frames must leave exactly one coalesced line — the first after a quiet spell \
         always reports, and the rest fold into it until the window closes:\n{log}"
    );
    assert!(
        log.contains("\"frames\""),
        "the coalesced line must carry the frame count it stands for, or it records that the \
         screen was watched without recording for how long:\n{log}"
    );
}

/// A capture a **person** asked for is still audited one line each — the property that makes the
/// log worth reading. Bounded by a human action, so it cannot run away.
#[tokio::test]
async fn a_capture_a_person_asked_for_is_still_audited_one_for_one() {
    let audit = app_with_audit_file("shotaudit-human");
    let app = &audit.app;
    let cookie = login(app, PASSWORD).await.unwrap();

    for _ in 0..3 {
        let res = get(app, "/api/screenshot?tier=full", Some(&cookie)).await;
        assert_eq!(res.status(), StatusCode::OK);
    }

    let log = std::fs::read_to_string(&audit.path).unwrap_or_default();
    assert_eq!(
        log.matches("screenshot_taken").count(),
        3,
        "each deliberate capture is one line:\n{log}"
    );
}

/// The child asks, the parent answers, and the child's own page is where the answer lands.
///
/// Before this, `/status` returned four numbers and none of them was the state of the request the
/// page exists to make. A denial reached the child through no channel at all — it was
/// indistinguishable from being ignored — and an approval showed up only as a number that changed
/// by itself. Driven end to end through the real routes, because the value is in the round trip.
///
/// Builds its own queue rather than using `test_state`'s, which is `TimeRequests::disabled()` —
/// a no-op store whose `latest()` is permanently `None`, so this test would pass against a
/// completely broken implementation.
#[tokio::test]
async fn child_status_reports_what_happened_to_the_request() {
    let dir = ScratchDir::new("status-request");

    let mut state = test_state();
    state.time_requests =
        std::sync::Arc::new(nestwatch::timereq::TimeRequests::new(dir.join("req.jsonl")));
    let app = app_with(state);
    let app = &app;

    // Nothing asked for yet.
    let body = body_json(get(app, "/status", None).await).await;
    assert_eq!(
        body["request"],
        json!(null),
        "no request, no answer to report"
    );

    // The child asks — no cookie; this endpoint is theirs.
    let res = post_json(
        app,
        "/time-request",
        None,
        json!({ "minutes": 25, "reason": "maths" }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);

    let body = body_json(get(app, "/status", None).await).await;
    assert_eq!(body["request"]["state"], json!("pending"));
    assert_eq!(body["request"]["minutes"], json!(25));

    // The parent denies it.
    let cookie = login(app, PASSWORD).await.expect("login");
    let queue = body_json(get(app, "/api/time-requests", Some(&cookie)).await).await;
    let id = queue[0]["id"]
        .as_str()
        .expect("a pending request")
        .to_string();
    let res = post_json(
        app,
        &format!("/api/time-requests/{id}/deny"),
        Some(&cookie),
        json!({}),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);

    // …and the child is told, which is the whole point.
    let body = body_json(get(app, "/status", None).await).await;
    assert_eq!(
        body["request"]["state"],
        json!("denied"),
        "a denial must reach the child; silence used to be the only signal"
    );

    // Still no rules on this endpoint — the neighbouring guard stays true with the new field.
    let raw = body.to_string();
    for secret in ["blocklist", "app_limits", "curfew"] {
        assert!(
            !raw.contains(secret),
            "child status must not reveal `{secret}`: {raw}"
        );
    }
}

/// An error a person reads must carry the number they need, not only the verdict.
///
/// `Rules::validate`'s five messages already do this ("daily limit must be <= 10080 minutes"). These
/// four did not: they named *which* limit was hit and never *what it is*, so a parent told "minutes
/// out of range" has to guess what to try next. `assets/app.js` compensated by pinning the bound
/// client-side, which is a second place for the constant to live and drift.
///
/// Covers all four sites, including `POST /api/extra-time` — which is bounded by the same constant
/// as the child's request endpoint and is easy to miss when reading for "the time-code ones".
#[tokio::test]
async fn a_rejected_amount_is_told_what_the_limit_actually_is() {
    let app = test_app();
    let cookie = login(&app, PASSWORD).await.expect("login");

    let over_request = nestwatch::timereq::MAX_REQUEST_MINUTES + 1;
    let over_code = nestwatch::timecode::MAX_CODE_MINUTES + 1;

    // (route, cookie, body, the number the message must contain)
    let cases: Vec<(&str, Option<&str>, serde_json::Value, u32)> = vec![
        (
            "/api/extra-time",
            Some(cookie.as_str()),
            json!({ "minutes": over_request }),
            nestwatch::timereq::MAX_REQUEST_MINUTES,
        ),
        (
            "/time-request",
            None,
            json!({ "minutes": over_request, "reason": "" }),
            nestwatch::timereq::MAX_REQUEST_MINUTES,
        ),
        (
            "/api/time-codes",
            Some(cookie.as_str()),
            json!({ "minutes": over_code }),
            nestwatch::timecode::MAX_CODE_MINUTES,
        ),
    ];

    for (route, ck, body, bound) in cases {
        let res = post_json(&app, route, ck, body).await;
        assert_eq!(
            res.status(),
            StatusCode::BAD_REQUEST,
            "{route} should reject"
        );
        let msg = body_json(res).await["error"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            msg.contains(&bound.to_string()),
            "{route} said {msg:?} — it must name the limit ({bound}), not just report one was hit"
        );
        // Zero is rejected too, so "<= N" would be a lie; the range is what the check enforces.
        assert!(
            msg.contains('1') && msg.contains("between"),
            "{route} said {msg:?} — state the range, since 0 is rejected as well"
        );
    }
}

/// The child's endpoint is unauthenticated, so naming the bound there is worth a second look.
/// It is safe: `MAX_REQUEST_MINUTES` is a compile-time constant identical on every install, not a
/// household rule, and `ask.html` already ships it to the child as `max="240"` on the input.
/// Confirmed here so a future change to that constant cannot quietly make the two disagree.
#[tokio::test]
async fn the_bound_the_child_is_told_is_the_one_their_own_form_already_shows() {
    let markup = include_str!("../assets/ask.html");
    let want = format!("max=\"{}\"", nestwatch::timereq::MAX_REQUEST_MINUTES);
    assert!(
        markup.contains(&want),
        "ask.html must cap the minutes input at {} — the server rejects above it either way, and \
         the page should not invite a number it will refuse",
        nestwatch::timereq::MAX_REQUEST_MINUTES
    );
}

/// The fourth site: the active-code cap. Needs a real `TimeCodes` store, because `test_state`
/// installs a disabled one whose `issue` never fills up — so this would pass against any
/// implementation if it used the shared helper.
#[tokio::test]
async fn a_full_code_queue_says_how_many_codes_are_allowed() {
    let dir = ScratchDir::new("code-cap");

    let mut state = test_state();
    state.time_codes =
        std::sync::Arc::new(nestwatch::timecode::TimeCodes::new(dir.join("codes.jsonl")));
    let app = app_with(state);
    let cookie = login(&app, PASSWORD).await.expect("login");

    for i in 0..nestwatch::timecode::MAX_ACTIVE_CODES {
        let res = post_json(
            &app,
            "/api/time-codes",
            Some(&cookie),
            json!({ "minutes": 5 }),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK, "code {i} should mint");
    }

    let res = post_json(
        &app,
        "/api/time-codes",
        Some(&cookie),
        json!({ "minutes": 5 }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST, "the cap must bite");
    let msg = body_json(res).await["error"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        msg.contains(&nestwatch::timecode::MAX_ACTIVE_CODES.to_string()),
        "said {msg:?} — a parent who cannot mint a code needs the number, not just the refusal"
    );
}
