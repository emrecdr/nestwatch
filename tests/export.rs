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
use common::{PASSWORD, app_with, body_json, get, login, test_state};
use serde_json::json;

/// A router whose screen-time log is a real file, seeded with `rows`.
///
/// `test_state` installs `ScreentimeLog::disabled()`, whose reads are permanently empty — an export
/// test built on it would pass against a handler that returned nothing at all.
fn app_with_history(tag: &str, rows: &[serde_json::Value]) -> (axum::Router, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("nw-export-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

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
    let (app, dir) = app_with_history("auth", &[day("2026-08-01", 30)]);
    let res = get(&app, "/api/export", None).await;
    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "the whole history is the most complete record this tool holds"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn export_returns_every_stored_rollup_and_offers_it_as_a_file() {
    let rows = [
        day("2026-08-01", 30),
        day("2026-08-02", 45),
        day("2026-08-03", 60),
    ];
    let (app, dir) = app_with_history("rows", &rows);
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

    let _ = std::fs::remove_dir_all(&dir);
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
    let (app, dir) = app_with_history("dupe", &rows);
    let cookie = login(&app, PASSWORD).await.expect("login");

    let body = body_json(get(&app, "/api/export", Some(&cookie)).await).await;
    assert_eq!(
        body["rollup_count"],
        json!(2),
        "both rows for the date must survive; reconciling is the report's job, not the export's"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Rotation renames the live file to `.1`, so half the history can live in the backup. An export
/// that read only the live file would silently return the newer half — and look complete.
#[tokio::test]
async fn the_rotated_backup_is_exported_too() {
    let dir = std::env::temp_dir().join(format!("nw-export-rot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
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

    let _ = std::fs::remove_dir_all(&dir);
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
