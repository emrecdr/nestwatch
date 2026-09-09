//! The credential half of a probe — `POST /api/providers/{name}/secret` — and the `probe` field
//! of `POST /api/providers/{name}`, plus what `GET /api/providers` says about both.
//!
//! The secret is the phone's StudyGo session, forwarded so the probe this machine runs can ask
//! on the child's behalf. Three things have to be true of it and are pinned here: it lands in the
//! data dir and never in the registry or `config.json` (both of which leave the machine through
//! `/api/policy` and `/api/export`); only the parent and the integration paired *as that name*
//! may deposit it; and uninstalling the provider forgets it, the way uninstalling already revokes
//! the pairing.
//!
//! **One test, its own binary**, for the reason `earned_grant.rs` gives: every section persists,
//! so this needs the process-wide `NESTWATCH_DATA_DIR` override.

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::{Value, json};

use nestwatch::config::data_paths;
use nestwatch::control::FakeControl;
use nestwatch::pairing::Scope;
use nestwatch::probe;

mod common;
use common::{
    PASSWORD, ScratchDir, app_with, configure_provider, login, pair_with, send_json, state_with,
    test_config,
};

fn stored(name: &str) -> Option<Vec<u8>> {
    probe::read_secret(&data_paths().dir, name).unwrap()
}

async fn deposit(app: &axum::Router, cookie: &str, name: &str, body: Value) -> (StatusCode, Value) {
    send_json(
        app,
        cookie,
        "POST",
        &format!("/api/providers/{name}/secret"),
        body,
    )
    .await
}

async fn listed(app: &axum::Router, cookie: &str) -> Value {
    send_json(app, cookie, "GET", "/api/providers", json!({}))
        .await
        .1
}

