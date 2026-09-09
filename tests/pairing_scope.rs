//! What a pairing credential is allowed to do — `O89`, enforced rather than documented.
//!
//! The finding this closes: `auth::pair` used to perform the same two steps as `auth::login`,
//! and `require_auth` read one boolean, so a paired device held the parent's whole capability
//! table. A phone app that keeps the cookie could therefore grant as `source=parent`, which is
//! routed *around* the provider registry, the once-per-day latch and the daily ceiling. Measured
//! before the fix: five requests, 1200 minutes, against a configured 30.
//!
//! **Its own binary, like `earned_grant.rs`, for the same reason** — the sections below persist
//! config, so they need the process-wide `NESTWATCH_DATA_DIR` override, and sections in one test
//! keep the scratch directory alive for exactly as long as anything can write to it.

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

use nestwatch::pairing::Scope;

mod common;
use common::{
    PASSWORD, ScratchDir, app_with, configure_provider, login, pair_with, send_json, state_with,
    test_config,
};

async fn send(
    app: &axum::Router,
    cookie: &str,
    method: &str,
    uri: &str,
    body: Value,
) -> StatusCode {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::COOKIE, cookie)
        .header(header::CONTENT_TYPE, "application/json");
    let req = if method == "GET" {
        req.body(Body::empty()).unwrap()
    } else {
        req.body(Body::from(body.to_string())).unwrap()
    };
    app.clone().oneshot(req).await.unwrap().status()
}

