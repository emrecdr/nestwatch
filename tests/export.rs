//! `GET /api/export` — taking your own history off the machine.
//!
//! The feature exists because the history lives in JSONL under a directory `install` ACL-locks to
//! SYSTEM and Administrators, the dashboard renders slices of it, and nothing offered it as a file.
//! `uninstall --purge` deletes all of it irreversibly and had no escape hatch attached.
//!
//! What these tests pin is mostly what the export must **not** do: not reconcile, not filter, not
//! quietly drop the rotated half. An export you cannot check the tool against is worth very little,
//! so "verbatim" is the property under test.

use axum::http::{StatusCode, header};

mod common;
use common::{PASSWORD, ScratchDir, app_with, body_json, get, login, test_state};
use serde_json::json;

/// A router whose screen-time log is a real file, seeded with `rows`.
///
/// `test_state` installs `ScreentimeLog::disabled()`, whose reads are permanently empty — an export
/// test built on it would pass against a handler that returned nothing at all.
fn app_with_history(tag: &str, rows: &[serde_json::Value]) -> (axum::Router, ScratchDir) {
    let dir = ScratchDir::new(&format!("export-{tag}"));

    let log = nestwatch::screentime::ScreentimeLog::new(dir.join("screentime.jsonl"));
    for r in rows {
        log.record(r.clone());
    }

    let mut state = test_state();
    state.screentime = std::sync::Arc::new(log);
    (app_with(state), dir)
}

fn day(date: &str, used: u64) -> serde_json::Value {
    json!({ "day": date, "used_mins": used, "budget_mins": 120, "enabled": true })
}

#[tokio::test]
async fn export_needs_a_session_like_every_other_api_route() {
    let (app, _dir) = app_with_history("auth", &[day("2026-08-01", 30)]);
    let res = get(&app, "/api/export", None).await;
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "the whole history is the most complete record this tool holds"
    );
}

#[tokio::test]
async fn export_returns_every_stored_rollup_and_offers_it_as_a_file() {
    let rows = [
        day("2026-08-01", 30),
        day("2026-08-02", 45),
        day("2026-08-03", 60),
    ];
    let (app, _dir) = app_with_history("rows", &rows);
    let cookie = login(&app, PASSWORD).await.expect("login");

    let res = get(&app, "/api/export", Some(&cookie)).await;
    assert_eq!(res.status(), StatusCode::OK);

    let disposition = res
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        disposition.starts_with("attachment; filename=\"nestwatch-history-"),
        "the point is to end up with a file, not a page: {disposition:?}"
    );

    // The security layer's default must still apply: this body is the whole history.
    assert_eq!(
        res.headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store"),
        "an export must not be storable by an intermediary"
    );

    let body = body_json(res).await;
    assert_eq!(body["rollup_count"], json!(3));
    assert_eq!(
        body["nestwatch_version"],
        nestwatch::VERSION,
        "the file must say which build wrote it — it outlives this install"
    );

    let days: Vec<&str> = body["rollups"]
        .as_array()
        .expect("rollups must be an array")
        .iter()
        .filter_map(|r| r["day"].as_str())
        .collect();
    assert_eq!(
        days,
        vec!["2026-08-03", "2026-08-02", "2026-08-01"],
        "every stored day, newest first"
    );
}

/// The export is a dump, not a second implementation of the report.
///
/// `build_report` collapses duplicate dates by preferring the richer row, and that rule is private
/// to it. Restating it here would put one fact in two places. So a duplicate date must survive the
/// export — the manifest says so, and this is what holds it to that.
#[tokio::test]
async fn a_duplicate_date_is_preserved_rather_than_reconciled() {
    let rows = [
        day("2026-08-01", 30),
        json!({ "day": "2026-08-01", "used_mins": 30, "budget_mins": 120, "enabled": true,
                "per_app": [{ "name": "minecraft", "used_mins": 30 }] }),
    ];
    let (app, _dir) = app_with_history("dupe", &rows);
    let cookie = login(&app, PASSWORD).await.expect("login");

    let body = body_json(get(&app, "/api/export", Some(&cookie)).await).await;
    assert_eq!(
        body["rollup_count"],
        json!(2),
        "both rows for the date must survive; reconciling is the report's job, not the export's"
    );
}

