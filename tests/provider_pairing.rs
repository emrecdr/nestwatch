//! Minting an integration pairing from the dashboard — `F7`.
//!
//! Until now `pairing::mint` had exactly one caller, reached through `nestwatch pair`, and that
//! command calls `install::ensure_elevated` first. So a parent could install and configure StudyGo
//! from their phone and then had to walk to the child's PC and open an administrator console to
//! make it work. The console step is the reason the feature goes unused.
//!
//! **The elevation check was never a policy about credentials, which is what made this decidable.**
//! Its own comment says why it is there: minting writes into the ACL-locked data dir, so the *CLI*
//! needs elevation the way `install` does. The service already runs as SYSTEM and can write that
//! file. There was no standing decision that creating a credential requires physical access —
//! there was a user-mode process that could not reach a file.
//!
//! What is a real decision is the exposure model. `pairing.rs` states it: the token is printed on a
//! console on the child's own PC, so "the exposure window is while the parent is standing at the
//! machine, and it closes the instant they scan". Minting from the dashboard trades that for an
//! *authentication* model, and this project supports reaching the dashboard through a tunnel that
//! terminates on the LAN — so the window becomes "wherever the parent's browser is". The LAN gate
//! cannot tell those apart, because `require_lan_peer` sits on the outer router and a terminated
//! tunnel looks local by design.
//!
//! So the credential is bounded by **step-up authentication** instead: the parent re-enters the
//! password for this one action, the way GitHub requires sudo-mode before it will create a token.
//! That is what these sections pin, along with the thing a mint must never do — appear in the
//! audit log.
//!
//! One `#[tokio::test]`, like `pairing_scope.rs`: the sections persist config and mint tokens, so
//! they need the process-wide `NESTWATCH_DATA_DIR` override.

use axum::http::StatusCode;
use serde_json::{Value, json};

mod common;
use common::{
    PASSWORD, ScratchDir, app_with_audit_file, body_json, configure_provider, get, login, send_json,
};

/// Redeem a minted token the way a phone does, returning the session cookie it produced.
async fn redeem(app: &axum::Router, token: &str) -> Option<String> {
    let res = get(app, &format!("/p/{token}"), None).await;
    res.headers()
        .get(axum::http::header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|c| c.split(';').next())
        .map(str::to_owned)
}

#[tokio::test]
async fn a_parent_can_mint_a_pairing_without_leaving_the_dashboard() {
    let tmp = ScratchDir::new("providerpairing");
    // SAFETY: single-threaded test entry, before any data-dir access; own test binary.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };

    let a = app_with_audit_file("providerpairing");
    let app = a.app.clone();
    let parent = login(&app, PASSWORD).await.expect("parent login");
    assert_eq!(
        configure_provider(&app, &parent, "studygo", true, 25).await,
        StatusCode::OK
    );

    // --- The wrong password mints nothing. -------------------------------------------------
    //
    // First, deliberately: a section order that mints successfully and *then* tries a bad password
    // cannot tell "refused" from "already spent", because a token is single-use.
    {
        let (status, body) = send_json(
            &app,
            &parent,
            "POST",
            "/api/providers/studygo/pair",
            json!({ "password": "not-the-password" }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "a live session is not enough to create a credential"
        );
        assert!(
            body.get("token").is_none(),
            "and nothing may leak in the refusal: {body}"
        );
    }

    // --- An uninstalled integration cannot be paired to. -----------------------------------
    {
        let (status, body) = send_json(
            &app,
            &parent,
            "POST",
            "/api/providers/nosuch/pair",
            json!({ "password": PASSWORD }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body["error"], "no 'nosuch' integration is installed",
            "the same wording the grant and the read already use, so a client classifies once"
        );
    }

    // --- The right password mints a credential worth exactly the integration. --------------
    {
        let (status, body) = send_json(
            &app,
            &parent,
            "POST",
            "/api/providers/studygo/pair",
            json!({ "password": PASSWORD }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let token = body["token"].as_str().expect("a token").to_string();
        assert!(!token.is_empty());
        assert_eq!(
            body["expires_in_secs"], 900,
            "the parent is told how long they have, rather than finding out by failing"
        );
        let url = body["url"].as_str().expect("a url");
        assert!(
            url.contains(&format!("/p/{token}")),
            "the URL is the one the phone opens: {url}"
        );
        assert!(
            body["qr_svg"]
                .as_str()
                .is_some_and(|s| s.starts_with("<svg")),
            "and a QR, because typing a token on a phone is the friction this removes"
        );

        // The whole point: it redeems, and what it redeems to is the integration — not a
        // dashboard. A mint that produced parent authority would be `O89` reopened from the
        // other end.
        let phone = redeem(&app, &token).await.expect("the token must redeem");
        let (status, session) = send_json(&app, &phone, "GET", "/session", json!({})).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            session["scope"],
            json!({ "kind": "integration", "source": "studygo" }),
            "minted for the provider whose row the parent pressed, and worth only that"
        );
        assert_eq!(
            session["provider"],
            json!({ "enabled": true, "minutes": 25 }),
            "and it can read its own entry straight away"
        );

        // Single-use, as `pairing::redeem` promises. Worth pinning here as well as there,
        // because this route is a second way to reach that file.
        assert!(
            redeem(&app, &token).await.is_none(),
            "a minted token is spent by the first phone that scans it"
        );
    }

    // --- The log records that it happened, and never what was minted. ----------------------
    {
        let rows = body_json(get(&app, "/api/audit", Some(&parent)).await).await;
        let rows = rows.as_array().expect("audit is an array");
        let minted: Vec<&Value> = rows
            .iter()
            .filter(|r| r["event"] == "pairing_minted")
            .collect();
        assert_eq!(
            minted.len(),
            1,
            "one successful mint, so one entry — the refusals above are not mints"
        );
        // `jsonl::record` flattens the fields onto the row beside `ts` and `event` rather than
        // nesting them under a key, so this is `["source"]` and not `["data"]["source"]`.
        assert_eq!(minted[0]["source"], "studygo");

        // **A credential must not be recoverable from the log that records it.** The audit file is
        // readable by every dashboard session and is exported by `/api/export`; a token in it would
        // outlive the fifteen-minute window in a place nothing expires.
        let whole = serde_json::to_string(&rows).unwrap();
        assert!(
            !whole.contains("\"token\""),
            "the audit log must not carry the minted token"
        );
    }

    // Keep the audit file alive until every assertion above has run.
    drop(a);
}
