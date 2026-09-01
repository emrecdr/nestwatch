//! `GET`/`POST /api/policy` — taking a household's settings off the machine, and putting them back.
//!
//! # What these are really guarding
//!
//! The feature is a convenience; two of its properties are not. A restore writes straight into the
//! config that decides when a child's PC locks, from a file a parent may have carried on a USB
//! stick, so the interesting tests are the ones about what a document is **not** allowed to reach:
//!
//! * the **trusted-clock anchor** (`tz_offset_mins` / `tz_zone`), recorded at install against the
//!   machine the child sits at. An imported anchor would leave the enforcer comparing against a
//!   zone this PC is not in — the exact state a child gains two hours of evening from, and the
//!   reason `POST /api/re-anchor` exists as a separate, deliberate action.
//! * `Curfew::extra_until`, which suppresses bedtime until an instant. It cannot be excluded by
//!   leaving a field out of `Policy`, because it lives *inside* `Curfew` — so a hand-edited file
//!   carrying one far in the future would switch the curfew off and look like a restore.
//!
//! Both are asserted against a document that actively tries, rather than against a well-formed one.

use chrono::Datelike;
use nestwatch::config::{Config, Language, Policy};
use serde_json::{Value, json};

mod common;
use common::{body_json, get, login, post_json, state_with, test_config};

/// A config that is recognisably *this* machine: an anchored clock, a granted extension, a
/// password and a port. Everything a restore must leave alone.
fn anchored_config() -> Config {
    let mut cfg = test_config();
    cfg.port = 9443;
    cfg.tz_offset_mins = Some(60);
    cfg.tz_zone = Some("W. Europe Standard Time".into());
    cfg.language = Language::En;
    // A bedtime extension granted a moment ago. Load-bearing for the export test below: with this
    // `None`, `extra_until` serializes as `null` whether or not `Config::policy` clears it, so the
    // assertion that it is absent from the file passes for the wrong reason. Found by mutating
    // `policy()` to stop clearing it and watching every test stay green.
    cfg.curfew.extra_until = Some(live_extension());
    cfg
}

/// The extension this machine already has. Deliberately a *different* instant from the one the
/// hostile document below carries, so "the live value survived" and "the document's value was
/// ignored" are distinguishable — asserting only that the result is absent would pass if the
/// import cleared both, which is not the behaviour a parent who just granted half an hour wants.
fn live_extension() -> chrono::DateTime<chrono::FixedOffset> {
    chrono::DateTime::parse_from_rfc3339("2030-06-01T21:30:00+00:00").expect("fixed instant")
}

#[tokio::test]
async fn a_parent_can_download_their_settings_and_the_file_carries_no_secrets() {
    let state = state_with(anchored_config());
    let app = common::app_with(state);
    let cookie = login(&app, common::PASSWORD).await.expect("sign in");

    let res = get(&app, "/api/policy", Some(&cookie)).await;
    assert_eq!(res.status(), 200);

    let disposition = res
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        disposition.starts_with("attachment;"),
        "the link is a plain `<a download>`, so the browser must be told to save it rather than \
         render it: {disposition:?}"
    );

    let doc = body_json(res).await;
    assert!(doc["policy"].is_object(), "the document carries a policy");
    assert_eq!(doc["nestwatch_version"], nestwatch::VERSION);

    // The whole document, as text, must not contain the machine's secrets or identity. Asserted on
    // the serialized bytes rather than field-by-field: a future field added to `Policy` that
    // happened to carry one of these would slip past a field-name check and not past this.
    let text = serde_json::to_string(&doc).unwrap();
    for forbidden in [
        "$argon2",
        "password_hash",
        "tz_zone",
        "tz_offset",
        "cert_sans",
    ] {
        assert!(
            !text.contains(forbidden),
            "an exported settings file is meant to be copied about — it must not contain \
             {forbidden:?}: {text}"
        );
    }

    // Tonight's granted extension is state, not a setting, and it is the one exclusion that cannot
    // be made by leaving a field off `Policy` — it lives inside `Curfew`. Exported, it would travel
    // to another machine as a bedtime suppression nobody granted there.
    assert_eq!(
        doc["policy"]["curfew"]["extra_until"],
        Value::Null,
        "the export carried tonight's bedtime extension: {text}"
    );
    assert!(
        !text.contains("2099"),
        "the extension leaked somewhere else in the document: {text}"
    );
}