#[tokio::test]
async fn a_secret_is_deposited_kept_privately_and_forgotten_with_its_provider() {
    let tmp = ScratchDir::new("providersecret");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };

    let fake = Arc::new(FakeControl::new());
    let mut state = state_with(test_config());
    state.control = fake.clone();
    let app = app_with(state.clone());
    let parent = login(&app, PASSWORD).await.unwrap();

    // --- Depositing needs an installed provider --------------------------------------------
    let (status, body) = deposit(&app, &parent, "studygo", json!({ "secret": "tok" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(stored("studygo"), None);
    assert_eq!(
        configure_provider(&app, &parent, "studygo", true, 30).await,
        StatusCode::OK
    );

    // --- The parent may deposit; the bytes land in the data dir and nowhere else ----------
    let (status, body) = deposit(
        &app,
        &parent,
        "studygo",
        json!({ "secret": "session-token" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "ok": true }));
    assert_eq!(stored("studygo"), Some(b"session-token".to_vec()));
    let list = listed(&app, &parent).await;
    assert!(
        !list.to_string().contains("session-token"),
        "the registry never carries the secret: {list}"
    );
    assert!(
        list["studygo"]["secret_at"].is_string(),
        "but it says when one was deposited: {list}"
    );
    let disk = std::fs::read_to_string(data_paths().config).unwrap();
    assert!(
        !disk.contains("session-token"),
        "config.json leaves the machine through /api/policy and must not carry it"
    );

    // --- The integration paired as this provider may deposit its own -----------------------
    let phone = pair_with(
        &app,
        Scope::Integration {
            source: "studygo".into(),
        },
        None,
    )
    .await
    .expect("an integration pairing must produce a session");
    let (status, body) = deposit(&app, &phone, "studygo", json!({ "secret": "newer-token" })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(stored("studygo"), Some(b"newer-token".to_vec()));

    // --- Bounds: too big, empty, and absent are all refused and leave the earlier one -------
    // Both sides of the limit, because a one-sided check is exactly what mutation testing found
    // here: `>` and `>=` both refuse MAX+1, and only a deposit of exactly MAX tells them apart.
    let big = "x".repeat(probe::MAX_SECRET_BYTES + 1);
    let (status, _) = deposit(&app, &parent, "studygo", json!({ "secret": big })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let exact = "y".repeat(probe::MAX_SECRET_BYTES);
    let (status, body) = deposit(&app, &parent, "studygo", json!({ "secret": exact })).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "exactly the limit is allowed: {body}"
    );
    assert_eq!(
        stored("studygo").map(|s| s.len()),
        Some(probe::MAX_SECRET_BYTES)
    );
    let (status, _) = deposit(&app, &parent, "studygo", json!({ "secret": "newer-token" })).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = deposit(&app, &parent, "studygo", json!({ "secret": "" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = deposit(&app, &parent, "studygo", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(stored("studygo"), Some(b"newer-token".to_vec()));

    // --- `null` forgets it ------------------------------------------------------------------
    let (status, _) = deposit(&app, &parent, "studygo", json!({ "secret": null })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stored("studygo"), None);
    assert!(
        listed(&app, &parent).await["studygo"]
            .get("secret_at")
            .is_none()
    );

    // --- Uninstalling forgets it too --------------------------------------------------------
    let (status, _) = deposit(&app, &parent, "studygo", json!({ "secret": "tok" })).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send_json(
        &app,
        &parent,
        "POST",
        "/api/providers/studygo/delete",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        stored("studygo"),
        None,
        "an uninstalled provider keeps no credential"
    );

    // --- The probe field: set, kept across an unrelated upsert, refused when invalid, cleared -
    let set = |probe: Value| json!({ "enabled": true, "minutes": 30, "probe": probe });
    let (status, body) = send_json(
        &app,
        &parent,
        "POST",
        "/api/providers/studygo",
        set(json!({ "exe": "studygo-probe.exe", "every_mins": 15 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let expected = json!({ "exe": "studygo-probe.exe", "every_mins": 15 });
    assert_eq!(listed(&app, &parent).await["studygo"]["probe"], expected);
    assert_eq!(
        configure_provider(&app, &parent, "studygo", false, 30).await,
        StatusCode::OK
    );
    assert_eq!(
        listed(&app, &parent).await["studygo"]["probe"],
        expected,
        "a client that has never heard of the field must not erase it"
    );
    for bad in [
        json!({ "exe": "../studygo-probe.exe", "every_mins": 15 }),
        json!({ "exe": "C:\\\\probe.exe", "every_mins": 15 }),
        json!({ "exe": "studygo-probe.exe", "every_mins": 4 }),
        json!({ "exe": "studygo-probe.exe", "every_mins": 241 }),
        json!({ "exe": "", "every_mins": 15 }),
    ] {
        let (status, _) = send_json(
            &app,
            &parent,
            "POST",
            "/api/providers/studygo",
            set(bad.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad} must be refused");
    }
    assert_eq!(listed(&app, &parent).await["studygo"]["probe"], expected);
    let (status, _) = send_json(
        &app,
        &parent,
        "POST",
        "/api/providers/studygo",
        set(Value::Null),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        listed(&app, &parent).await["studygo"]
            .get("probe")
            .is_none(),
        "null takes the probe off"
    );

    // --- A provider that opted into nothing is listed exactly as it always was --------------
    assert_eq!(
        configure_provider(&app, &parent, "chores", true, 20).await,
        StatusCode::OK
    );
    assert_eq!(
        listed(&app, &parent).await["chores"],
        json!({ "enabled": true, "minutes": 20 }),
        "no probe, no secret, no status: nothing new appears until it is asked for"
    );

    // --- A probe that ran leaves its last outcome in the list ------------------------------
    let (status, _) = send_json(
        &app,
        &parent,
        "POST",
        "/api/providers/studygo",
        set(json!({ "exe": "studygo-probe.exe", "every_mins": 15 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    fake.script_probe(Ok(br#"{"questions":12,"minutes":5}"#.to_vec()));
    let now = chrono::DateTime::parse_from_rfc3339("2026-09-08T16:00:00+02:00").unwrap();
    probe::run_once(&state, now).await;
    let status = listed(&app, &parent).await["studygo"]["probe_status"].clone();
    assert_eq!(status["at"], "2026-09-08T16:00:00+02:00", "{status}");
    assert_eq!(status["questions"], 12);
    assert_eq!(status["minutes"], 5);
    assert_eq!(status["granted"], 30, "no ladder, so the single reward");
    assert!(
        listed(&app, &parent).await["chores"]
            .get("probe_status")
            .is_none(),
        "a provider whose probe never ran says nothing about one"
    );
}
