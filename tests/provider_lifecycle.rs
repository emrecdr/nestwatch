//! What installing, disabling and uninstalling an integration do to the credential it was
//! paired with — `O92`.
//!
//! The registry decided what a provider *may do* and never decided what a provider *is*.
//! `delete_provider` dropped the config entry and `require_auth` read only the session's scope,
//! so the two never met: uninstalling StudyGo from the Integrations card refused its next grant
//! and left it reading the child's day — budget, per-app usage, and up to `MAX_PAGES` window
//! titles — for the remainder of the absolute session cap. Disabling it did the same.
//!
//! The shape of the fix is borrowed rather than invented. GitHub Apps separate *suspend* from
//! *uninstall*: suspension keeps the installation and blocks **all** API access, and is offered
//! explicitly as the alternative to uninstalling, "which has the consequence of deauthorizing
//! every user". Slack's `apps.uninstall` revokes every token for that installation. OWASP's
//! session guidance asks for the same thing in general terms — a session must be renewed or
//! destroyed "after any privilege level change", and it names permission changes. Uninstalling a
//! provider is one.
//!
//! So: **disable keeps the credential and refuses both routes; uninstall revokes the credential.**
//!
//! **Refused with `400`, not `403`, and that is a cross-repo contract rather than a preference.**
//! Voortgang maps `400` to "the integration is not switched on over there" and `401`/`403` to
//! "re-pair" (`nestwatch_client.dart`, `refused` versus `pairingRejected`). A disabled provider
//! needs the first sentence; a revoked one needs the second. Answering a disabled provider with
//! `403` would tell a parent to re-pair a link that is perfectly good.
//!
//! **One test, its own binary**, for the reason `earned_grant.rs` states at length: the sections
//! below persist config and need the process-wide `NESTWATCH_DATA_DIR` override, and concurrent
//! `#[tokio::test]`s in one binary have already deleted each other's scratch directory here.

use axum::http::StatusCode;
use serde_json::{Value, json};

use nestwatch::pairing::Scope;

mod common;
use common::{
    PASSWORD, ScratchDir, app_with, configure_provider, login, pair_with, send_json, state_with,
    test_config,
};

/// A signed-in parent and an integration paired to an installed, enabled `name`.
///
/// Every section below starts here, and none of them is *about* getting here — the behaviour each
/// one proves is entirely in the assertions that follow.
async fn ready(name: &str) -> (axum::Router, String, String) {
    let app = app_with(state_with(test_config()));
    let parent = login(&app, PASSWORD).await.expect("parent login");
    assert_eq!(
        configure_provider(&app, &parent, name, true, 30).await,
        StatusCode::OK,
        "installing {name}"
    );
    let integration = pair_with(
        &app,
        Scope::Integration {
            source: name.into(),
        },
        None,
    )
    .await
    .expect("an integration pairing must produce a session");
    (app, parent, integration)
}

