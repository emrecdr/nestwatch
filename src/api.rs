//! The `/api/*` handlers. Each one offloads its blocking OS call to a `spawn_blocking`
//! worker so the async runtime stays responsive, then maps the result into a response.
//! All routes here sit behind the `require_auth` middleware.

use std::net::SocketAddr;

use axum::Json;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use tower_sessions::Session;

use crate::timereq::{MAX_REQUEST_MINUTES, PendingRequest};

use crate::config::Config;
use crate::control::{ProcessInfo, SystemControl};
use crate::curfew::Curfew;
use crate::error::AppError;
use crate::state::AppState;

/// Run a blocking `SystemControl` call on the blocking thread pool.
async fn blocking<T, F>(control: std::sync::Arc<dyn SystemControl>, f: F) -> Result<T, AppError>
where
    T: Send + 'static,
    F: FnOnce(&dyn SystemControl) -> Result<T, crate::control::ControlError> + Send + 'static,
{
    tokio::task::spawn_blocking(move || f(control.as_ref()))
        .await?
        .map_err(AppError::from)
}

/// Offload a blocking closure (file I/O, password hashing, log reads) to the blocking pool.
/// Sibling of [`blocking`] for work that doesn't take a `SystemControl`; a `JoinError` maps to
/// `AppError` via its `From` impl.
async fn spawn<T, F>(f: F) -> Result<T, AppError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    tokio::task::spawn_blocking(f).await.map_err(AppError::from)
}

/// Mutate the single-source [`Config`] and persist it off the async runtime.
///
/// SAFETY/ORDERING: the std `RwLock` write guard is dropped at the end of the inner block —
/// BEFORE any `.await` — so it never crosses an await point (which would trip clippy's
/// `await_holding_lock` and make the future `!Send`). We apply `mutate`, clone the whole
/// `Config` out under the guard, release the lock, then save the owned snapshot on a blocking
/// thread. Callers should `validate()` before calling.
///
/// That release is also why `config_save_lock` is held for the whole function. Dropping the
/// `RwLock` before the await is required, but it means two handlers can interleave as
/// mutate-A, mutate-B, save-B, save-A — and the last write wins on disk, so A's older snapshot
/// silently reverts B's change at the next restart while memory still shows both. Holding the
/// async mutex across mutate *and* save makes the pair atomic, so disk order matches the order
/// the parent actually made the changes in.
///
/// The two locks nest in one direction only: `config_save_lock` is taken first and the
/// `RwLock` inside it. No path takes them the other way round, so they cannot deadlock.
async fn update_config<F>(state: &AppState, mutate: F) -> Result<(), AppError>
where
    F: FnOnce(&mut Config),
{
    try_update_config(state, |c| {
        mutate(c);
        Ok(())
    })
    .await
}

/// [`update_config`] for a mutation that can reject the request (a cap, a validation that needs
/// to see the current config). This is the **only** place config is mutated and persisted, so a
/// handler cannot accidentally write config without taking `config_save_lock` — `save_routine`
/// previously hand-rolled this block because it needed a fallible mutation, which is exactly the
/// kind of second write path that makes serialization look done while leaving a hole.
///
/// `mutate` must leave the config unchanged when it returns `Err`: on that path the guard is
/// dropped without saving, so a partial change would live in memory and not on disk.
async fn try_update_config<F>(state: &AppState, mutate: F) -> Result<(), AppError>
where
    F: FnOnce(&mut Config) -> Result<(), AppError>,
{
    let _persist = state.config_save_lock.lock().await;
    let snapshot = {
        let mut guard = crate::state::recover_write(&state.config);
        mutate(&mut guard)?;
        guard.clone()
    };
    spawn(move || snapshot.save())
        .await?
        .map_err(AppError::Internal)
}

/// Query for [`screenshot`]. Absent means [`ShotTier::Full`] — see `ShotTier::from_arg`.
#[derive(Deserialize)]
pub struct ShotQuery {
    tier: Option<String>,
}