#[tokio::test]
async fn a_pairing_can_only_do_what_it_was_minted_for() {
    let tmp = ScratchDir::new("pairscope");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };

    // --- An integration pairing reaches its three routes and nothing else. --------------
    {
        let state = state_with(test_config());
        let config = state.config.clone();
        let app = app_with(state);

        // Install the integration the way a parent does, from a full session.
        let parent = login(&app, PASSWORD).await.unwrap();
        assert_eq!(
            send(
                &app,
                &parent,
                "POST",
                "/api/providers/studygo",
                json!({ "enabled": true, "minutes": 30 })
            )
            .await,
            StatusCode::OK
        );

        let phone = pair_with(
            &app,
            Scope::Integration {
                source: "studygo".into(),
            },
            None,
        )
        .await
        .expect("an integration pairing must still produce a session");

        // The two routes it exists for.
        assert_eq!(
            send(&app, &phone, "POST", "/api/extra-time", json!({})).await,
            StatusCode::OK,
            "an integration must be able to push a grant"
        );
        assert_eq!(
            send(&app, &phone, "GET", "/api/usage/today", json!({})).await,
            StatusCode::OK,
            "and to read the grant back — this is `O85`'s mitigation, and the route an \
             allowlist written from 'the phone pushes grants' silently omits"
        );
        assert_eq!(
            send(
                &app,
                &phone,
                "POST",
                "/api/providers/studygo/secret",
                json!({ "secret": "session-token" })
            )
            .await,
            StatusCode::OK,
            "and to deposit the session it holds, for the probe this machine runs on its behalf"
        );

        // Everything else. These are the capabilities `docs/SECURITY.md`'s blast-radius table
        // lists, and before this change the same cookie reached every one of them.
        for (method, uri, body) in [
            ("POST", "/api/shutdown", json!({})),
            ("POST", "/api/lock", json!({})),
            ("POST", "/api/re-anchor", json!({})),
            ("GET", "/api/audit", json!({})),
            ("GET", "/api/export", json!({})),
            ("GET", "/api/providers", json!({})),
            (
                "POST",
                "/api/providers/studygo",
                json!({ "enabled": true, "minutes": 240 }),
            ),
            ("POST", "/api/providers/studygo/delete", json!({})),
            (
                "POST",
                "/api/providers/chores/secret",
                json!({ "secret": "not mine to set" }),
            ),
            ("POST", "/api/curfew/extend", json!({ "minutes": 60 })),
            ("GET", "/api/screenshot", json!({})),
            (
                "POST",
                "/api/password",
                json!({ "current": "x", "new": "yyyyyyyy" }),
            ),
        ] {
            assert_eq!(
                send(&app, &phone, method, uri, body).await,
                StatusCode::FORBIDDEN,
                "an integration pairing must not reach {method} {uri}"
            );
        }

        // --- The heart of `O89`: it cannot grant as somebody else. ---------------------
        // `source: parent` is the bypass — that name routes around the registry, the day latch
        // and the daily ceiling. The scope overrides the body, so this grants as `studygo` at
        // the configured 30, and the second is refused by the latch it tried to skip.
        let before = nestwatch::state::recover_read(&config)
            .extra
            .for_day(nestwatch::config::today());
        assert_eq!(
            before, 30,
            "the first push above granted the configured minutes"
        );
        for _ in 0..5 {
            send(
                &app,
                &phone,
                "POST",
                "/api/extra-time",
                json!({ "source": "parent", "minutes": 240 }),
            )
            .await;
        }
        let after = nestwatch::state::recover_read(&config)
            .extra
            .for_day(nestwatch::config::today());
        assert_eq!(
            after,
            30,
            "five `source=parent` pushes from an integration granted {} extra minutes; before \
             this fix they granted 1200",
            after - before
        );
    }

    // --- An integration may call the read; that is not the same as knowing all of it. -----
    //
    // `F2`. The allowlist above is route-scoped, and the justification for putting
    // `GET /api/usage/today` on it was one sentence: an integration pushes a grant and reads it
    // back, because it refuses to tell a parent a number the PC does not show (`O85`). The route
    // it was handed to do that returns the dashboard's whole usage picture — per-app minutes,
    // and up to `foreground::MAX_PAGES` window titles for the day.
    //
    // The consumer's own contract test is called "one field out of fourteen", and its
    // `dependedOn` set is derived from its source rather than hand-kept, so it stays honest:
    // `extra_mins` is the only field it reads off this route. Seventeen were being sent — counted
    // from the failure this test produces when the filter is removed, not from that fixture's
    // name, which was captured three fields ago.
    {
        let state = state_with(test_config());
        let app = app_with(state);
        let parent = login(&app, PASSWORD).await.unwrap();
        assert_eq!(
            configure_provider(&app, &parent, "studygo", true, 30).await,
            StatusCode::OK
        );
        let phone = pair_with(
            &app,
            Scope::Integration {
                source: "studygo".into(),
            },
            None,
        )
        .await
        .unwrap();

        let (status, narrow) = send_json(&app, &phone, "GET", "/api/usage/today", json!({})).await;
        assert_eq!(status, StatusCode::OK, "the read-back must still work");
        assert!(
            narrow.get("extra_mins").is_some(),
            "the one field the read-back exists for must survive: {narrow}"
        );
        // Named individually rather than by counting keys: a count passes whatever the fields
        // are, and the point is *which* facts leave this process.
        for leaked in [
            "pages",
            "per_app",
            "groups",
            "focused",
            "used_mins",
            "remaining_mins",
        ] {
            assert!(
                narrow.get(leaked).is_none(),
                "an integration reads back its own grant; it has no claim on `{leaked}`: {narrow}"
            );
        }

        // The other half, and the reason this is a filter rather than a smaller handler: the
        // same route is the browser dashboard's and the Android client's, both of which are
        // `Scope::Dashboard` and read the full payload. Narrowing for one caller must not
        // narrow it for them.
        let (status, full) = send_json(&app, &parent, "GET", "/api/usage/today", json!({})).await;
        assert_eq!(status, StatusCode::OK);
        for kept in ["extra_mins", "pages", "per_app", "used_mins"] {
            assert!(
                full.get(kept).is_some(),
                "a dashboard session still reads the whole day, `{kept}` included: {full}"
            );
        }
    }

    // --- A dashboard pairing is unchanged, because a person and the Android app need it. ---
    {
        let state = state_with(test_config());
        let app = app_with(state);
        let browser = pair_with(&app, Scope::Dashboard, None)
            .await
            .expect("a dashboard pairing must produce a session");

        for (method, uri) in [
            ("GET", "/api/audit"),
            ("GET", "/api/providers"),
            ("GET", "/api/usage/today"),
            ("GET", "/api/events"),
        ] {
            assert_eq!(
                send(&app, &browser, method, uri, json!({})).await,
                StatusCode::OK,
                "a dashboard pairing must still reach {method} {uri} — narrowing this would \
                 break `nestwatch-mobile`, which pairs exactly the same way and is a full \
                 parent dashboard"
            );
        }
    }

    // --- A session authenticated before scopes existed is refused, not promoted. ---------
    {
        let state = state_with(test_config());
        let app = app_with(state);
        let cookie = login(&app, PASSWORD).await.unwrap();
        assert_eq!(
            send(&app, &cookie, "GET", "/api/providers", json!({})).await,
            StatusCode::OK,
            "a password login is a full session"
        );
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/providers")
                    .header(header::COOKIE, "id=nonsense-not-a-real-session")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let _ = to_bytes(res.into_body(), usize::MAX).await;
    }

    // --- `GET /session` says what the caller is holding, so an app can check. -------------
    //
    // The gap this closes is not enforcement — that is settled above — it is that the two QRs
    // are byte-identical in form, so a parent who mints a dashboard link and scans it with an
    // integration app hands it the parent's whole authority, and the app cannot tell. Reading
    // this field is how it tells.
    {
        let state = state_with(test_config());
        let app = app_with(state);

        let read_scope = |cookie: Option<String>| {
            let app = app.clone();
            async move {
                let mut req = Request::builder().uri("/session");
                if let Some(c) = cookie {
                    req = req.header(header::COOKIE, c);
                }
                let res = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
                let bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
                serde_json::from_slice::<Value>(&bytes).unwrap()
            }
        };

        // No session at all: nothing to report, and nothing learned.
        let anon = read_scope(None).await;
        assert_eq!(anon["authenticated"], json!(false));
        assert_eq!(anon["scope"], Value::Null);
        // `.get`, not `["provider"]`: indexing a `Value` yields `Null` for a missing key, so the
        // convenient assertion cannot tell "absent" from "null" — which is the whole distinction.
        assert!(
            anon.get("provider").is_none(),
            "a caller bound to no provider is not told about one"
        );

        // A dashboard pairing says so — this is the value an integration app must refuse.
        let browser = pair_with(&app, Scope::Dashboard, None).await.unwrap();
        let seen = read_scope(Some(browser)).await;
        assert_eq!(seen["authenticated"], json!(true));
        assert_eq!(
            seen["scope"],
            json!({ "kind": "dashboard" }),
            "an app that asked for an integration link and reads this must be able to tell it \
             was handed the parent's authority instead"
        );
        assert!(
            seen.get("provider").is_none(),
            "a dashboard session is not bound to a provider, and its `/session` answer is a \
             golden file the Android client parses — this field must not appear in it"
        );

        // An integration pairing names its source. The **name** matters as much as the kind: a
        // link minted for a different integration would otherwise push happily under that other
        // name, with no error anywhere.
        let phone = pair_with(
            &app,
            Scope::Integration {
                source: "studygo".into(),
            },
            None,
        )
        .await
        .unwrap();
        let phone_cookie = phone.clone();
        let seen = read_scope(Some(phone)).await;
        assert_eq!(
            seen["scope"],
            json!({ "kind": "integration", "source": "studygo" }),
            "the source name has to travel, or a client cannot tell which integration it is"
        );
        // `F3`. Nothing has installed `studygo` in this section, and the honest answer to "what
        // is this credential bound to" is then *nothing* — which is a different sentence from
        // "switched off" and leads a parent somewhere different. Before this field the two were
        // reachable only by pushing a grant and reading the refusal prose, which the consumer
        // deliberately does not parse: the remedy is the same for both, and prose gets reworded
        // and translated.
        assert_eq!(
            seen["provider"],
            Value::Null,
            "paired to an integration this PC has never installed"
        );
        assert!(
            seen.as_object().is_some_and(|o| o.contains_key("provider")),
            "and *present*: absent would mean a build without the field, which is a different \
             thing for a client to do about"
        );

        // Installed and switched on: the entry itself, including the reward. `minutes` lives
        // only in this registry, and the bar the child clears lives only in the provider's own
        // app, so until this field neither side could state the whole rule.
        let parent = login(&app, PASSWORD).await.unwrap();
        assert_eq!(
            configure_provider(&app, &parent, "studygo", true, 25).await,
            StatusCode::OK
        );
        let seen = read_scope(Some(phone_cookie)).await;
        assert_eq!(
            seen["provider"],
            json!({ "enabled": true, "minutes": 25 }),
            "installed and on, with the minutes one met threshold is worth"
        );
    }

    // --- The migration itself: a session authenticated but never scoped. ------------------
    //
    // **Written because a mutation survived without it.** Replacing `require_auth`'s scope read
    // with `.unwrap_or(Scope::Dashboard)` — the obvious way to stop legacy sessions being
    // annoying — reopened `O89` for every session that already exists, and the entire suite
    // stayed green. The section above does not cover this: a nonsense cookie never authenticates,
    // so it exercises the `AUTH_KEY` gate and not the scope gate at all.
    //
    // This is the *only* test that distinguishes fail-closed from fail-open, so a record is built
    // by hand: nothing in the running program can still mint an unscoped session, which is
    // exactly what makes the state unreachable by any other route.
    {
        let state = state_with(test_config());
        let sessions = state.sessions.clone();
        let app = app_with(state);

        let mut legacy = tower_sessions::session::Record {
            id: tower_sessions::session::Id::default(),
            data: std::collections::HashMap::from([("authenticated".to_owned(), json!(true))]),
            expiry_date: tower_sessions::cookie::time::OffsetDateTime::now_utc()
                + tower_sessions::cookie::time::Duration::days(30),
        };
        tower_sessions::SessionStore::create(&sessions, &mut legacy)
            .await
            .expect("seeding a pre-scope session");

        let cookie = format!("hh_session={}", legacy.id);
        assert_eq!(
            send(&app, &cookie, "GET", "/api/providers", json!({})).await,
            StatusCode::UNAUTHORIZED,
            "a session authenticated before scopes existed must be refused, not promoted. \
             Defaulting it to a full session would leave `O89` open in precisely the installs \
             that already have it — which is every install that exists today."
        );
        // And the same session cannot push either, so the refusal is not route-specific.
        assert_eq!(
            send(
                &app,
                &cookie,
                "POST",
                "/api/extra-time",
                json!({ "minutes": 10 })
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
    }
}