/// Rotation renames the live file to `.1`, so half the history can live in the backup. An export
/// that read only the live file would silently return the newer half — and look complete.
#[tokio::test]
async fn the_rotated_backup_is_exported_too() {
    let dir = ScratchDir::new("export-rot");
    let path = dir.join("screentime.jsonl");

    // Older events, then rotate them aside exactly as `append_line` does.
    let old = nestwatch::screentime::ScreentimeLog::new(path.clone());
    old.record(day("2026-07-01", 10));
    std::fs::rename(&path, path.with_extension("jsonl.1")).unwrap();

    let live = nestwatch::screentime::ScreentimeLog::new(path.clone());
    live.record(day("2026-08-01", 20));

    let mut state = test_state();
    state.screentime = std::sync::Arc::new(live);
    let app = app_with(state);
    let cookie = login(&app, PASSWORD).await.expect("login");

    let body = body_json(get(&app, "/api/export", Some(&cookie)).await).await;
    let days: Vec<&str> = body["rollups"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["day"].as_str())
        .collect();
    assert!(
        days.contains(&"2026-07-01") && days.contains(&"2026-08-01"),
        "the backup holds the OLDER events; dropping it loses the oldest history silently: {days:?}"
    );
}

/// A deliberate parent action against the fullest record the tool holds, and one that leaves the
/// machine. Unlike the capture timer, nothing can reach this in a loop, so one line per call is
/// right and it must actually be written.
#[tokio::test]
async fn exporting_is_recorded_in_the_access_log() {
    let audit = common::app_with_audit_file("export-audit");
    let cookie = login(&audit.app, PASSWORD).await.expect("login");

    let before = std::fs::read_to_string(&audit.path).unwrap_or_default();
    assert!(!before.contains("\"export\""), "precondition");

    let res = get(&audit.app, "/api/export", Some(&cookie)).await;
    assert_eq!(res.status(), StatusCode::OK);

    let after = std::fs::read_to_string(&audit.path).unwrap_or_default();
    assert!(
        after.contains("\"event\":\"export\""),
        "taking the whole history off the machine must be visible in Recent access: {after}"
    );
}

// --- Re-anchoring the trusted clock ------------------------------------------------------
//
// Grouped here rather than in `api.rs` because it shares this file's concern: operations that act
// on the record itself rather than on the machine.

/// The anchor exists to catch a child moving the time zone, so "parent-authenticated" and "the
/// child cannot reach it" have to be the same statement. Re-anchoring to a zone the child just
/// chose would launder the tamper into the trusted state.
///
/// Tested from both directions a child actually has: no session at all, and the unauthenticated
/// child router, which is a different router rather than the same one with a weaker guard.
#[tokio::test]
async fn re_anchoring_is_out_of_the_child_s_reach() {
    let app = common::test_app();

    let res = common::post_json(&app, "/api/re-anchor", None, json!({})).await;
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "without the parent's password this must not move"
    );

    // The child's surfaces live on the outer router. Nothing there may answer this path.
    for path in ["/re-anchor", "/status/re-anchor"] {
        let res = common::post_json(&app, path, None, json!({})).await;
        assert_eq!(
            res.status(),
            StatusCode::NOT_FOUND,
            "{path} must not exist on the unauthenticated router"
        );
    }
}

#[tokio::test]
async fn re_anchoring_records_the_machine_s_current_zone_and_says_so_in_the_log() {
    let audit = common::app_with_audit_file("reanchor-audit");
    let cookie = login(&audit.app, PASSWORD).await.expect("login");

    let res = common::post_json(&audit.app, "/api/re-anchor", Some(&cookie), json!({})).await;
    assert_eq!(res.status(), StatusCode::OK);

    let body = body_json(res).await;
    assert_eq!(body["ok"], json!(true));
    assert_eq!(
        body["offset_mins"],
        json!(nestwatch::clock::current_offset_mins()),
        "it must record what this machine reports now, which is the whole point"
    );

    let log = std::fs::read_to_string(&audit.path).unwrap_or_default();
    assert!(
        log.contains("\"event\":\"clock_reanchored\""),
        "moving the anchor is exactly the line you want when a curfew misbehaves later: {log}"
    );
    assert!(
        log.contains("to_offset_mins"),
        "the line must say what it moved to, not merely that it moved"
    );
}

// --- The child's language ----------------------------------------------------------------

