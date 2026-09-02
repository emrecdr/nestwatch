//! `O77`: which devices hold a session, and signing out exactly one of them.
//!
//! Before this, `api::change_password` was the only revocation there was, and it calls
//! `clear_all()` — so ending one leaked cookie signed out every device in the house and forced a
//! re-pair of each. That is a remedy expensive enough to postpone, which is the worst property a
//! revocation lever can have. `docs/REMOTE-ACCESS.md` makes this the precondition for reaching
//! the dashboard from outside: the tunnel handles the network, the phone holds everything else.
//!
//! **Its own binary**, like `earned_grant.rs` and `pairing_scope.rs`, for the same reason — these
//! sections persist sessions, so they need the process-wide `NESTWATCH_DATA_DIR` override.

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use nestwatch::pairing::Scope;

mod common;
use common::{PASSWORD, ScratchDir, app_with, login, state_with, test_config};

/// Pair against a freshly minted token of `scope`, returning the cookie it produced.
async fn pair_with(app: &axum::Router, scope: Scope, agent: &str) -> String {
    let token = nestwatch::pairing::mint(&nestwatch::config::data_paths().pairing, scope)
        .expect("minting a pairing token");
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/p/{token}"))
                .header(header::USER_AGENT, agent)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    res.headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|c| c.split(';').next())
        .map(str::to_owned)
        .expect("pairing must produce a session cookie")
}

async fn get_json(app: &axum::Router, cookie: &str, uri: &str) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn revoke(app: &axum::Router, cookie: &str, handle: &str) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/sessions/{handle}/revoke"))
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A session stops working once it is old, however much it has been used.
///
/// **The idle window cannot express this.** `Expiry::OnInactivity` slides — `require_auth`
/// refreshes it on activity — so a device opened daily held a session that never expired. OWASP
/// asks for an absolute timeout alongside the idle one for exactly that reason, and NIST SP
/// 800-63B caps a single-factor session at 30 days.
///
/// The record is aged by hand because the alternative is a test that takes a month. That is also
/// the honest shape of the thing under test: what `require_auth` reads is a stored `first_seen`,
/// so a stored `first_seen` is what this moves.
#[tokio::test]
async fn a_session_expires_on_age_even_if_it_is_used_every_day() {
    let tmp = ScratchDir::new("sessage");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };

    let state = state_with(test_config());
    let sessions = state.sessions.clone();
    let app = app_with(state);
    let cookie = login(&app, PASSWORD).await.unwrap();

    // Fresh: works, and would go on working under the idle window alone for as long as it is used.
    let (status, _) = get_json(&app, &cookie, "/api/sessions").await;
    assert_eq!(status, StatusCode::OK);

    // Age the session past the cap, leaving the *expiry* untouched — an actively used session
    // always has a future expiry, which is the whole point: only the absolute rule can catch it.
    let mut record = sessions.snapshot().into_iter().next().expect("one session");
    let old = tower_sessions::cookie::time::OffsetDateTime::now_utc().unix_timestamp()
        - (nestwatch::auth::SESSION_MAX_DAYS * 86_400 + 60);
    let device = record.data.get_mut("device").expect("device record");
    device["first_seen"] = json!(old);
    assert!(
        record.expiry_date > tower_sessions::cookie::time::OffsetDateTime::now_utc(),
        "the idle window must still be in the future, or this proves nothing"
    );
    tower_sessions::SessionStore::save(&sessions, &record)
        .await
        .expect("ageing the session");

    let (status, _) = get_json(&app, &cookie, "/api/sessions").await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a session past its absolute age must stop working even though its idle window is still \
         open — the idle window slides, so on a daily-used device it never closes"
    );
    assert!(
        sessions.snapshot().is_empty(),
        "and the dead record is dropped, so it stops being re-rejected on every later request \
         and stops appearing in the Signed-in devices card"
    );
}

