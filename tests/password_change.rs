//! Password-change end-to-end. In its own test binary so its `NESTWATCH_DATA_DIR` override
//! runs in a separate process and can't affect (or be affected by) the other integration tests.

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::json;
use tower::ServiceExt;

mod common;
use common::{PASSWORD, app_with, login, state_with, test_config};

#[tokio::test]
async fn password_change_end_to_end() {
    let tmp = std::env::temp_dir().join(format!("nw-pw-{}", std::process::id()));
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", &tmp) };

    let app = app_with(state_with(test_config()));

    // Log in with the original password.
    let cookie = login(&app, PASSWORD).await.unwrap();

    // Change the password.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/password")
                .header(header::COOKIE, &cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "current": PASSWORD, "new": "a-fresh-passphrase" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // The new password now works; the old one does not.
    assert!(login(&app, "a-fresh-passphrase").await.is_some());
    assert!(login(&app, PASSWORD).await.is_none());

    let _ = std::fs::remove_dir_all(&tmp);
}