#[tokio::test]
async fn restoring_settings_cannot_move_the_trusted_clock() {
    let state = state_with(anchored_config());
    let app = common::app_with(state.clone());
    let cookie = login(&app, common::PASSWORD).await.expect("sign in");

    // A document that tries. `tz_zone`, `tz_offset_mins`, `port` and `password_hash` are not
    // fields of `Policy` at all, so serde ignores them — this asserts that ignoring is what
    // actually happens, against a file that names them anyway.
    let hostile = json!({
        "nestwatch_version": nestwatch::VERSION,
        "tz_zone": "Pacific/Kiritimati",
        "tz_offset_mins": 840,
        "port": 1,
        "password_hash": "$argon2id$v=19$fake",
        "policy": {
            "curfew": { "enabled": true, "start": "21:00", "end": "07:00", "warn_secs": 60 },
            "rules": { "daily_budget_mins": 45 },
            "routines": [],
            "language": "nl"
        }
    });

    let res = post_json(&app, "/api/policy", Some(&cookie), hostile.clone()).await;
    assert_eq!(res.status(), 200, "a well-formed policy still applies");

    let cfg = nestwatch::state::recover_read(&state.config);
    assert_eq!(
        cfg.tz_zone.as_deref(),
        Some("W. Europe Standard Time"),
        "the clock anchor moved. A restore that can re-anchor the clock is a restore that can \
         hand a child two hours of evening, which is precisely why re-anchoring is its own \
         authenticated action that reads the machine"
    );
    assert_eq!(cfg.tz_offset_mins, Some(60), "the offset anchor moved too");
    assert_eq!(cfg.port, 9443, "the port is a property of the install");
    assert!(
        cfg.password_hash.starts_with("$argon2"),
        "the password hash was overwritten by the document"
    );

    // …and the parts that *are* the household did apply.
    assert_eq!(cfg.rules.daily_budget_mins, 45);
    assert_eq!(cfg.curfew.start, "21:00");
    assert_eq!(cfg.language, Language::Nl);
}

#[tokio::test]
async fn restoring_settings_cannot_suppress_tonights_bedtime() {
    let mut cfg = anchored_config();
    cfg.curfew.enabled = true;
    let state = state_with(cfg);
    let app = common::app_with(state.clone());
    let cookie = login(&app, common::PASSWORD).await.expect("sign in");

    // `extra_until` lives INSIDE `Curfew`, so unlike the fields above it cannot be kept out by
    // leaving it off `Policy`. A document carrying one far in the future would switch bedtime off
    // for a century and read as an ordinary restore.
    let hostile = json!({
        "policy": {
            "curfew": {
                "enabled": true,
                "start": "21:00",
                "end": "07:00",
                "warn_secs": 60,
                "extra_until": "2099-01-01T00:00:00+00:00"
            },
            "rules": {},
            "routines": [],
            "language": "en"
        }
    });

    let res = post_json(&app, "/api/policy", Some(&cookie), hostile.clone()).await;
    assert_eq!(res.status(), 200);

    let cfg = nestwatch::state::recover_read(&state.config);
    let kept = cfg
        .curfew
        .extra_until
        .expect("the live extension must survive a restore");
    assert_eq!(
        kept.to_rfc3339(),
        live_extension().to_rfc3339(),
        "a restored file moved tonight's bedtime extension. `extra_until` is state a parent set a \
         moment ago, not a setting — it must come from the LIVE config and never from the \
         document, or a hand-edited file switches bedtime off and reads as an ordinary restore"
    );
    assert!(
        kept.year() < 2099,
        "the document's instant won: bedtime is now suppressed until {kept}"
    );
    assert!(cfg.curfew.enabled, "the rest of the curfew still applied");
}