/// `GET /api/screenshot?tier=preview|full` → JPEG image of the primary monitor.
///
/// The tier is chosen by **who asked**, not by a control the parent sets: the dashboard's live
/// timer requests `preview`, and the two buttons a person presses request `full`. Defaulting to
/// `full` keeps this endpoint behaving as it always has for anything that does not ask.
pub async fn screenshot(
    State(state): State<AppState>,
    Query(q): Query<ShotQuery>,
) -> Result<Response, AppError> {
    let tier = crate::control::ShotTier::from_arg(q.tier.as_deref());
    let bytes = blocking(state.control.clone(), move |c| c.screenshot(tier)).await?;

    // Full captures are audited one for one: there are few, a person asked for each, and they are
    // the ones that make text legible. Preview frames arrive on a timer and are coalesced, because
    // a per-frame line evicts the entire security history in about 57 hours of live viewing.
    match tier {
        crate::control::ShotTier::Full => state.audit.record("screenshot_taken", json!({})),
        crate::control::ShotTier::Preview => {
            if let Some(frames) = state
                .live_audit
                .observe(std::time::Instant::now(), crate::audit::LIVE_AUDIT_WINDOW)
            {
                state.audit.record("live_view", json!({ "frames": frames }));
            }
        }
    }

    Ok(([(header::CONTENT_TYPE, crate::control::SHOT_MIME)], bytes).into_response())
}

/// `GET /api/processes` → JSON array of running processes.
pub async fn list_processes(
    State(state): State<AppState>,
) -> Result<Json<Vec<ProcessInfo>>, AppError> {
    let list = blocking(state.control.clone(), |c| c.list_processes()).await?;
    Ok(Json(list))
}

/// `POST /api/processes/{pid}/kill` → terminate a process.
pub async fn kill_process(
    State(state): State<AppState>,
    Path(pid): Path<u32>,
) -> Result<Json<Value>, AppError> {
    blocking(state.control.clone(), move |c| c.kill_process(pid)).await?;
    state.audit.record("process_kill", json!({ "pid": pid }));
    Ok(Json(json!({ "ok": true, "pid": pid })))
}

/// `POST /api/shutdown` → begin machine shutdown (short delay so the response is sent).
pub async fn shutdown(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    blocking(state.control.clone(), |c| {
        c.shutdown(5, Some("Shutting down (remote request)".into()))
    })
    .await?;
    state.audit.record("shutdown_issued", json!({}));
    Ok(Json(json!({ "ok": true })))
}

/// `POST /api/lock` → lock the screen (softer than shutdown; password to resume).
pub async fn lock(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    blocking(state.control.clone(), |c| c.lock_workstation()).await?;
    state.audit.record("lock_issued", json!({}));
    Ok(Json(json!({ "ok": true })))
}

/// `GET /api/curfew` → the current curfew settings.
pub async fn get_curfew(State(state): State<AppState>) -> Json<Curfew> {
    Json(crate::state::recover_read(&state.config).curfew.clone())
}

/// `POST /api/curfew` → validate, persist, and hot-apply new curfew settings.
pub async fn set_curfew(
    State(state): State<AppState>,
    Json(new_curfew): Json<Curfew>,
) -> Result<Json<Value>, AppError> {
    new_curfew.validate().map_err(AppError::BadRequest)?;
    let audit_fields =
        json!({ "enabled": new_curfew.enabled, "start": new_curfew.start, "end": new_curfew.end });
    update_config(&state, |c| c.curfew = new_curfew).await?;
    state.audit.record("curfew_change", audit_fields);
    Ok(Json(json!({ "ok": true })))
}

/// `GET /api/audit` → the most recent security-audit events (newest first), so the parent can
/// see logins and their source IP. Read-only; behind `require_auth` like the rest of `/api`.
pub async fn audit(State(state): State<AppState>) -> Result<Json<Vec<Value>>, AppError> {
    let audit = state.audit.clone();
    let events = spawn(move || audit.recent(200)).await?;
    Ok(Json(events))
}

/// `GET /api/usage` → the most recent usage-history events (newest first): daily screen-time,
/// sessions, and enforcement actions. Read-only; behind `require_auth`.
pub async fn usage(State(state): State<AppState>) -> Result<Json<Vec<Value>>, AppError> {
    let usage = state.usage.clone();
    let events = spawn(move || usage.recent(200)).await?;
    Ok(Json(events))
}

/// Query for [`screentime`]. `days` is optional so `/api/screentime` alone is valid.
#[derive(Deserialize)]
pub struct ScreentimeQuery {
    days: Option<u32>,
}