#[tokio::test]
async fn devices_are_listed_and_revoked_one_at_a_time() {
    let tmp = ScratchDir::new("sessdev");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };

    let state = state_with(test_config());
    let sessions = state.sessions.clone();
    let app = app_with(state);

    // Three principals: the parent's browser, a paired phone, and an integration.
    let browser = login(&app, PASSWORD).await.unwrap();
    let phone = pair_with(
        &app,
        Scope::Dashboard,
        "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0)",
    )
    .await;
    let robot = pair_with(
        &app,
        Scope::Integration {
            source: "studygo".into(),
        },
        "Voortgang/1.0",
    )
    .await;

    // --- The list names all three, and says what each may do. ---------------------------
    let (status, rows) = get_json(&app, &browser, "/api/sessions").await;
    assert_eq!(status, StatusCode::OK);
    let rows = rows.as_array().unwrap().clone();
    assert_eq!(rows.len(), 3, "every signed-in device appears: {rows:#?}");

    let current: Vec<&Value> = rows
        .iter()
        .filter(|r| r["current"] == json!(true))
        .collect();
    assert_eq!(
        current.len(),
        1,
        "exactly one row is the caller's own, or the parent cannot tell which they are about to \
         sign themselves out of"
    );
    assert_eq!(current[0]["scope"], json!({ "kind": "dashboard" }));

    let robot_row = rows
        .iter()
        .find(|r| r["scope"]["kind"] == json!("integration"))
        .expect("the integration pairing is a device too");
    assert_eq!(robot_row["scope"]["source"], json!("studygo"));
    assert_eq!(
        robot_row["user_agent"],
        json!("Voortgang/1.0"),
        "the device is described by what it announced, not by where the packet came from — a \
         router that masquerades the tunnel gives every remote device one address"
    );

    // --- **A handle is not a session id.** ----------------------------------------------
    //
    // The whole point of this card is containing a leaked cookie, so it must not hand one out.
    // The cookie value is `hh_session=<id>`; no handle may contain it, and none may be it.
    let raw_id = browser.split('=').nth(1).unwrap_or_default();
    assert!(!raw_id.is_empty());
    for row in &rows {
        let handle = row["handle"].as_str().unwrap();
        assert_ne!(handle, raw_id, "a handle must never be the session id");
        assert!(
            !raw_id.contains(handle) && !handle.contains(raw_id),
            "a handle must not reveal any part of the session id: {handle}"
        );
    }

    // --- Revoking one leaves the others alone. ------------------------------------------
    let phone_handle = rows
        .iter()
        .find(|r| r["user_agent"].as_str().unwrap_or("").contains("iPhone"))
        .expect("the phone is listed")["handle"]
        .as_str()
        .unwrap()
        .to_owned();

    let (status, body) = revoke(&app, &browser, &phone_handle).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["was_current"], json!(false));

    // The phone is out…
    let (status, _) = get_json(&app, &phone, "/api/usage/today").await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the revoked device is signed out"
    );
    // …and nobody else was touched. This is the whole difference from `clear_all`.
    let (status, _) = get_json(&app, &browser, "/api/sessions").await;
    assert_eq!(status, StatusCode::OK, "the parent stays signed in");
    let (status, _) = get_json(&app, &robot, "/api/usage/today").await;
    assert_eq!(status, StatusCode::OK, "the integration stays signed in");
    assert_eq!(sessions.snapshot().len(), 2, "exactly one session went");

    // --- A handle that matches nothing is a 404, not a cheerful 200. ---------------------
    let (status, _) = revoke(&app, &browser, "deadbeefcafe").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "reporting success for a device that was not there tells a parent they revoked something \
         they did not"
    );

    // --- An integration cannot read or revoke devices. -----------------------------------
    let (status, _) = get_json(&app, &robot, "/api/sessions").await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "the device list is a parent's view; a scoped integration has no business reading it"
    );

    // --- Revoking yourself is allowed, and says so. --------------------------------------
    let (_, rows) = get_json(&app, &browser, "/api/sessions").await;
    let own = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["current"] == json!(true))
        .expect("own row")["handle"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, body) = revoke(&app, &browser, &own).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["was_current"],
        json!(true),
        "the page needs to know it just signed itself out, or it re-renders a dashboard with no \
         session behind it"
    );
    let (status, _) = get_json(&app, &browser, "/api/sessions").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