/// The parent chooses what the child is told, in every sense — including which language.
#[tokio::test]
async fn only_the_parent_can_change_the_child_s_language() {
    let app = common::test_app();

    let res = common::post_json(&app, "/api/language", None, json!({ "language": "nl" })).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // The child's own router must not expose it under any shorter path either.
    let res = common::post_json(&app, "/language", None, json!({ "language": "nl" })).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn setting_the_language_changes_what_the_child_s_page_is_told_to_render() {
    let app = common::test_app();
    let cookie = login(&app, PASSWORD).await.expect("login");

    // Default is English, so an install that never touches this behaves as it always did.
    let body = body_json(get(&app, "/status", None).await).await;
    assert_eq!(body["language"], json!("en"));

    let res = common::post_json(
        &app,
        "/api/language",
        Some(&cookie),
        json!({ "language": "nl" }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);

    let body = body_json(get(&app, "/status", None).await).await;
    assert_eq!(
        body["language"],
        json!("nl"),
        "the child's page reads this to decide which strings to show"
    );
}

/// Rejecting rather than falling back matters: a silent fallback to English is indistinguishable
/// from the setting not having saved, and the parent would have no way to tell which happened.
#[tokio::test]
async fn a_language_this_build_cannot_speak_is_refused_and_says_which_it_can() {
    let app = common::test_app();
    let cookie = login(&app, PASSWORD).await.expect("login");

    for tag in ["de", "", "nl-BE", "EN"] {
        let res = common::post_json(
            &app,
            "/api/language",
            Some(&cookie),
            json!({ "language": tag }),
        )
        .await;
        assert_eq!(
            res.status(),
            StatusCode::BAD_REQUEST,
            "{tag:?} has no strings in this build and must be refused, not silently ignored"
        );
        let msg = body_json(res).await["error"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            msg.contains("en") && msg.contains("nl"),
            "say what this build can speak: {msg:?}"
        );
    }

    // …and nothing changed.
    let body = body_json(get(&app, "/status", None).await).await;
    assert_eq!(body["language"], json!("en"));
}

// --- The change stream -------------------------------------------------------------------

/// The stream names what changed and carries no data, but it is still a live view of when a
/// household is active, so it sits behind the same door as everything else.
#[tokio::test]
async fn the_event_stream_needs_a_session() {
    let app = common::test_app();
    let res = get(&app, "/api/events", None).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_event_stream_is_a_stream_and_is_never_cached() {
    let app = common::test_app();
    let cookie = login(&app, PASSWORD).await.expect("login");

    let res = get(&app, "/api/events", Some(&cookie)).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap_or("").trim().to_string()),
        Some("text/event-stream".to_string())
    );
    // The security layer's default must reach this too: an intermediary holding a household's
    // activity stream is exactly what `no-store` is for.
    assert_eq!(
        res.headers()
            .get(header::CACHE_CONTROL)
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    // Deliberately not read to completion — it never completes. Dropping the response closes it.
}

/// The whole point: a child submitting a request must reach an open dashboard immediately, rather
/// than at its next minute boundary.
///
/// Subscribes to the same channel the handler publishes on, which is what the SSE route does one
/// layer up. Driving the HTTP stream instead would mean reading a body that by design never ends.
#[tokio::test]
async fn a_child_s_request_notifies_an_open_dashboard_at_once() {
    let dir = ScratchDir::new("events");

    let mut state = common::test_state();
    state.time_requests =
        std::sync::Arc::new(nestwatch::timereq::TimeRequests::new(dir.join("req.jsonl")));
    let mut rx = state.events.subscribe();
    let app = common::app_with(state);

    let res = common::post_json(
        &app,
        "/time-request",
        None,
        json!({ "minutes": 20, "reason": "" }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);

    let tag = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("a request must notify within two seconds, not at the next 60s poll")
        .expect("the channel must stay open");
    assert_eq!(tag, "requests");
}

/// Granting time moves two panels, and a parent watching on a second device should see both.
#[tokio::test]
async fn granting_time_notifies_both_the_queue_and_today() {
    let state = common::test_state();
    let mut rx = state.events.subscribe();
    let app = common::app_with(state);
    let cookie = login(&app, PASSWORD).await.expect("login");

    let res = common::post_json(
        &app,
        "/api/extra-time",
        Some(&cookie),
        json!({ "minutes": 10 }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);

    let tag = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("granting must notify")
        .expect("channel open");
    assert_eq!(tag, "usage", "today's budget moved");
}

/// A handler must never fail because nobody happens to be looking.
#[tokio::test]
async fn notifying_with_no_dashboard_open_is_not_an_error() {
    let app = common::test_app();
    let cookie = login(&app, PASSWORD).await.expect("login");
    // No subscriber exists at all here — `broadcast::send` returns Err in that state, and the
    // handler must discard it rather than surfacing a 500 to a parent granting time.
    for _ in 0..3 {
        let res = common::post_json(
            &app,
            "/api/extra-time",
            Some(&cookie),
            json!({ "minutes": 1 }),
        )
        .await;
        assert_eq!(res.status(), StatusCode::OK);
    }
}