/// `GET /api/screentime?days=N` → the daily screen-time report: one entry per day for the last
/// `N` completed days, newest last, plus totals and a comparison against the preceding window.
///
/// `days` defaults to 30 — matching what commercial screen-time tools retain, and erring toward
/// keeping less of a child's data — and is clamped to 1..=365 so one request cannot ask for
/// unbounded work. Read-only; behind `require_auth`.
pub async fn screentime(
    State(state): State<AppState>,
    Query(q): Query<ScreentimeQuery>,
) -> Result<Json<crate::screentime::Report>, AppError> {
    let days = q.days.unwrap_or(30).clamp(1, 365);
    let today = crate::config::today();
    let screentime = state.screentime.clone();
    let usage = state.usage.clone();
    let report = spawn(move || {
        let rows = crate::screentime::history_rows(&screentime, &usage);
        crate::screentime::build_report(&rows, today, days)
    })
    .await?;
    Ok(Json(report))
}

#[derive(Deserialize)]
pub struct ExtraTimeBody {
    minutes: u32,
}

/// `POST /api/extra-time` → grant bonus minutes to today's budget directly, parent-initiated
/// (no child request needed). Uses the exact same `DailyGrant` mechanism as approving a time
/// request, so a mid-day reboot keeps the grant and it resets tomorrow.
pub async fn extra_time(
    State(state): State<AppState>,
    Json(body): Json<ExtraTimeBody>,
) -> Result<Json<Value>, AppError> {
    if body.minutes == 0 || body.minutes > MAX_REQUEST_MINUTES {
        return Err(AppError::BadRequest("minutes out of range".into()));
    }
    let today = crate::config::today();
    let minutes = body.minutes;
    update_config(&state, |c| c.extra.add(today, minutes)).await?;
    state.audit.record(
        "extra_time_granted",
        json!({ "minutes": minutes, "source": "parent" }),
    );
    state.usage.record(
        "extra_time_granted",
        json!({ "minutes": minutes, "source": "parent" }),
    );
    Ok(Json(json!({ "ok": true, "minutes": minutes })))
}

/// `GET /api/usage/today` → today's live screen-time tally: minutes used/remaining against the
/// effective budget (base + granted extra) plus per-app usage for apps that have a limit. The
/// numbers come from the enforcer's persisted sidecar (up to one 30s tick behind live).
pub async fn usage_today(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let today = crate::config::today();
    let (rules, extra) = {
        let cfg = crate::state::recover_read(&state.config);
        (cfg.rules.clone(), cfg.extra.for_day(today))
    };
    let usage = spawn(move || crate::rules::Usage::load_for_today(today)).await?;
    // Read the enforcer heartbeat here rather than inside the summary: it touches process
    // globals and the system clock, and keeping it at the edge is what lets `today_summary`
    // be tested in full.
    let enforcer_age_secs = crate::heartbeat::worst_age_secs();
    Ok(Json(crate::rules::today_summary(
        &rules,
        today,
        extra,
        &usage,
        enforcer_age_secs,
    )))
}

/// `GET /status` → the child's own screen-time figures, for the `/ask` page.
///
/// **Unauthenticated** (LAN-gated like the rest of the child surface) and deliberately narrow:
/// only the totals the child is already entitled to know — how long they've been on and how much
/// is left. It exposes no blocklist, no per-app limits, no app groups, no curfew window, no
/// request queue, and nothing about *why* a limit exists, so it can't be used to map the rules
/// and plan around them.
///
/// Why it exists: without it the child's only way to answer "how much time do I have?" is to
/// interrupt the parent, which is exactly the interruption the request queue is meant to remove.
/// A child who can see the number is also far less likely to feel ambushed by a lock.
///
/// Throttled like its siblings: every call does a file read + JSON parse on the blocking pool
/// (shared with screenshots and config writes), and this is a route the child is deliberately
/// pointed at — an unthrottled `setInterval(()=>fetch('/status'))` would let them make the
/// parent's dashboard unresponsive on demand. The `/ask` page polls once a minute, so the
/// allowance is generous.
pub async fn child_status(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
) -> Result<Json<Value>, AppError> {
    state
        .status_limiter
        .count_and_check(peer.ip(), std::time::Instant::now())?;
    let today = crate::config::today();
    // Only the two numbers, computed under the guard — cloning `Rules` here would deep-copy the
    // blocklist, app limits and groups just to read a budget off them.
    let (enabled, budget) = {
        let cfg = crate::state::recover_read(&state.config);
        let extra = cfg.extra.for_day(today);
        (
            cfg.rules.enabled,
            cfg.rules.effective_budget_mins(today, extra),
        )
    };
    let usage = spawn(move || crate::rules::Usage::load_for_today(today)).await?;

    // Clamped, via the shared helper — `as u32` here used to wrap a corrupt tally to a small
    // number, telling the child they had plenty of time left.
    let remaining = usage.remaining_mins(budget).unwrap_or(0);
    let used = u32::try_from(usage.total_secs / 60).unwrap_or(u32::MAX);
    Ok(Json(json!({
        // `false` when enforcement is paused or no budget applies today — the page then says
        // "no limit today" rather than showing a meaningless 0.
        "limited": enabled && budget > 0,
        "budget_mins": budget,
        "used_mins": used,
        "remaining_mins": remaining,
    })))
}

