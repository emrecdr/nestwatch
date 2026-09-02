//! Guards the partition that keeps the audit trail from being erased by someone with no credential.
//!
//! # The property
//!
//! `audit.jsonl` is a fixed-size ring — 2 MiB and one rotated backup — and `GET /api/audit` shows a
//! bounded window onto it. That is safe only while every writer is paced by someone trustworthy.
//! Two of the twenty-seven `audit.record` call sites are not: `auth_failure` and `pair_failed` are
//! written for a caller who has presented no session, no password and no code. Before they were
//! split into their own file, four hundred wrong passwords removed a recorded `lock_issued` from the
//! parent's only view of the log, and enough of them destroy it on disk.
//!
//! [`nestwatch::audit`] holds the unit tests for the mechanism. This file guards the two things a
//! unit test cannot see: that the shipped HTTP path actually routes that way, and that the *list* of
//! attacker-paced events still describes the tree.
//!
//! # Why the list needs a guard at all
//!
//! The reason this was reachable in a released build is written into `audit.rs`'s own history. The
//! decision not to bound these events rested on a sentence — *there are fourteen `audit.record` call
//! sites and the other thirteen are each bounded by a discrete human action* — which was true when
//! written, verified once by hand, and wrong by thirteen sites by the time anyone re-read it. The
//! same sentence was copied into `docs/SECURITY.md`, where more people read it than read the code.
//!
//! A sentence cannot notice a twenty-eighth call site. This can.

use nestwatch::audit::{ATTEMPT_EVENTS, ATTEMPTS_FILE};
use nestwatch::srcscan::{call_arguments, find_tokens, line_of, production_source};
use serde_json::{Value, json};

mod common;
use common::{PASSWORD, app_with_audit_file, body_json, crate_sources, get, login, post_json};

/// Every event tag written to the audit log, and which side of the partition it belongs on.
///
/// `false` = paced by a parent, a session, a secret, or a queue the parent drains; may keep history.
/// `true`  = an unauthenticated LAN caller can cause it at will; must not be able to evict anything.
///
/// Each row is a claim about reachability that someone checked. They were checked individually, not
/// by pattern, because the pattern is exactly what was wrong last time: "everything in `api.rs` is
/// behind `require_auth`" is *nearly* true, and the two exceptions — `time_request_submitted` and
/// `time_code_redeemed` — are the interesting ones. Both are safe, and for reasons that have nothing
/// to do with the module they live in: the first is only recorded when the submission was
/// **accepted**, and acceptance is capped at `timereq::MAX_PENDING` until a parent resolves one; the
/// second is only recorded on a **valid** code, of which at most `timecode::MAX_ACTIVE_CODES` exist.
const CLASSIFIED: [(&str, bool); 31] = [
    // auth.rs — the only module where an unauthenticated caller reaches a writer.
    ("\"auth_failure\"", true), // no credential at all; 5/min/IP from the login limiter
    ("\"pair_failed\"", true),  // no credential; coalesced onto the lockout, so 1/min/IP
    ("\"auth_success\"", false), // requires the password
    ("\"paired\"", false),      // requires the single-use 80-bit pairing token
    ("\"logout\"", false),      // gated on `was_authenticated`: requires a live session
    // api.rs — behind `require_auth`, except the two noted above.
    ("SCREENSHOT_EVENT", false),
    ("LIVE_VIEW_EVENT", false), // timer-paced, and already coalesced by `LiveViewAudit`
    ("\"process_kill\"", false),
    // The settings backup pair. Both are parent-paced: a person clicking a link, and a person
    // choosing a file. Neither can be reached without a session, and neither is on a timer — the
    // property this table exists to record.
    ("\"policy_exported\"", false),
    ("\"policy_imported\"", false),
    ("\"shutdown_issued\"", false),
    ("\"lock_issued\"", false),
    ("\"curfew_change\"", false),
    ("\"curfew_extended\"", false),
    ("\"language_changed\"", false),
    ("\"clock_reanchored\"", false),
    ("\"export\"", false),
    ("\"rules_change\"", false),
    ("\"routine_saved\"", false),
    ("\"routine_applied\"", false),
    ("\"routine_deleted\"", false),
    ("\"time_request_submitted\"", false), // unauthenticated, but capped by MAX_PENDING
    ("\"time_request_approved\"", false),
    ("\"time_request_denied\"", false),
    ("\"time_code_issued\"", false),
    ("\"time_code_redeemed\"", false), // unauthenticated, but requires a live code
    ("\"extra_time_granted\"", false),
    ("\"provider_configured\"", false), // POST /api/providers, behind require_auth
    ("\"provider_removed\"", false),    // POST /api/providers/{name}/delete, behind require_auth
    ("\"password_change_failed\"", false),
    ("\"password_changed\"", false),
];