#[tokio::test]
async fn a_paused_enforcer_is_not_resumed_behind_the_parents_back() {
    let mut cfg = anchored_config();
    cfg.rules.enabled = false; // the parent paused enforcement ten minutes ago
    let state = state_with(cfg);
    let app = common::app_with(state.clone());
    let cookie = login(&app, common::PASSWORD).await.expect("sign in");

    // `Curfew` has no serde default for `start`/`end`, so an empty object really is malformed —
    // spelled out rather than relying on `{}`, which this test originally did and which serde
    // rejected with a 422. That strictness is correct and is asserted separately below.
    let doc = json!({
        "policy": {
            "curfew": { "enabled": false, "start": "22:00", "end": "07:00", "warn_secs": 60 },
            "rules": { "enabled": true, "daily_budget_mins": 60 },
            "routines": [], "language": "en"
        }
    });
    let res = post_json(&app, "/api/policy", Some(&cookie), doc.clone()).await;
    assert_eq!(res.status(), 200);

    let cfg = nestwatch::state::recover_read(&state.config);
    assert!(
        !cfg.rules.enabled,
        "pausing is a temporary override, not a setting — `apply_routine` already decided this \
         case and a restore is the same shape"
    );
    assert_eq!(
        cfg.rules.daily_budget_mins, 60,
        "the rules themselves applied"
    );
}

#[tokio::test]
async fn an_invalid_document_changes_nothing_at_all() {
    let mut cfg = anchored_config();
    cfg.rules.daily_budget_mins = 120;
    cfg.curfew.start = "22:00".into();
    let state = state_with(cfg);
    let app = common::app_with(state.clone());
    let cookie = login(&app, common::PASSWORD).await.expect("sign in");

    // Valid rules, invalid curfew. The curfew is checked first, so this also pins that a failure
    // in the FIRST section does not leave the later ones applied.
    let bad = json!({
        "policy": {
            "curfew": { "enabled": true, "start": "25:99", "end": "07:00", "warn_secs": 60 },
            "rules": { "daily_budget_mins": 15 },
            "routines": [],
            "language": "en"
        }
    });
    let res = post_json(&app, "/api/policy", Some(&cookie), bad.clone()).await;
    assert_eq!(res.status(), 400, "an invalid document is refused");

    let cfg = nestwatch::state::recover_read(&state.config);
    assert_eq!(
        cfg.rules.daily_budget_mins, 120,
        "a rejected restore is all-or-nothing: the budget from the bad document was applied even \
         though the curfew in it was refused, which leaves a household with half of one setup"
    );
    assert_eq!(cfg.curfew.start, "22:00");
}

#[tokio::test]
async fn an_unrelated_json_file_is_refused_rather_than_read_as_an_empty_setup() {
    let state = state_with(anchored_config());
    let app = common::app_with(state.clone());
    let cookie = login(&app, common::PASSWORD).await.expect("sign in");

    // The failure this guards is silent and total: with `policy` optional, a parent uploading the
    // wrong file — their *history* export, say, which sits next to it in the downloads folder —
    // would restore an empty policy over their whole setup and be told it worked.
    let wrong_file = json!({ "nestwatch_version": nestwatch::VERSION, "rollups": [] });
    let res = post_json(&app, "/api/policy", Some(&cookie), wrong_file.clone()).await;
    assert!(
        res.status().is_client_error(),
        "the wrong file must be refused, not read as 'no settings': {}",
        res.status()
    );

    let cfg = nestwatch::state::recover_read(&state.config);
    assert_eq!(cfg.port, 9443, "nothing was touched");
}

/// A half-written curfew is refused rather than defaulted into something plausible.
///
/// Found by getting it wrong: two fixtures here used `"curfew": {}` and serde answered 422,
/// because `Curfew::start` and `Curfew::end` carry no `#[serde(default)]`. That is the behaviour
/// worth keeping — a document naming a curfew but omitting when it starts must not silently become
/// a curfew at some default hour, which is a bedtime nobody chose appearing on a child's PC.
#[tokio::test]
async fn a_half_written_curfew_is_refused_rather_than_defaulted() {
    let state = state_with(anchored_config());
    let app = common::app_with(state.clone());
    let cookie = login(&app, common::PASSWORD).await.expect("sign in");

    let partial = json!({ "policy": { "curfew": { "enabled": true }, "rules": {},
                                      "routines": [], "language": "en" } });
    let res = post_json(&app, "/api/policy", Some(&cookie), partial.clone()).await;
    assert!(
        res.status().is_client_error(),
        "a curfew with no times must be refused, not defaulted: {}",
        res.status()
    );
    assert_eq!(
        nestwatch::state::recover_read(&state.config).port,
        9443,
        "nothing was touched"
    );
}

