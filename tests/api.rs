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
    PASSWORD, app_with, body_json, get, login, post_json, state_with, test_app, test_config,
    test_state,
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
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/screenshot{query}"))
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
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

    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/screenshot")
                .header(header::COOKIE, &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

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
    assert_eq!(body["code"].as_str().unwrap().len(), 8);

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
    let dir = std::env::temp_dir().join(format!("nw-logout-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("audit.jsonl");

    let mut state = state_with(test_config());
    state.audit = std::sync::Arc::new(nestwatch::audit::AuditLog::new(log.clone()));
    let app = app_with(state);

    for _ in 0..200 {
        let res = post_json(&app, "/logout", None, json!({})).await;
        assert_eq!(res.status(), StatusCode::OK);
    }

    let lines = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .count();
    let _ = std::fs::remove_dir_all(&dir);
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