/// The event tag of every `audit.record(...)` call in production source, with where it was found.
///
/// Matched as a **token sequence** rather than as the string `"audit.record("`, and that is not
/// style. `rustfmt` breaks a call before the dot and after the open paren, so a line-oriented search
/// for that needle stops matching the moment the call grows past the line width — and this guard
/// would then find nothing, report success, and check nothing. `O79` is the record of three
/// scanners that failed exactly that way. [`find_tokens`] matches across the break.
fn recorded_tags() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (path, full) in crate_sources(&["src"]) {
        let text = production_source(&full);
        for at in find_tokens(text, &["audit", ".record", "("]) {
            let Some(args) = call_arguments(text, at) else {
                continue;
            };
            // The tag is the first argument, and it is always a literal or a constant — neither
            // contains a comma, so splitting on the first one is exact rather than approximate.
            let Some(tag) = args.split(',').next().map(str::trim) else {
                continue;
            };
            if tag.is_empty() {
                continue;
            }
            out.push((
                tag.to_string(),
                format!("{}:{}", path.display(), line_of(text, at)),
            ));
        }
    }
    out
}

/// A call site that nobody has classified must fail, on the day it lands.
#[test]
fn every_audit_call_site_has_been_placed_on_one_side_of_the_partition() {
    let found = recorded_tags();

    // Anti-vacuity: if the scan breaks, it finds nothing, every assertion below is trivially true,
    // and this guard passes forever over a file it can no longer read.
    assert!(
        found.len() >= CLASSIFIED.len(),
        "found only {} audit.record call sites; there were {} when this was written, so the scan \
         has stopped matching rather than the code having shrunk",
        found.len(),
        CLASSIFIED.len()
    );

    let unclassified: Vec<&(String, String)> = found
        .iter()
        .filter(|(tag, _)| !CLASSIFIED.iter().any(|(known, _)| known == tag))
        .collect();
    assert!(
        unclassified.is_empty(),
        "audit event(s) with no decision recorded about who paces them:\n{}\n\n\
         Add each to CLASSIFIED in this file. `true` means an unauthenticated LAN caller can cause \
         it at will — those also belong in `audit::ATTEMPT_EVENTS`, or a flood of them will rotate \
         the parent's security history away.",
        unclassified
            .iter()
            .map(|(tag, at)| format!("  {at}  {tag}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The classification and the shipped routing table must be the same list.
///
/// Without this the table above is documentation: someone could mark an event attacker-paced here
/// and the log would still put it where it can evict a shutdown.
#[test]
fn the_classification_and_the_routing_table_agree() {
    let mut classified: Vec<String> = CLASSIFIED
        .iter()
        .filter(|(_, attacker_paced)| *attacker_paced)
        .map(|(tag, _)| tag.trim_matches('"').to_string())
        .collect();
    classified.sort();

    let mut shipped: Vec<String> = ATTEMPT_EVENTS.iter().map(|e| (*e).to_string()).collect();
    shipped.sort();

    assert_eq!(
        classified, shipped,
        "the events this file calls attacker-paced are not the events audit::ATTEMPT_EVENTS routes \
         to the unprivileged log"
    );
}

/// Wrong passwords must reach the file that is allowed to rotate, and no other.
#[tokio::test]
async fn a_wrong_password_is_written_beside_the_action_log_not_into_it() {
    let a = app_with_audit_file("partition-files");
    let attempts_path = a.path.with_file_name(ATTEMPTS_FILE);

    // One real action first, so there is something a flood could have destroyed.
    let cookie = login(&a.app, PASSWORD).await.expect("test password works");
    let res = post_json(&a.app, "/api/lock", Some(&cookie), json!({})).await;
    assert!(
        res.status().is_success(),
        "precondition: the lock was issued"
    );

    for _ in 0..6 {
        let _ = post_json(
            &a.app,
            "/login",
            None,
            json!({ "password": "wrong-wrong-wrong" }),
        )
        .await;
    }

    let actions = std::fs::read_to_string(&a.path).unwrap_or_default();
    let attempts = std::fs::read_to_string(&attempts_path).unwrap_or_default();

    assert!(
        actions.contains("lock_issued"),
        "the action log should hold the lock"
    );
    assert!(
        !actions.contains("auth_failure"),
        "a wrong password was appended to the log the parent's history lives in:\n{actions}"
    );
    assert!(
        attempts.contains("auth_failure"),
        "the attempt was not recorded anywhere — ASVS 16.3.1 requires every unsuccessful \
         authentication to be logged"
    );
}

/// Splitting the file must not hide the attempts from the parent — the flood is itself the signal.
#[tokio::test]
async fn the_parent_still_sees_both_the_action_and_the_attempts() {
    let a = app_with_audit_file("partition-view");

    let cookie = login(&a.app, PASSWORD).await.expect("test password works");
    let _ = post_json(&a.app, "/api/lock", Some(&cookie), json!({})).await;
    for _ in 0..6 {
        let _ = post_json(
            &a.app,
            "/login",
            None,
            json!({ "password": "wrong-wrong-wrong" }),
        )
        .await;
    }

    let rows = body_json(get(&a.app, "/api/audit", Some(&cookie)).await).await;
    let events: Vec<&str> = rows
        .as_array()
        .expect("the audit endpoint returns a list")
        .iter()
        .filter_map(|r: &Value| r["event"].as_str())
        .collect();

    assert!(
        events.contains(&"lock_issued"),
        "the parent's action vanished from the view: {events:?}"
    );
    let failures = events.iter().filter(|e| **e == "auth_failure").count();
    assert_eq!(
        failures, 5,
        "the limiter admits five attempts before locking out and every one must be visible; \
         the sixth is refused before Argon2 and is deliberately not recorded. Saw: {events:?}"
    );
}
