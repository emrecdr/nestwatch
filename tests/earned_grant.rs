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

use nestwatch::config::{MAX_PROVIDERS, data_paths};

mod common;
use common::{PASSWORD, ScratchDir, app_with, configure_provider, login, state_with, test_config};

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

/// Uninstall a provider via `POST /api/providers/{name}/delete`.
async fn delete_provider(app: &axum::Router, cookie: &str, name: &str) -> StatusCode {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/providers/{name}/delete"))
                .header(header::COOKIE, cookie)
                .body(Body::empty())
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
        // Carries weight beyond the latch this section is named for. The contract section at the
        // bottom pins the response *key sets*, which catch a dropped field but pass happily on a
        // flipped boolean — so the `ok` **values** are held only by bare assertions like this one,
        // here and in the idempotency section below. None of them announces that another
        // repository branches on the answer, which is why these two now say so.
        assert_eq!(body["ok"], json!(true), "a provider grant says so in `ok`");
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
        assert_eq!(
            body["ok"],
            json!(false),
            "and a refusal says so too — see above"
        );
        assert_eq!(body["reason"], json!("already_granted_today"));
        {
            let cfg = nestwatch::state::recover_read(&config);
            assert_eq!(
                cfg.extra.for_day(today),
                45,
                "the second robot grant added nothing"
            );
            assert_eq!(
                cfg.earned.get("studygo").map(|entry| entry.date),
                Some(today),
                "the source stays latched for the day"
            );
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

    // --- The earned-source population is bounded, and it can outgrow the registry. --------
    //
    // Sixteen latched sources against a twelve-slot registry, which is only reachable because
    // uninstalling a provider deliberately leaves `earned` alone. "Sources that granted today"
    // and "providers installed right now" are therefore different counts, and that is what keeps
    // MAX_EARNED_SOURCES a live bound instead of dead code sitting behind the smaller
    // MAX_PROVIDERS. Install, grant, uninstall, repeat.
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
            // Frees the registry slot; the day latch stays behind, which is the point.
            assert_eq!(delete_provider(&app, &cookie, &name).await, StatusCode::OK);
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
        // The same reservation on the way out, so the two handlers cannot disagree about what
        // a provider name is.
        assert_eq!(
            delete_provider(&app, &cookie, "parent").await,
            StatusCode::BAD_REQUEST,
        );
    }

    // --- The registry is bounded, and the bound does not freeze it. ----------------------
    {
        let (app, cookie, _config) = fresh_app().await; // studygo + chores hold 2 of the slots

        for i in 0..(MAX_PROVIDERS - 2) {
            assert_eq!(
                configure_provider(&app, &cookie, &format!("p{i}"), true, 1).await,
                StatusCode::OK,
                "install {i} is within the cap"
            );
        }
        assert_eq!(
            configure_provider(&app, &cookie, "one_too_many", true, 1).await,
            StatusCode::BAD_REQUEST,
            "a brand-new name past the cap is refused"
        );
        // The half that matters: an integration that already exists can still be reconfigured
        // at the cap. Without this a parent who filled the registry could not turn anything
        // off, and a bound meant to keep the file small would instead freeze it.
        assert_eq!(
            configure_provider(&app, &cookie, "studygo", false, 30).await,
            StatusCode::OK,
            "reconfiguring an installed provider is never capped"
        );
        // And removing one frees the slot, which is what stops the cap from being a trap.
        assert_eq!(
            delete_provider(&app, &cookie, "chores").await,
            StatusCode::OK
        );
        assert_eq!(
            configure_provider(&app, &cookie, "one_too_many", true, 1).await,
            StatusCode::OK,
            "a freed slot is usable"
        );
    }

    // --- Uninstalling stops the grant, and does NOT hand back the day. -------------------
    {
        let (app, cookie, config) = fresh_app().await;

        let (_, body) = grant(&app, &cookie, json!({ "source": "studygo" }), None).await;
        assert_eq!(body["ok"], json!(true));

        assert_eq!(
            delete_provider(&app, &cookie, "studygo").await,
            StatusCode::OK
        );
        let (status, _) = grant(&app, &cookie, json!({ "source": "studygo" }), None).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "an uninstalled provider is refused, not silently ignored"
        );

        // THE SECURITY PROPERTY. Reinstalling must not clear the latch, or delete-then-install
        // is a two-request bypass of the once-per-source-per-day rule — available to exactly the
        // caller who would want to push the second grant.
        assert_eq!(
            configure_provider(&app, &cookie, "studygo", true, 30).await,
            StatusCode::OK
        );
        let (status, body) = grant(&app, &cookie, json!({ "source": "studygo" }), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["ok"],
            json!(false),
            "reinstalling a provider must not clear its day latch"
        );
        assert_eq!(body["reason"], json!("already_granted_today"));
        assert_eq!(
            nestwatch::state::recover_read(&config).extra.for_day(today),
            30,
            "still exactly one grant's worth of minutes"
        );

        // Removing something that is not installed is not an error, matching routines.
        assert_eq!(
            delete_provider(&app, &cookie, "never_installed").await,
            StatusCode::OK
        );
    }

    // --- An idempotency key belongs to one source. ---------------------------------------
    {
        let (app, cookie, config) = fresh_app().await;

        // Two integrations that both key by the day. It is the obvious thing for a client to
        // choose, and while keys were stored bare the second push was handed the first's answer,
        // granted nothing, and reported success.
        let (_, first) = grant(
            &app,
            &cookie,
            json!({ "source": "studygo" }),
            Some("2026-09-02"),
        )
        .await;
        assert_eq!(first["ok"], json!(true));
        assert_eq!(first["minutes"], json!(30));

        let (status, second) = grant(
            &app,
            &cookie,
            json!({ "source": "chores" }),
            Some("2026-09-02"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            second["ok"],
            json!(true),
            "a key another source used must not replay onto this one"
        );
        assert_eq!(
            second["minutes"],
            json!(10),
            "and it earns its own provider's reward"
        );
        assert_eq!(
            nestwatch::state::recover_read(&config).extra.for_day(today),
            40,
            "both grants landed"
        );
    }

    // --- One key, one grant: reuse carrying different values is refused. -----------------
    {
        let (app, cookie, config) = fresh_app().await;

        let (_, first) = grant(&app, &cookie, json!({ "minutes": 10 }), Some("dup")).await;
        assert_eq!(first["ok"], json!(true));

        // Same key, same value: the honest retry a lost response produces. Still a replay.
        let (status, again) = grant(&app, &cookie, json!({ "minutes": 10 }), Some("dup")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(again, first, "an unchanged retry still replays");

        // Same key, different value: a client bug, not a retry. Replaying would answer this
        // request with the other one's outcome and report a 20-minute grant that never happened.
        let (status, _) = grant(&app, &cookie, json!({ "minutes": 20 }), Some("dup")).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a key reused across two different grants is refused, not replayed"
        );
        assert_eq!(
            nestwatch::state::recover_read(&config).extra.for_day(today),
            10,
            "only the first grant landed"
        );
    }

    // --- The response shape is a cross-repo contract, so pin it here. --------------------
    //
    // `O86`: Voortgang (in the `studygo` repository) parses `ok`, `reason` and `minutes` out of
    // this endpoint, and until now **nothing on this side noticed if they moved**. Renaming
    // `reason`, or dropping `minutes` from the success body, breaks that client with every test in
    // both repositories green — which is precisely the failure `tests/golden.rs` exists to prevent,
    // one repository over.
    //
    // Deliberately asserted here rather than as a file in `tests/golden/`. That directory is a
    // contract with `nestwatch-mobile` specifically: its `tool/check_golden.sh` walks
    // `tests/golden/*.json` and counts every file it does not itself carry as drift, so a fixture
    // for a *different* consumer would fail a repo that has no parser for it. Where the shared
    // fixtures should live is a real design question and it belongs to both repositories' owners;
    // this test does not answer it. It closes the hole that does not need it answered.
    //
    // The key set is asserted exactly, not field-by-field. A field-by-field check passes when a
    // field is *added*, and an added field is the change most likely to be made without thinking
    // about who else reads this — the same reason `golden()` compares whole documents.
    {
        let (app, cookie, _config) = fresh_app().await;

        let (_, granted) = grant(&app, &cookie, json!({ "minutes": 10 }), None).await;
        let mut keys: Vec<&str> = granted
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["curfew_note", "minutes", "ok"],
            "the granted response shape changed; `studygo` reads ok and minutes from it \
             (its side is pinned in 5518f97). Changing this is allowed — doing it without \
             telling that repository is not."
        );

        // The refused body is the other half of the contract and carries no `minutes` at all,
        // which is why that client reads it as nullable rather than defaulting it to zero.
        let (_, refused) = grant(&app, &cookie, json!({ "source": "studygo" }), None).await;
        assert_eq!(refused["ok"], json!(true), "first push of the day grants");
        let (_, refused) = grant(&app, &cookie, json!({ "source": "studygo" }), None).await;
        let mut keys: Vec<&str> = refused
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["ok", "reason"],
            "the refused response shape changed; Voortgang (in the `studygo` repository) tells \
             this body from a grant by reading `reason`. Changing this is allowed — doing it \
             without telling that repository is not."
        );
        assert_eq!(refused["reason"], json!("already_granted_today"));

        // **The `ok` values are deliberately not re-asserted here**, and the reason is worth
        // keeping. A first version of this block did assert them, on the argument that `ok` and
        // `reason` must never disagree — a client branching on `ok` would otherwise read every
        // latched day as a fresh grant, which is a mistake `studygo` really made until `1e4d1a5`.
        // The argument is sound and the assertions were still worthless: the latch section at the
        // top of this test already pins both values, it runs first, and `assert_eq!` panics, so
        // the copies here could never be reached by a failure. Proven rather than reasoned —
        // forcing the refused body to `ok: true` dies at line 163 with the copies deleted.
        //
        // The general form, since it cost a wrong claim to learn: a mutation that goes red proves
        // *the suite* catches the edit, not that the assertion you just wrote catches it. Read
        // which assertion fired, or delete yours and re-run.
    }

    // --- `minutes` is the parent's to give and the registry's to decide. -----------------
    {
        let (app, cookie, config) = fresh_app().await;

        // A push may omit the field entirely; the reward comes from the registry.
        let (status, body) = grant(&app, &cookie, json!({ "source": "studygo" }), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["minutes"],
            json!(30),
            "the registry's number, with nothing in the body to read"
        );

        // Zero is what the phone sends as an explicit non-guess, and it is still ignored —
        // `require_minutes` would reject it if a provider grant ever consulted it.
        let (status, body) = grant(
            &app,
            &cookie,
            json!({ "minutes": 0, "source": "chores" }),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["minutes"], json!(10));

        // The parent has no registry entry to fall back on, so an omitted `minutes` is refused
        // rather than silently read as zero.
        let (status, body) = grant(&app, &cookie, json!({}), None).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a parent grant must say how many minutes"
        );
        // Refused as a *missing field*, not as a number out of range. Both are a 400 — dropping
        // the field and defaulting it to zero would land on `require_minutes` and produce one
        // too — so the status alone cannot tell them apart, and the message is the whole reason
        // `minutes` became an `Option`: a client that omitted the field should be told that,
        // rather than told its zero was too small.
        assert!(
            body["error"]
                .as_str()
                .unwrap_or_default()
                .contains("required"),
            "the refusal should name the missing field: {body}"
        );
        assert_eq!(
            nestwatch::state::recover_read(&config).extra.for_day(today),
            40,
            "the two pushes landed and the malformed parent grant did not"
        );
    }

    // --- A daily ceiling lets one source push more than once, and still bounds it. --------
    //
    // The opt-in half of the same rule, and every section above is the proof of its default:
    // without `daily_cap_mins` a source grants once a day, exactly as it always has. With one,
    // the same source may push until the ceiling is reached — what a signal arriving in pieces
    // through an afternoon needs, and what a single latch cannot express.
    //
    // The property that decides whether this is safe is the *last* push, not the first. A latch
    // bounded a bad client to one reward; a ceiling has to bound it to a number the parent chose,
    // including when a push straddles the line.
    {
        let (app, cookie, config) = fresh_app().await;

        // 20 a push, 50 a day. Chosen so the ceiling lands *mid-push*: the third grant must be
        // worth 10 rather than 20. A design that only refused pushes starting past the ceiling
        // would pay out 60 here and still look correct against round numbers.
        assert_eq!(
            common::post_json(
                &app,
                "/api/providers/reading",
                Some(&cookie),
                json!({ "enabled": true, "minutes": 20, "daily_cap_mins": 50 }),
            )
            .await
            .status(),
            StatusCode::OK,
            "a provider may be installed with a ceiling"
        );

        for (push, expected) in [(1, 20), (2, 20), (3, 10)] {
            let (_, body) = grant(&app, &cookie, json!({ "source": "reading" }), None).await;
            assert_eq!(body["ok"], json!(true), "push {push} should still grant");
            assert_eq!(
                body["minutes"],
                json!(expected),
                "push {push} is worth the remainder once the ceiling is within reach, and the \
                 body has to say the number that actually landed"
            );
        }

        let (_, body) = grant(&app, &cookie, json!({ "source": "reading" }), None).await;
        assert_eq!(
            body["ok"],
            json!(false),
            "the ceiling stops the fourth push"
        );
        assert_eq!(
            body["reason"],
            json!("daily_cap_reached"),
            "and distinguishes a spent allowance from a day already claimed"
        );
        {
            let cfg = nestwatch::state::recover_read(&config);
            assert_eq!(
                cfg.extra.for_day(today),
                50,
                "exactly the ceiling landed — not a fourth reward on top of it"
            );
            assert_eq!(
                cfg.earned.get("reading").and_then(|entry| entry.minutes),
                Some(50),
                "the entry measures what it granted, which is what the ceiling is judged against"
            );
        }

        // **A ceiling survives the dashboard's own upsert, which has never heard of it.**
        // `assets/app.js::loadProviders` rebuilds each row as {name, enabled, minutes} and posts
        // that row back on any change, so an entry-replacing upsert would erase the ceiling the
        // first time a parent used the on/off toggle — a setting destroyed by a client that does
        // not know it exists. This is the same rule `a_newer_configs_unknown_settings_survive_a_
        // load_and_save` keeps for the config file, applied to the writer instead.
        assert_eq!(
            common::post_json(
                &app,
                "/api/providers/reading",
                Some(&cookie),
                json!({ "enabled": true, "minutes": 20 }),
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            nestwatch::state::recover_read(&config).providers["reading"].daily_cap_mins,
            Some(50),
            "a body that never mentions the ceiling must not remove it"
        );

        // An explicit null is how it comes off, so a ceiling stays removable without a second
        // route — the distinction `absent_or_null` exists to carry, exercised end to end.
        assert_eq!(
            common::post_json(
                &app,
                "/api/providers/reading",
                Some(&cookie),
                json!({ "enabled": true, "minutes": 20, "daily_cap_mins": null }),
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            nestwatch::state::recover_read(&config).providers["reading"].daily_cap_mins,
            None,
            "an explicit null clears the ceiling"
        );

        // **The provider next door is untouched.** `studygo` has no ceiling, so it keeps the
        // original rule and the original refusal string, in the same process and the same day as
        // a provider that does. This is the assertion that says adding the field changed nothing
        // for anyone who has not asked for it.
        let (_, first) = grant(&app, &cookie, json!({ "source": "studygo" }), None).await;
        assert_eq!(first["ok"], json!(true));
        let (_, second) = grant(&app, &cookie, json!({ "source": "studygo" }), None).await;
        assert_eq!(second["ok"], json!(false));
        assert_eq!(
            second["reason"],
            json!("already_granted_today"),
            "a provider with no ceiling keeps the original refusal, word for word"
        );
        {
            let cfg = nestwatch::state::recover_read(&config);
            assert!(
                cfg.earned["studygo"].minutes.is_none(),
                "and its entry stays untracked, so its config keeps the shape an older build wrote"
            );
        }
    }

    // --- Facts decide the reward, and the ladder lives here rather than in the client. ----
    //
    // The other half of moving judgement onto this machine. `83f0ce3` took the *reward* off the
    // push; this takes the *threshold* off it too, so a parent can move the bar without anyone
    // shipping a new client. A push now says what the child did, and this side says what it is
    // worth.
    {
        let (app, cookie, config) = fresh_app().await;

        // The two-step ladder a household actually asks for, with the ceiling set to the top
        // rung. That pairing is the point: the day then totals exactly the best tier reached,
        // however many pushes it took to get there.
        assert_eq!(
            common::post_json(
                &app,
                "/api/providers/reading",
                Some(&cookie),
                json!({
                    "enabled": true,
                    "minutes": 30,
                    "daily_cap_mins": 30,
                    "tiers": [
                        { "questions": 10, "minutes_practised": 20, "reward_mins": 16 },
                        { "questions": 15, "minutes_practised": 30, "reward_mins": 30 }
                    ]
                }),
            )
            .await
            .status(),
            StatusCode::OK
        );

        // Below every rung: refused, and deliberately not an error. A client that reports what it
        // sees and lets this side judge is the whole design, so "not yet" has to be an ordinary
        // 200 answer — otherwise that client is back to pre-judging.
        let (status, body) = grant(
            &app,
            &cookie,
            json!({ "source": "reading", "progress": { "questions": 9, "minutes": 19 } }),
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "failing to clear the bar is an answer, not a failure"
        );
        assert_eq!(body["ok"], json!(false));
        assert_eq!(body["reason"], json!("below_threshold"));

        // The lower rung, reached on questions alone — the child who works quickly.
        let (_, body) = grant(
            &app,
            &cookie,
            json!({ "source": "reading", "progress": { "questions": 12, "minutes": 0 } }),
            None,
        )
        .await;
        assert_eq!(body["ok"], json!(true));
        assert_eq!(body["minutes"], json!(16), "the lower rung is worth 16");

        // The upper rung, reached later on minutes alone — the child who works slowly. The
        // ceiling turns this into a top-up rather than a second full payment.
        let (_, body) = grant(
            &app,
            &cookie,
            json!({ "source": "reading", "progress": { "questions": 0, "minutes": 30 } }),
            None,
        )
        .await;
        assert_eq!(body["ok"], json!(true));
        assert_eq!(
            body["minutes"],
            json!(14),
            "reaching the upper rung after the lower one pays the difference"
        );
        assert_eq!(
            nestwatch::state::recover_read(&config).extra.for_day(today),
            30,
            "so the day totals the best tier reached, not the sum of the rungs"
        );

        // **A push reporting nothing is worth the single reward, tiers or no tiers.** The
        // compatibility half: today's client sends no `progress`, and it has to keep working
        // against a provider somebody later gave a ladder to.
        assert_eq!(
            common::post_json(
                &app,
                "/api/providers/chores",
                Some(&cookie),
                json!({
                    "enabled": true,
                    "minutes": 10,
                    "tiers": [{ "questions": 99, "minutes_practised": 99, "reward_mins": 25 }]
                }),
            )
            .await
            .status(),
            StatusCode::OK
        );
        let (_, body) = grant(&app, &cookie, json!({ "source": "chores" }), None).await;
        assert_eq!(body["ok"], json!(true));
        assert_eq!(
            body["minutes"],
            json!(10),
            "no progress reported means the single reward, not a tier it never claimed"
        );

        // Tiers survive the dashboard's upsert, for the reason the ceiling does.
        assert_eq!(
            common::post_json(
                &app,
                "/api/providers/reading",
                Some(&cookie),
                json!({ "enabled": true, "minutes": 30 }),
            )
            .await
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            nestwatch::state::recover_read(&config).providers["reading"]
                .tiers
                .len(),
            2,
            "a body that never mentions tiers must not remove them"
        );

        // A tier asking for nothing could never be met, so it is refused rather than stored as
        // configuration that reads like a rule and behaves like a blank.
        assert_eq!(
            common::post_json(
                &app,
                "/api/providers/reading",
                Some(&cookie),
                json!({
                    "enabled": true,
                    "minutes": 30,
                    "tiers": [{ "questions": 0, "minutes_practised": 0, "reward_mins": 5 }]
                }),
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST,
            "a tier stating no condition is refused at the door"
        );
        // **A tier may state one condition.** Mutation testing found this gap: the check above is
        // `questions == 0 && minutes_practised == 0`, and every earlier case here set both to zero
        // — which `&&` and `||` reject alike. So nothing distinguished "asks for neither" from
        // "asks for one", and a one-sided tier being refused would have passed the whole suite.
        // A ladder of "ten questions, never mind how long it took" is an ordinary thing to want.
        assert_eq!(
            common::post_json(
                &app,
                "/api/providers/reading",
                Some(&cookie),
                json!({
                    "enabled": true,
                    "minutes": 30,
                    "tiers": [{ "questions": 10, "minutes_practised": 0, "reward_mins": 16 }]
                }),
            )
            .await
            .status(),
            StatusCode::OK,
            "a tier asking only about questions is a rule, not a blank"
        );

        // The tier cap, from both sides — also a mutation-testing find: nothing exercised it, so
        // `>` could have been `==` or `>=` and the suite would not have moved. Both directions are
        // needed. Exactly MAX_TIERS must be accepted (which `>=` would refuse) and one more must
        // be refused (which `==` would let through at any other count).
        let rung = |n: u32| json!({ "questions": n, "minutes_practised": 0, "reward_mins": 5 });
        assert_eq!(
            common::post_json(
                &app,
                "/api/providers/reading",
                Some(&cookie),
                json!({
                    "enabled": true,
                    "minutes": 30,
                    "tiers": [rung(1), rung(2), rung(3), rung(4)]
                }),
            )
            .await
            .status(),
            StatusCode::OK,
            "exactly MAX_TIERS rungs is allowed"
        );
        assert_eq!(
            common::post_json(
                &app,
                "/api/providers/reading",
                Some(&cookie),
                json!({
                    "enabled": true,
                    "minutes": 30,
                    "tiers": [rung(1), rung(2), rung(3), rung(4), rung(5)]
                }),
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST,
            "one past MAX_TIERS is refused"
        );
        // The two threshold bounds, from both sides each. They exist because a threshold nobody
        // could reach in a day is a rule that silently never fires while reading exactly like one
        // that does — the 0/0 case arriving from the other end. `web.rs`'s minutes-limit guard is
        // what forced them: it refuses to let a number box on the dashboard restate a limit no
        // endpoint enforces, and its panic says so outright.
        let tier_at = |q: u32, m: u32| {
            json!({
                "enabled": true,
                "minutes": 30,
                "tiers": [{ "questions": q, "minutes_practised": m, "reward_mins": 5 }]
            })
        };
        for (body, want, what) in [
            (tier_at(1000, 0), StatusCode::OK, "questions at the limit"),
            (
                tier_at(1001, 0),
                StatusCode::BAD_REQUEST,
                "questions one past",
            ),
            (
                tier_at(0, 1440),
                StatusCode::OK,
                "practised minutes at the limit",
            ),
            (
                tier_at(0, 1441),
                StatusCode::BAD_REQUEST,
                "practised minutes one past",
            ),
        ] {
            assert_eq!(
                common::post_json(&app, "/api/providers/reading", Some(&cookie), body)
                    .await
                    .status(),
                want,
                "{what}"
            );
        }
    }
    // --- A refusal writes nothing and wakes nobody (`O84`) -------------------------------------
    //
    // `try_update_config` saved and woke on every `Ok`, so a push that granted nothing still
    // serialised the whole config, rewrote `config.json` and pulled both enforcers out of their
    // cadence. Harmless per refusal and no longer rare: under a ceiling every push after the
    // allowance is spent is a refusal, and the probe scheduler polls on a timer rather than waiting
    // for a person to press something.
    //
    // **Observed by tampering with the file rather than by timing it.** A marker appended to
    // `config.json` by hand survives only if nothing rewrote the file from memory, which is exactly
    // the property under test and is deterministic — an mtime comparison would not be.
    {
        let state = state_with(test_config());
        let wake = state.enforcement_wake.clone();
        let app = app_with(state);
        let cookie = login(&app, PASSWORD).await.unwrap();
        assert_eq!(
            configure_provider(&app, &cookie, "studygo", true, 30).await,
            StatusCode::OK
        );
        let (status, _) = grant(&app, &cookie, json!({ "source": "studygo" }), None).await;
        assert_eq!(status, StatusCode::OK, "the first push grants");

        let path = data_paths().config;
        let mut tampered = std::fs::read_to_string(&path).unwrap();
        tampered.push('\n');
        let woken_before = *wake.borrow();

        std::fs::write(&path, &tampered).unwrap();
        let (status, body) = grant(&app, &cookie, json!({ "source": "studygo" }), None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["ok"],
            json!(false),
            "the second push is refused: {body}"
        );

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            tampered,
            "a refused grant rewrote config.json, which it has nothing to write"
        );
        assert_eq!(
            *wake.borrow(),
            woken_before,
            "a refused grant woke the enforcers, which have nothing to re-read"
        );
    }
}