/// `GET /api/rules` → the current usage rules (budget, blocklist, per-app limits).
pub async fn get_rules(State(state): State<AppState>) -> Json<crate::rules::Rules> {
    Json(crate::state::recover_read(&state.config).rules.clone())
}

/// `POST /api/rules` → validate, persist, and hot-apply new usage rules.
pub async fn set_rules(
    State(state): State<AppState>,
    Json(new_rules): Json<crate::rules::Rules>,
) -> Result<Json<Value>, AppError> {
    new_rules.validate().map_err(AppError::BadRequest)?;
    let audit_fields = json!({
        "daily_budget_mins": new_rules.daily_budget_mins,
        "blocklist_count": new_rules.blocklist.len(),
        "app_limits_count": new_rules.app_limits.len(),
        "budget_action": new_rules.budget_action,
    });
    update_config(&state, |c| c.rules = new_rules).await?;
    state.audit.record("rules_change", audit_fields);
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct SaveRoutineBody {
    name: String,
    rules: crate::rules::Rules,
}

/// `GET /api/routines` → the saved routine names (newest-last, as stored).
pub async fn list_routines(State(state): State<AppState>) -> Json<Vec<String>> {
    let names = crate::state::recover_read(&state.config)
        .routines
        .iter()
        .map(|r| r.name.clone())
        .collect();
    Json(names)
}

/// `POST /api/routines` → save (upsert) a named preset of the given rules.
pub async fn save_routine(
    State(state): State<AppState>,
    Json(body): Json<SaveRoutineBody>,
) -> Result<Json<Value>, AppError> {
    let name = body.name.trim().to_string();
    if name.is_empty() || name.chars().count() > crate::config::MAX_ROUTINE_NAME {
        return Err(AppError::BadRequest("invalid routine name".into()));
    }
    body.rules.validate().map_err(AppError::BadRequest)?;
    let rules = body.rules;

    // Cap check + upsert under a single write guard (no TOCTOU between checking the count and
    // pushing). Updating an existing routine is always allowed; only a brand-new one can hit the
    // cap, and it is rejected before anything is pushed, so the `Err` path leaves config
    // untouched as `try_update_config` requires.
    try_update_config(&state, |cfg| {
        match cfg.routines.iter_mut().find(|r| r.name == name) {
            Some(existing) => existing.rules = rules,
            None => {
                if cfg.routines.len() >= crate::config::MAX_ROUTINES {
                    return Err(AppError::BadRequest("too many routines".into()));
                }
                cfg.routines.push(crate::config::Routine {
                    name: name.clone(),
                    rules,
                });
            }
        }
        Ok(())
    })
    .await?;
    state.audit.record("routine_saved", json!({ "name": name }));
    Ok(Json(json!({ "ok": true })))
}

/// `POST /api/routines/{name}/apply` → copy the routine's rules into the live rules (hot-applied
/// by the enforcer next tick).
pub async fn apply_routine(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, AppError> {
    let rules = crate::state::recover_read(&state.config)
        .routines
        .iter()
        .find(|r| r.name == name)
        .map(|r| r.rules.clone());
    let Some(rules) = rules else {
        return Err(AppError::BadRequest("no such routine".into()));
    };
    // Apply the routine's rule *content* but preserve the current pause state: the enforcing/paused
    // toggle is a temporary override, not something a "Homework"/"Weekend" preset should flip.
    update_config(&state, |c| {
        let paused = !c.rules.enabled;
        c.rules = rules;
        c.rules.enabled = !paused;
    })
    .await?;
    state
        .audit
        .record("routine_applied", json!({ "name": name }));
    Ok(Json(json!({ "ok": true })))
}

/// `POST /api/routines/{name}/delete` → remove a saved routine (does not touch the live rules).
pub async fn delete_routine(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, AppError> {
    update_config(&state, |c| c.routines.retain(|r| r.name != name)).await?;
    state
        .audit
        .record("routine_deleted", json!({ "name": name }));
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct TimeReqBody {
    minutes: u32,
    #[serde(default)]
    reason: String,
}

/// `POST /time-request` — the child asks for extra minutes. **Unauthenticated** (the child
/// isn't logged in) but LAN-gated (outer router → `require_lan_peer`) and per-IP rate-limited.
/// Returns only `{ok:true}` regardless of accept/reject, so it leaks nothing about the queue.
pub async fn time_request(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<TimeReqBody>,
) -> Result<Json<Value>, AppError> {
    let ip = peer.ip();
    state
        .time_req_limiter
        .count_and_check(ip, std::time::Instant::now())?;
    if body.minutes == 0 || body.minutes > MAX_REQUEST_MINUTES {
        return Err(AppError::BadRequest("minutes out of range".into()));
    }
    let requests = state.time_requests.clone();
    let accepted = spawn(move || requests.submit(body.minutes, &body.reason)).await?;
    // Only audit a submission that actually joined the queue. Recording rejections too made
    // audit volume a function of request volume from an unauthenticated caller: once the cap is
    // full every further attempt still wrote a line, so a loop at the throttle's 5/min appended
    // indefinitely and eventually rolled the log (and its single backup) off disk. Accepted
    // submissions are capped at MAX_PENDING until the parent resolves one, so this bounds audit
    // growth to parent actions. Fourth site of this defect — see `login`, `pair`, `logout`.
    if accepted.is_some() {
        state.audit.record(
            "time_request_submitted",
            json!({ "src_ip": ip, "minutes": body.minutes }),
        );
    }
    Ok(Json(json!({ "ok": true })))
}

/// `GET /api/time-requests` → the pending requests, newest first (parent-facing).
pub async fn list_time_requests(
    State(state): State<AppState>,
) -> Result<Json<Vec<PendingRequest>>, AppError> {
    let requests = state.time_requests.clone();
    let pending = spawn(move || requests.pending()).await?;
    Ok(Json(pending))
}

/// `POST /api/time-requests/{id}/approve` → grant the requested minutes to today's budget.
pub async fn approve_time_request(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let requests = state.time_requests.clone();
    let resolved = spawn(move || requests.resolve(&id, true)).await?;
    let Some(req) = resolved else {
        return Err(AppError::BadRequest("no such pending request".into()));
    };

    // Add the minutes to today's grant (the reset-if-not-today rule lives in DailyGrant).
    let today = crate::config::today();
    let minutes = req.minutes;
    update_config(&state, |c| c.extra.add(today, minutes)).await?;

    state
        .audit
        .record("time_request_approved", json!({ "minutes": minutes }));
    state.usage.record(
        "extra_time_granted",
        json!({ "minutes": minutes, "source": "request" }),
    );
    Ok(Json(json!({ "ok": true, "minutes": minutes })))
}

/// `POST /api/time-requests/{id}/deny` → reject a pending request.
pub async fn deny_time_request(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let requests = state.time_requests.clone();
    let resolved = spawn(move || requests.resolve(&id, false)).await?;
    if resolved.is_none() {
        return Err(AppError::BadRequest("no such pending request".into()));
    }
    state.audit.record("time_request_denied", json!({}));
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct IssueCodeBody {
    minutes: u32,
}

/// `POST /api/time-codes` → mint a single-use time code worth `minutes`, returned to the parent
/// to write down / hand to the child. Authenticated (parent-facing).
pub async fn issue_time_code(
    State(state): State<AppState>,
    Json(body): Json<IssueCodeBody>,
) -> Result<Json<Value>, AppError> {
    if body.minutes == 0 || body.minutes > crate::timecode::MAX_CODE_MINUTES {
        return Err(AppError::BadRequest("minutes out of range".into()));
    }
    let codes = state.time_codes.clone();
    let minutes = body.minutes;
    let code = spawn(move || codes.issue(minutes)).await?;
    let Some(code) = code else {
        return Err(AppError::BadRequest("too many active codes".into()));
    };
    // The code itself is a secret (it grants time), so it is NOT written to the audit log.
    state
        .audit
        .record("time_code_issued", json!({ "minutes": minutes }));
    Ok(Json(
        json!({ "ok": true, "code": code, "minutes": minutes }),
    ))
}

/// `GET /api/time-codes` → the active (unredeemed) codes, newest first. Authenticated.
pub async fn list_time_codes(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::timecode::ActiveCode>>, AppError> {
    let codes = state.time_codes.clone();
    let active = spawn(move || codes.active()).await?;
    Ok(Json(active))
}

#[derive(Deserialize)]
pub struct RedeemBody {
    code: String,
}

/// `POST /redeem-code` — the child cashes in a time code. **Unauthenticated** (the child isn't
/// logged in) but LAN-gated (outer router → `require_lan_peer`) and per-IP rate-limited (which
/// also blunts brute-forcing). On a valid code the minutes are added to today's budget; the
/// response reveals only whether it worked (and how many minutes), never anything else.
pub async fn redeem_code(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(body): Json<RedeemBody>,
) -> Result<Json<Value>, AppError> {
    state
        .code_limiter
        .count_and_check(peer.ip(), std::time::Instant::now())?;
    let codes = state.time_codes.clone();
    let input = body.code;
    let granted = spawn(move || codes.redeem(&input)).await?;
    let Some(minutes) = granted else {
        return Ok(Json(json!({ "ok": false })));
    };
    let today = crate::config::today();
    update_config(&state, |c| c.extra.add(today, minutes)).await?;
    state.audit.record(
        "time_code_redeemed",
        json!({ "src_ip": peer.ip(), "minutes": minutes }),
    );
    state.usage.record(
        "extra_time_granted",
        json!({ "minutes": minutes, "source": "code" }),
    );
    Ok(Json(json!({ "ok": true, "minutes": minutes })))
}

#[derive(Deserialize)]
pub struct PasswordChange {
    current: String,
    new: String,
}

/// `POST /api/password` → verify the current password, then set a new one (Argon2id re-hash,
/// persisted). Lets the parent rotate the password without re-running `install`.
///
/// Session policy: **all other devices are signed out**, and the caller's own session id is
/// rotated so they stay logged in. Sessions now persist across restarts, so a password change is
/// the only remaining way to revoke a leaked cookie before its 30-day expiry — and revoking one
/// is exactly what a parent expects "change the password" to do.
pub async fn change_password(
    State(state): State<AppState>,
    session: Session,
    Json(body): Json<PasswordChange>,
) -> Result<Json<Value>, AppError> {
    // Same checker and the same wording as `install`, so the dashboard and the console cannot
    // disagree about what makes a password acceptable — or describe the same rejection two ways.
    if let Err(problem) = crate::auth::check_password(&body.new) {
        return Err(AppError::BadRequest(problem.message()));
    }

    // Verify the current password off the async runtime (Argon2 is memory-hard).
    let current_hash = crate::state::recover_read(&state.config)
        .password_hash
        .clone();
    let candidate = body.current;
    let ok = spawn(move || crate::auth::verify_password(&candidate, &current_hash)).await?;
    if !ok {
        state
            .audit
            .record("password_change_failed", json!({ "reason": "bad_current" }));
        return Err(AppError::Unauthorized);
    }

    // Hash the new password off the runtime, then persist via the single-source helper.
    let new_pw = body.new;
    let new_hash = spawn(move || crate::auth::hash_password(&new_pw))
        .await?
        .map_err(AppError::Internal)?;
    update_config(&state, |c| c.password_hash = new_hash).await?;

    // Sign every device out. Changing the password is what a parent does when they think a
    // session may be compromised, so it must actually revoke one. This matters more now that
    // sessions survive restarts: before, a service restart cleared them implicitly; today a
    // leaked cookie would otherwise stay valid for its full 30 days with no way to kill it.
    // `cycle_id` afterwards re-creates a record for the caller, so the parent stays signed in
    // on a brand-new id.
    state.sessions.clear_all();
    session.cycle_id().await?;
    state.audit.record("password_changed", json!({}));
    Ok(Json(json!({ "ok": true })))
}
