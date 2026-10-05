//! `POST /api/rules/preview` says what saving a set of rules would do to today — and saves nothing.
//!
//! Its own test binary because it writes today's tally into a scratch data directory through
//! `NESTWATCH_DATA_DIR`, which is process-wide.
//!
//! Why the endpoint exists: lowering the limit below what has already been used today puts the
//! PC over it the moment the save lands, and the enforcer then acts after its short grace — with
//! none of the 15, 5 and 1-minute warnings the README promises, because there was never a moment
//! when 15 minutes were left. The parent making that change on a phone could not see it coming.

use axum::http::StatusCode;
use chrono::Duration;
use serde_json::{Value, json};

use nestwatch::config::{Routine, data_paths};
use nestwatch::rules::{EnforceAction, Rules, Usage};

mod common;
use common::{
    PASSWORD, ScratchDir, app_with, body_json, login, post_json, state_with, test_config,
};

fn rules(daily_budget_mins: u32) -> Rules {
    Rules {
        enabled: true,
        daily_budget_mins,
        budget_action: EnforceAction::Lock,
        warn_secs: 30,
        ..Default::default()
    }
}

fn write_tally(minutes: u64) {
    let usage = Usage {
        day: Some(nestwatch::config::today()),
        total_secs: minutes * 60,
        ..Default::default()
    };
    std::fs::create_dir_all(&data_paths().dir).unwrap();
    std::fs::write(
        data_paths().dir.join("usage_state.json"),
        serde_json::to_string(&usage).unwrap(),
    )
    .unwrap();
}

async fn preview(app: &axum::Router, cookie: &str, candidate: &Rules) -> (StatusCode, Value) {
    let res = post_json(
        app,
        "/api/rules/preview",
        Some(cookie),
        serde_json::to_value(candidate).unwrap(),
    )
    .await;
    let status = res.status();
    (status, body_json(res).await)
}

/// One entry point for both scenarios, run in turn. Each writes today's tally into the data
/// directory, and that directory is named by a process-wide environment variable — two `#[test]`
/// functions here would run on parallel threads and point it at each other's files.
#[tokio::test]
async fn rules_preview() {
    let tmp = ScratchDir::new("rules-preview");
    // SAFETY: the only test in this binary, before any data-dir access.
    unsafe { std::env::set_var("NESTWATCH_DATA_DIR", tmp.path()) };

    a_preview_says_when_saving_would_put_today_over_and_changes_nothing().await;
    a_routine_in_force_keeps_a_change_to_the_everyday_rules_from_reaching_today().await;
}

async fn a_preview_says_when_saving_would_put_today_over_and_changes_nothing() {
    let mut config = test_config();
    config.rules = rules(120);
    let state = state_with(config);
    let handle = state.config.clone();
    let app = app_with(state);
    let cookie = login(&app, PASSWORD).await.unwrap();
    write_tally(95);

    // 95 used, the limit cut from 120 to 60: today goes over the moment this is saved.
    let (status, body) = preview(&app, &cookie, &rules(60)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["used_mins"], 95);
    assert_eq!(body["budget_mins"], 60);
    assert_eq!(body["over"], true);
    assert_eq!(
        body["was_over"], false,
        "it is the save that does it, which is what the parent needs to be told"
    );
    assert_eq!(body["action"], "lock");
    assert_eq!(body["warn_secs"], 30);

    // A preview is not a save.
    assert_eq!(
        nestwatch::state::recover_read(&handle)
            .rules
            .daily_budget_mins,
        120
    );
    assert!(
        !data_paths().config.exists(),
        "nothing may be written by asking what a save would do"
    );

    // A cut that still leaves time, and the rules as they stand: nothing new happens.
    let (_, body) = preview(&app, &cookie, &rules(100)).await;
    assert_eq!(body["over"], false);
    let (_, body) = preview(&app, &cookie, &rules(120)).await;
    assert_eq!(
        (body["over"].clone(), body["was_over"].clone()),
        (json!(false), json!(false))
    );

    // Pausing the rules ends the day's limit rather than reaching it.
    let (_, body) = preview(
        &app,
        &cookie,
        &Rules {
            enabled: false,
            ..rules(60)
        },
    )
    .await;
    assert_eq!(body["over"], false, "paused rules count nothing");

    // Already over before the change: the PC is already locked or warned, so the save is not
    // what does it, and the dashboard must not ask as if it were.
    write_tally(130);
    let (_, body) = preview(&app, &cookie, &rules(60)).await;
    assert_eq!(
        (body["over"].clone(), body["was_over"].clone()),
        (json!(true), json!(true))
    );

    // The same validation as a save.
    let (status, _) = preview(
        &app,
        &cookie,
        &Rules {
            warn_secs: 100_000,
            ..rules(60)
        },
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // And a parent's endpoint, like the save.
    let res = post_json(
        &app,
        "/api/rules/preview",
        None,
        serde_json::to_value(rules(60)).unwrap(),
    )
    .await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Minutes granted today count, exactly as they do for the enforcer: 60 plus a 40-minute grant
    // is a 100-minute day, and 95 used is inside it.
    write_tally(95);
    let res = post_json(
        &app,
        "/api/extra-time",
        Some(&cookie),
        json!({ "minutes": 40 }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::OK);
    let (_, body) = preview(&app, &cookie, &rules(60)).await;
    assert_eq!(body["budget_mins"], 100);
    assert_eq!(body["over"], false, "a grant is part of today's limit");
}

/// While a scheduled routine is in force the enforcer counts against *its* rules, so a change to
/// the everyday ones does nothing to today until the routine's window closes — and the preview
/// must say so rather than ask the parent to confirm a lock that will not happen.
///
/// The window is built around the trusted clock's own "now", two hours either side, so it is in
/// force whenever this runs; a window that crosses midnight is one the schedule already supports.
async fn a_routine_in_force_keeps_a_change_to_the_everyday_rules_from_reaching_today() {
    let now = nestwatch::clock::now();
    let hm = |t: chrono::DateTime<chrono::FixedOffset>| t.format("%H:%M").to_string();
    let mut config = test_config();
    config.rules = rules(120);
    config.routines = vec![Routine {
        name: "Homework".into(),
        rules: rules(180),
        schedule: vec![nestwatch::curfew::Window {
            start: hm(now - Duration::hours(2)),
            end: hm(now + Duration::hours(2)),
            days: Default::default(),
        }],
    }];
    let app = app_with(state_with(config));
    let cookie = login(&app, PASSWORD).await.unwrap();
    write_tally(95);

    let (status, body) = preview(&app, &cookie, &rules(60)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["budget_mins"], 180,
        "the routine's limit is the one in force today"
    );
    assert_eq!(
        body["over"], false,
        "cutting the everyday limit cannot put today over while the routine decides it"
    );
}
