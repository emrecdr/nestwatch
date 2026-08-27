//! Password-change end-to-end. In its own test binary so its `NESTWATCH_DATA_DIR` override
//! runs in a separate process and can't affect (or be affected by) the other integration tests.

use axum::Router;
use axum::http::StatusCode;
use serde_json::json;

mod common;
use common::{PASSWORD, ScratchDir, app_with, get, login, post_json, state_with, test_config};

/// Point the data dir at a scratch path for this binary. Safe here and nowhere else: this is a
/// dedicated test binary, so the process-global override can't race another test.
fn scratch_data_dir(name: &str) -> ScratchDir {
    let tmp = ScratchDir::new(&format!("pw-{name}"));
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };
    tmp
}

async fn change_password(app: &Router, cookie: &str, new: &str) -> StatusCode {
    post_json(
        app,
        "/api/password",
        Some(cookie),
        json!({ "current": PASSWORD, "new": new }),
    )
    .await
    .status()
}

#[tokio::test]
async fn password_change_end_to_end() {
    let _tmp = scratch_data_dir("e2e");
    let app = app_with(state_with(test_config()));

    let cookie = login(&app, PASSWORD).await.unwrap();
    assert_eq!(
        change_password(&app, &cookie, "a-fresh-passphrase").await,
        StatusCode::OK
    );

    // The new password now works; the old one does not.
    assert!(login(&app, "a-fresh-passphrase").await.is_some());
    assert!(login(&app, PASSWORD).await.is_none());
}

/// Changing the password must sign other devices out.
///
/// Sessions persist across restarts now, so this is the only way to revoke a leaked cookie
/// before its 30-day expiry — and it's what a parent expects "change the password" to do.
/// Previously the implicit remedy was a service restart, which the persistent store removed.
#[tokio::test]
async fn changing_the_password_revokes_other_sessions() {
    let _tmp = scratch_data_dir("revoke");
    let app = app_with(state_with(test_config()));

    let phone = login(&app, PASSWORD).await.expect("first device signs in");
    let laptop = login(&app, PASSWORD).await.expect("second device signs in");

    // Both live before the change.
    for cookie in [&phone, &laptop] {
        assert_eq!(
            get(&app, "/api/curfew", Some(cookie)).await.status(),
            StatusCode::OK
        );
    }

    assert_eq!(
        change_password(&app, &phone, "a-much-longer-new-passphrase").await,
        StatusCode::OK
    );

    assert_eq!(
        get(&app, "/api/curfew", Some(&laptop)).await.status(),
        StatusCode::UNAUTHORIZED,
        "a password change must revoke sessions on other devices"
    );
}