#[tokio::test]
async fn a_providers_credential_follows_the_provider() {
    let tmp = ScratchDir::new("providerlifecycle");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };

    // --- Disable keeps the credential and closes both routes -------------------------
    {
        let (app, parent, phone) = ready("studygo").await;

        // Baseline: the read answers while the provider is installed and on.
        assert_eq!(
            send_json(&app, &phone, "GET", "/api/usage/today", json!({}))
                .await
                .0,
            StatusCode::OK,
        );

        assert_eq!(
            configure_provider(&app, &parent, "studygo", false, 30).await,
            StatusCode::OK,
        );

        // The grant was already refused before this change.
        let (status, body) = send_json(&app, &phone, "POST", "/api/extra-time", json!({})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body["error"], "the 'studygo' integration is turned off",
            "the wording Voortgang classifies on"
        );

        // The read was not, and that is the defect. GitHub's suspend blocks *all* access; this
        // served the child's day to an integration the parent had switched off.
        let (status, body) = send_json(&app, &phone, "GET", "/api/usage/today", json!({})).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a disabled integration must not read the child's day"
        );
        assert_eq!(
            body["error"], "the 'studygo' integration is turned off",
            "both routes must refuse in the same words, or Voortgang classifies one of them wrongly"
        );

        // **`/session` must still vouch for a merely disabled integration**, and a consumer
        // depends on it: Voortgang's pairing screen routes `authenticated != true` to "the PC no
        // longer accepts this link, link again". A switched-off provider is not a bad link, and
        // sending a parent to mint a new one would be the same mis-routing that choosing `400`
        // over `403` avoids at the grant, relocated to the pairing step.
        //
        // This was first written as "it cannot currently break — `auth::me` takes no `State`, so
        // it has no way to read the registry", pinned so that the day someone gave that handler a
        // `State` parameter it would fail here rather than at a household. `F3` is that day, and
        // deliberately: `me` now reads the registry to report the `provider` entry below. The
        // consumer's maintainer asked that this be re-aimed *before* the field landed rather than
        // repaired after it went red, because the general property ("`/session` ignores the
        // registry") was only ever a proxy for the narrow one that actually protects a parent —
        // **a disabled provider still reports `authenticated: true` and its own scope**. That is
        // what is pinned now, and it survives the coupling instead of dying with it.
        let (status, body) = send_json(&app, &phone, "GET", "/session", json!({})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["authenticated"],
            json!(true),
            "a disabled integration is switched off, not unpaired"
        );
        assert_eq!(
            body["scope"],
            json!({ "kind": "integration", "source": "studygo" }),
            "and it is still the integration it was minted for"
        );
        // The half that was missing, and the reason a parent could be told "linked and working"
        // by a screen while every grant behind it was refused: the credential is good and the
        // integration is off, and until now only the first of those was reported.
        assert_eq!(
            body["provider"],
            json!({ "enabled": false, "minutes": 30 }),
            "the pairing screen can now say *why* nothing is landing, without pushing a grant"
        );

        // The credential survived: switching back on restores it without re-pairing, which is
        // the whole difference between suspend and uninstall.
        assert_eq!(
            configure_provider(&app, &parent, "studygo", true, 30).await,
            StatusCode::OK,
        );
        assert_eq!(
            send_json(&app, &phone, "GET", "/api/usage/today", json!({}))
                .await
                .0,
            StatusCode::OK,
            "re-enabling must not cost a re-pair"
        );
    }

    // --- Uninstall revokes the credential --------------------------------------------
    {
        let (app, parent, phone) = ready("studygo").await;
        assert_eq!(
            send_json(&app, &phone, "GET", "/api/usage/today", json!({}))
                .await
                .0,
            StatusCode::OK,
        );

        assert_eq!(
            send_json(
                &app,
                &parent,
                "POST",
                "/api/providers/studygo/delete",
                json!({})
            )
            .await
            .0,
            StatusCode::OK,
        );

        // Unauthorized, not Forbidden: the session is gone rather than confined, and `401` is
        // the answer Voortgang turns into "link this app again".
        assert_eq!(
            send_json(&app, &phone, "GET", "/api/usage/today", json!({}))
                .await
                .0,
            StatusCode::UNAUTHORIZED,
            "uninstalling must end the sessions it authorised, not only refuse the grant"
        );
        assert_eq!(
            send_json(&app, &phone, "POST", "/api/extra-time", json!({}))
                .await
                .0,
            StatusCode::UNAUTHORIZED,
        );

        // `/session` stopped vouching for a provider that no longer exists — it reports the
        // credential honestly, and there is no longer a credential. Contrast the disabled case
        // above, which must keep answering `true`.
        let (status, body) = send_json(&app, &phone, "GET", "/session", json!({})).await;
        assert_eq!(status, StatusCode::OK, "/session answers everyone");
        assert_eq!(body["authenticated"], json!(false));
        assert_eq!(body["scope"], Value::Null);
        assert_eq!(
            body["provider"],
            Value::Null,
            "and there is no entry left to describe"
        );

        // And the parent's Devices card no longer offers a revoke for a device already gone.
        let (_, rows) = send_json(&app, &parent, "GET", "/api/sessions", json!({})).await;
        let integrations: Vec<&Value> = rows
            .as_array()
            .expect("sessions is an array")
            .iter()
            .filter(|r| r["scope"]["kind"] == "integration")
            .collect();
        assert!(
            integrations.is_empty(),
            "no integration session should survive its provider: {integrations:?}"
        );

        // The parent's own session is untouched by either operation.
        assert_eq!(
            send_json(&app, &parent, "GET", "/api/usage/today", json!({}))
                .await
                .0,
            StatusCode::OK,
            "revoking an integration must not sign the parent out"
        );
    }

    // --- Uninstalling one provider leaves another one's credential alone --------------
    {
        let (app, parent, _studygo) = ready("studygo").await;
        assert_eq!(
            configure_provider(&app, &parent, "chores", true, 30).await,
            StatusCode::OK,
        );
        let chores = pair_with(
            &app,
            Scope::Integration {
                source: "chores".into(),
            },
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            send_json(
                &app,
                &parent,
                "POST",
                "/api/providers/studygo/delete",
                json!({})
            )
            .await
            .0,
            StatusCode::OK,
        );
        assert_eq!(
            send_json(&app, &chores, "GET", "/api/usage/today", json!({}))
                .await
                .0,
            StatusCode::OK,
            "revocation must be scoped to the provider removed, not to every integration"
        );
    }
}
