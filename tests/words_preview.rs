//! `POST /api/rules/words-preview` — which of the child's running programs a group's words would
//! match, answered by the enforcer's own rule on the same owner snapshot.
//!
//! The dashboard shows this under a group's words so that a word broad enough to catch something
//! unexpected is visible before it closes anything (`docs/WORDS-IN-GROUPS.md`).

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::{Value, json};

use nestwatch::control::FakeControl;

mod common;
use common::{
    FAKE_CHROME as CHROME, FAKE_DISCORD as DISCORD, FAKE_MINECRAFT as MINECRAFT, PASSWORD,
    app_with, body_json, login, post_json, state_with, test_config,
};

async fn preview(app: &axum::Router, cookie: Option<&str>, words: Value) -> (StatusCode, Value) {
    let res = post_json(
        app,
        "/api/rules/words-preview",
        cookie,
        json!({ "words": words }),
    )
    .await;
    let status = res.status();
    (status, body_json(res).await)
}

/// An app whose fake reports `child` as the signed-in user's processes, and whose config names
/// `Discord.exe` as a check program.
async fn app(child: Result<BTreeSet<u32>, String>) -> (axum::Router, String) {
    let mut cfg = test_config();
    common::with_check_program(&mut cfg, "Discord.exe");
    let fake = FakeControl::new();
    fake.script_child_pids(child);
    let mut state = state_with(cfg);
    state.control = Arc::new(fake);
    let app = app_with(state);
    let cookie = login(&app, PASSWORD)
        .await
        .expect("the test password signs in");
    (app, cookie)
}

#[tokio::test]
async fn it_lists_the_childs_programs_the_words_match() {
    let (app, cookie) = app(Ok(BTreeSet::from([CHROME, MINECRAFT, DISCORD]))).await;
    let (status, body) = preview(&app, Some(&cookie), json!(["%craft%", "chrome", "cord"])).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["programs"],
        json!(["Minecraft.exe", "chrome.exe"]),
        "Discord.exe is a configured check program and must not be listed"
    );
}

#[tokio::test]
async fn a_program_under_another_account_is_not_listed() {
    let (app, cookie) = app(Ok(BTreeSet::from([CHROME]))).await;
    let (_, body) = preview(&app, Some(&cookie), json!(["craft", "chrome"])).await;
    assert_eq!(body["programs"], json!(["chrome.exe"]));
}

/// Unknown is not "nothing matches": the dashboard must not reassure a parent about a broad word
/// on the strength of a lookup that failed.
#[tokio::test]
async fn a_failed_owner_lookup_is_null_not_empty() {
    let (app, cookie) = app(Err("denied".into())).await;
    let (status, body) = preview(&app, Some(&cookie), json!(["craft"])).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["programs"], Value::Null);
}

#[tokio::test]
async fn it_needs_a_signed_in_parent() {
    let (app, _) = app(Ok(BTreeSet::new())).await;
    let (status, _) = preview(&app, None, json!(["craft"])).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// The same bounds a save applies, so the preview is no wider a door than the rule it previews.
#[tokio::test]
async fn it_is_bounded_like_the_words_it_previews() {
    let (app, cookie) = app(Ok(BTreeSet::new())).await;
    let at: Vec<String> = (0..nestwatch::rules::MAX_RULE_ENTRIES)
        .map(|i| format!("word{i}"))
        .collect();
    let (status, _) = preview(&app, Some(&cookie), json!(at)).await;
    assert_eq!(status, StatusCode::OK);
    let mut over = at.clone();
    over.push("oneword".into());
    let (status, _) = preview(&app, Some(&cookie), json!(over)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let long = "x".repeat(nestwatch::rules::MAX_RULE_NAME + 3);
    let (status, _) = preview(&app, Some(&cookie), json!([long])).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