#[tokio::test]
async fn a_file_from_another_version_applies_and_says_so() {
    let state = state_with(anchored_config());
    let app = common::app_with(state);
    let cookie = login(&app, common::PASSWORD).await.expect("sign in");

    let doc = json!({
        "nestwatch_version": "0.0.1-from-the-past",
        "policy": {
            "curfew": { "enabled": false, "start": "22:00", "end": "07:00", "warn_secs": 60 },
            "rules": {}, "routines": [], "language": "en"
        }
    });
    let res = post_json(&app, "/api/policy", Some(&cookie), doc.clone()).await;
    assert_eq!(res.status(), 200, "a version difference is not a refusal");

    let out: Value = body_json(res).await;
    let warning = out["warning"].as_str().unwrap_or_default();
    assert!(
        warning.contains("0.0.1-from-the-past") && warning.contains(nestwatch::VERSION),
        "the warning must name BOTH versions — 'this file is from another version' is not \
         something a parent can act on: {warning:?}"
    );

    // Same version: no warning at all, so the message means something when it does appear.
    let same = json!({
        "nestwatch_version": nestwatch::VERSION,
        "policy": {
            "curfew": { "enabled": false, "start": "22:00", "end": "07:00", "warn_secs": 60 },
            "rules": {}, "routines": [], "language": "en"
        }
    });
    let res = post_json(&app, "/api/policy", Some(&cookie), same.clone()).await;
    let out: Value = body_json(res).await;
    assert!(
        out["warning"].is_null(),
        "a same-version restore must be quiet, or the warning becomes noise: {out}"
    );
}

/// A round trip through the real endpoints preserves what it claims to.
///
/// Pinned end-to-end rather than by comparing structs, because the failure mode is a field that
/// serializes and does not deserialize — which a struct comparison in one process cannot see.
#[tokio::test]
async fn a_download_and_restore_round_trip_keeps_the_setup() {
    let mut cfg = anchored_config();
    cfg.language = Language::Nl;
    cfg.curfew.enabled = true;
    cfg.curfew.start = "20:30".into();
    cfg.rules.daily_budget_mins = 75;
    cfg.rules.blocklist = vec!["game.exe".into()];
    cfg.routines = vec![nestwatch::config::Routine {
        name: "Homework".into(),
        rules: nestwatch::rules::Rules {
            daily_budget_mins: 30,
            ..Default::default()
        },
    }];
    let state = state_with(cfg);
    let app = common::app_with(state.clone());
    let cookie = login(&app, common::PASSWORD).await.expect("sign in");

    let doc = body_json(get(&app, "/api/policy", Some(&cookie)).await).await;

    // Wipe the live settings, then restore from the downloaded document.
    let cleared = json!({ "policy": Policy::default() });
    assert_eq!(
        post_json(&app, "/api/policy", Some(&cookie), cleared.clone())
            .await
            .status(),
        200
    );
    assert_eq!(
        nestwatch::state::recover_read(&state.config)
            .rules
            .daily_budget_mins,
        0,
        "the wipe must really have happened, or the restore below proves nothing"
    );

    assert_eq!(
        post_json(&app, "/api/policy", Some(&cookie), doc.clone())
            .await
            .status(),
        200
    );

    let cfg = nestwatch::state::recover_read(&state.config);
    assert_eq!(cfg.rules.daily_budget_mins, 75);
    assert_eq!(cfg.rules.blocklist, vec!["game.exe".to_string()]);
    assert_eq!(cfg.curfew.start, "20:30");
    assert!(cfg.curfew.enabled);
    assert_eq!(cfg.language, Language::Nl);
    assert_eq!(cfg.routines.len(), 1, "routines are the laborious part");
    assert_eq!(cfg.routines[0].name, "Homework");
    assert_eq!(cfg.routines[0].rules.daily_budget_mins, 30);
}
