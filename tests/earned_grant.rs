//! Earned bonus-time grants: `POST /api/extra-time` with a named `source`.
//!
//! The contract under test, from the parent's side: a robot pushing "practice done" grants
//! **once per source per day**, a lost response replayed via `Idempotency-Key` does not grant
//! twice, and the parent's own button keeps meaning exactly what it always meant — twice is
//! twice.
//!
//! **One test, its own binary**, like `rules_persist.rs` and for a sharpened version of its
//! reason: every *successful* grant persists the config, so every section here needs the
//! `NESTWATCH_DATA_DIR` override — and the override is process-wide. A first draft of this file
//! held four `#[tokio::test]`s; the harness ran them concurrently, the one that owned the
//! scratch directory finished first and deleted it under the others, and a run without the
//! override wrote a test config into the real `~/.config/nestwatch`. Sections in one test keep
//! the directory alive for exactly as long as anything can write to it.

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use nestwatch::config::data_paths;

mod common;
use common::{PASSWORD, ScratchDir, app_with, login, state_with, test_config};

/// POST `/api/extra-time` with `body`, returning (status, parsed body).
async fn grant(
    app: &axum::Router,
    cookie: &str,
    body: Value,
    idempotency_key: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/extra-time")
        .header(header::COOKIE, cookie)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(key) = idempotency_key {
        builder = builder.header("Idempotency-Key", key);
    }
    let res = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let parsed = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, parsed)
}

/// Install or reconfigure a provider via `POST /api/providers/{name}`.
async fn configure_provider(
    app: &axum::Router,
    cookie: &str,
    name: &str,
    enabled: bool,
    minutes: u32,
) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/providers/{name}"))
                .header(header::COOKIE, cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({ "enabled": enabled, "minutes": minutes }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

/// A fresh signed-in app over its own state, so one section's day latch cannot leak into the
/// next section's assertions.
///
/// Installs `studygo` worth 30 minutes and `chores` worth 10, because a provider grant now
/// requires an enabled provider — the reward is the parent's policy on this machine, not a
/// number the push chose.
async fn fresh_app() -> (
    axum::Router,
    String,
    std::sync::Arc<std::sync::RwLock<nestwatch::config::Config>>,
) {
    let state = state_with(test_config());
    let config = state.config.clone();
    let app = app_with(state);
    let cookie = login(&app, PASSWORD).await.unwrap();
    assert_eq!(
        configure_provider(&app, &cookie, "studygo", true, 30).await,
        StatusCode::OK
    );
    assert_eq!(
        configure_provider(&app, &cookie, "chores", true, 10).await,
        StatusCode::OK
    );
    (app, cookie, config)
}

#[tokio::test]
async fn earned_grants_latch_replay_and_validate() {
    let tmp = ScratchDir::new("earned");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };
    let today = nestwatch::config::today();

    // --- The parent can still grant twice; a robot cannot. -------------------------------
    {
        let (app, cookie, config) = fresh_app().await;

        // Parent grants, twice — both land, exactly as before this feature existed.
        let (status, body) = grant(&app, &cookie, json!({ "minutes": 10 }), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], json!(true));
        let (_, body) = grant(&app, &cookie, json!({ "minutes": 5 }), None).await;
        assert_eq!(body["ok"], json!(true));
        {
            let cfg = nestwatch::state::recover_read(&config);
            assert_eq!(cfg.extra.for_day(today), 15, "parent grants accumulate");
            assert!(cfg.earned.is_empty(), "the parent is never latched");
        }

        // A robot grants once...
        let (status, body) = grant(
            &app,
            &cookie,
            json!({ "minutes": 30, "source": "studygo" }),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["minutes"], json!(30));

        // ...and the same source the same day is told no, with nothing granted.
        let (status, body) = grant(
            &app,
            &cookie,
            json!({ "minutes": 30, "source": "studygo" }),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], json!(false));
        assert_eq!(body["reason"], json!("already_granted_today"));
        {
            let cfg = nestwatch::state::recover_read(&config);
            assert_eq!(
                cfg.extra.for_day(today),
                45,
                "the second robot grant added nothing"
            );
            assert_eq!(cfg.earned.get("studygo"), Some(&today));
        }

        // A *different* source is its own latch.
        let (_, body) = grant(
            &app,
            &cookie,
            json!({ "minutes": 10, "source": "chores" }),
            None,
        )
        .await;
        assert_eq!(body["ok"], json!(true));
        {
            let cfg = nestwatch::state::recover_read(&config);
            assert_eq!(cfg.extra.for_day(today), 55);
        }

        // The latch is persisted: a service restart cannot forget today's grant.
        let saved = std::fs::read_to_string(data_paths().config).unwrap();
        assert!(
            saved.contains("studygo"),
            "earned latch reaches disk: {saved}"
        );
    }

    // --- An idempotency key replays the original outcome without granting again. ---------
    {
        let (app, cookie, config) = fresh_app().await;

        let body = json!({ "minutes": 30, "source": "studygo" });
        let (_, first) = grant(&app, &cookie, body.clone(), Some("retry-abc")).await;
        assert_eq!(first["ok"], json!(true));

        // The retry a killed scheduler sends: same key, byte-identical answer, no second grant.
        let (_, second) = grant(&app, &cookie, body.clone(), Some("retry-abc")).await;
        assert_eq!(
            second, first,
            "a replay is the original response, not a re-run"
        );
        {
            let cfg = nestwatch::state::recover_read(&config);
            assert_eq!(
                cfg.extra.for_day(today),
                30,
                "the replayed request granted nothing"
            );
        }

        // A fresh key is a new request, and the day latch answers it.
        let (_, third) = grant(&app, &cookie, body, Some("retry-xyz")).await;
        assert_eq!(third["ok"], json!(false));
        assert_eq!(third["reason"], json!("already_granted_today"));
    }

    // --- Bad sources and bad keys are rejected before anything happens. ------------------
    {
        let (app, cookie, config) = fresh_app().await;

        for source in [
            "",
            "UPPER",
            "with space",
            "a".repeat(33).as_str(),
            "quote\"",
        ] {
            let (status, _) = grant(
                &app,
                &cookie,
                json!({ "minutes": 10, "source": source }),
                None,
            )
            .await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "source {source:?} must be rejected"
            );
        }

        let long_key = "k".repeat(129);
        let (status, _) = grant(
            &app,
            &cookie,
            json!({ "minutes": 10 }),
            Some(long_key.as_str()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "over-long idempotency key");

        let cfg = nestwatch::state::recover_read(&config);
        assert_eq!(
            cfg.extra.for_day(today),
            0,
            "nothing was granted on any rejected path"
        );
    }

    // --- The earned-source population is bounded. -----------------------------------------
    {
        let (app, cookie, _config) = fresh_app().await;

        for i in 0..16 {
            let name = format!("s{i}");
            assert_eq!(
                configure_provider(&app, &cookie, &name, true, 1).await,
                StatusCode::OK
            );
            let (status, body) =
                grant(&app, &cookie, json!({ "minutes": 1, "source": name }), None).await;
            assert_eq!(status, StatusCode::OK);
            assert_eq!(body["ok"], json!(true), "source s{i} within the cap grants");
        }
        assert_eq!(
            configure_provider(&app, &cookie, "s16", true, 1).await,
            StatusCode::OK
        );
        let (status, body) = grant(
            &app,
            &cookie,
            json!({ "minutes": 1, "source": "s16" }),
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "the seventeenth source of the day is refused; got {body}"
        );
    }

    // --- The registry governs whether a provider may grant, and for how much. ------------
    {
        let (app, cookie, config) = fresh_app().await; // studygo(30), chores(10) installed

        // An unknown provider is refused — nothing installed by that name.
        let (status, _) = grant(
            &app,
            &cookie,
            json!({ "minutes": 30, "source": "duolingo" }),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "no such integration");

        // Turn studygo off; its push is now refused even though it is installed.
        assert_eq!(
            configure_provider(&app, &cookie, "studygo", false, 30).await,
            StatusCode::OK
        );
        let (status, _) = grant(
            &app,
            &cookie,
            json!({ "minutes": 30, "source": "studygo" }),
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a disabled provider cannot grant"
        );
        assert_eq!(
            nestwatch::state::recover_read(&config).extra.for_day(today),
            0,
            "nothing granted while off"
        );

        // The reward is the provider's config, not the number the push sent.
        assert_eq!(
            configure_provider(&app, &cookie, "studygo", true, 45).await,
            StatusCode::OK
        );
        let (status, body) = grant(
            &app,
            &cookie,
            json!({ "minutes": 999, "source": "studygo" }),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["ok"], json!(true));
        assert_eq!(
            body["minutes"],
            json!(45),
            "the PC's policy wins over the push's claim"
        );
        assert_eq!(
            nestwatch::state::recover_read(&config).extra.for_day(today),
            45,
        );
    }

    // --- The registry lists what is installed, and rejects a bad name. -------------------
    {
        let (app, cookie, _config) = fresh_app().await;

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/providers")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let listed: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(listed["studygo"]["enabled"], json!(true));
        assert_eq!(listed["studygo"]["minutes"], json!(30));
        assert_eq!(listed["chores"]["minutes"], json!(10));

        // 'parent' is reserved — it is the human's own grant, not an integration.
        assert_eq!(
            configure_provider(&app, &cookie, "parent", true, 30).await,
            StatusCode::BAD_REQUEST,
        );
    }
}
