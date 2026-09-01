//! The `/api/*` handlers. Each one offloads its blocking OS call to a `spawn_blocking`
//! worker so the async runtime stays responsive, then maps the result into a response.
//! All routes here sit behind the `require_auth` middleware.

use std::net::SocketAddr;

use axum::Json;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::HeaderName;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use tower_sessions::Session;

use crate::timereq::{MAX_REQUEST_MINUTES, PendingRequest};

use crate::audit::{LIVE_VIEW_EVENT, SCREENSHOT_EVENT};
use crate::config::Config;
use crate::control::{ProcessInfo, SHOT_MIME, ShotTier, SystemControl};
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
        .map_err(AppError::Internal)?;
    // After the save, so a woken enforcer reads the same config that reached disk. Every parent
    // action that can invalidate a pending shutdown flows through here, which is why the wake
    // lives at this choke point rather than in the four handlers that need it.
    crate::heartbeat::wake(&state.enforcement_wake);
    Ok(())
}

/// Query for [`screenshot`]. Absent means [`ShotTier::Full`] — see `ShotTier::from_arg`.
#[derive(Deserialize)]
pub struct ShotQuery {
    tier: Option<String>,
    /// Present when the **live timer** asked, absent when a person did.
    ///
    /// Separate from `tier` because they answer different questions, and conflating them is what
    /// broke: the audit used tier as a proxy for "who asked", which held only while the timer
    /// always requested previews. It no longer does — live frames follow the surface on screen, so
    /// the timer requests `full` whenever the full-size view is open.
    live: Option<String>,
}

/// `GET /api/screenshot?tier=preview|full` → JPEG image of the primary monitor.
///
/// The tier is chosen by **which surface is showing the frame**, not by a control the parent sets:
/// the thumbnail cannot use more than a preview holds, while the full-size view exists so a parent
/// can read what is on screen. Defaulting to `full` keeps this endpoint behaving as it always has
/// for anything that does not ask.
///
/// `live=1` is a separate question — *who* asked — and only the audit reads it. See below.
pub async fn screenshot(
    State(state): State<AppState>,
    Query(q): Query<ShotQuery>,
) -> Result<Response, AppError> {
    let tier = ShotTier::from_arg(q.tier.as_deref());
    let bytes = blocking(state.control.clone(), move |c| c.screenshot(tier)).await?;

    // Audited by **who asked**, never by tier.
    //
    // Captures a person asked for are few and each is bounded by a human action, so one line each
    // stays readable and cannot run away. Timer frames are coalesced into a periodic `live_view`
    // line carrying a count.
    //
    // This used to switch on `tier`, which worked only while the timer always asked for previews.
    // Once live frames began following the visible surface — full while the full-size view is open
    // — that proxy silently became ~1,800 one-for-one lines an hour at the fastest cadence a
    // parent can select. `audit.jsonl` rotates at 2 MiB
    // and keeps one backup, so it would evict every login, kill and password change to make room
    // for a timer: exactly the failure the coalescer exists to prevent, arriving through the other
    // tier.
    // Both names come from `audit`, which also reads them back to count how often the child was
    // looked at. Named constants rather than literals, because a rename here that missed the
    // reader would leave that count reading zero forever with nothing failing.
    if q.live.is_some() {
        if let Some(frames) = state.live_audit.observe(std::time::Instant::now()) {
            state
                .audit
                .record(LIVE_VIEW_EVENT, json!({ "frames": frames }));
        }
    } else {
        state.audit.record(SCREENSHOT_EVENT, json!({}));
    }

    // Name the tier actually served, so the client records what it *got* rather than what it
    // asked for. `ShotTier::from_arg` maps unknown and absent alike to `Full`, so a typo in the
    // query string returns a full frame on a two-second timer while the caller believes it is
    // getting previews — the failure `as_arg`'s doc describes as "no error, no failing test, just
    // the cost back". It is also what makes the overlay's "is the frame on screen already full?"
    // check an answer instead of an assumption.
    Ok((
        [
            (header::CONTENT_TYPE, SHOT_MIME),
            (HeaderName::from_static("x-shot-tier"), tier.as_arg()),
        ],
        bytes,
    )
        .into_response())
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
    // Carry tonight's extension across the save. The dashboard's curfew form does not send
    // `extra_until` — it is transient state, not a setting — so it deserializes to `None`, and
    // assigning the whole struct would silently revoke an extension a parent granted minutes
    // earlier. The nearest existing precedent is `port` and `password_hash`, which no handler
    // writes; this one has to be actively preserved rather than merely not written.
    update_config(&state, |c| {
        let mut next = new_curfew;
        next.extra_until = c.curfew.extra_until;
        c.curfew = next;
    })
    .await?;
    state.audit.record("curfew_change", audit_fields);
    Ok(Json(json!({ "ok": true })))
}

/// `GET /api/audit` → the most recent security-audit events (newest first), so the parent can
/// see logins and their source IP. Read-only; behind `require_auth` like the rest of `/api`.
pub async fn audit(State(state): State<AppState>) -> Result<Json<Vec<Value>>, AppError> {
    let audit = state.audit.clone();
    let events =
        spawn(move || audit.recent(crate::audit::AUDIT_VIEW, crate::audit::ATTEMPT_VIEW)).await?;
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
pub struct LanguageBody {
    language: String,
}

/// Reject a minutes value outside `1..=max`, naming the bound in the message.
///
/// Three handlers spelled this out, two of them character-for-character including the comment
/// below, differing only in which constant bounds them.
///
/// Carry the bound, not just the verdict. `Rules::validate`'s five messages already do ("daily
/// limit must be <= 10080 minutes"), and a parent told only that they broke a limit has to guess
/// which number to try next. That wording is the reason this exists, so it lives in one place
/// rather than three that can drift apart.
fn require_minutes(minutes: u32, max: u32) -> Result<(), AppError> {
    if minutes == 0 || minutes > max {
        return Err(AppError::BadRequest(format!(
            "minutes must be between 1 and {max}"
        )));
    }
    Ok(())
}

/// Tell any open dashboard that `tag` went stale, so it can refetch that endpoint now rather than
/// at its next minute boundary.
///
/// Best-effort by construction: `send` errors only when nobody is subscribed, which is the normal
/// state, so the result is discarded. A handler must never fail because no dashboard is open.
///
/// The tags are a closed set, matching the `addEventListener` names in `app.js`. Kept as a function
/// rather than inlined at each call site so a new one has to be added here, where the client's
/// listeners are named in the doc, instead of being invented at a handler and silently ignored.
///
/// * `requests` — the pending time-request queue changed.
/// * `usage` — today's minutes or budget changed.
fn notify(state: &AppState, tag: &'static str) {
    debug_assert!(
        matches!(tag, "requests" | "usage"),
        "unknown event tag {tag:?} — add it to app.js's listeners first"
    );
    let _ = state.events.send(tag);
}

/// `GET /api/events` → a Server-Sent Events stream naming what has changed.
///
/// # What this is for
///
/// The dashboard polls `/api/usage/today` and `/api/time-requests` once a minute. That sets the
/// floor on the one interaction here where somebody is actually waiting: a child asks for more
/// time and their parent's phone finds out up to sixty seconds later. Everything else the
/// dashboard shows can wait a minute; this cannot, because a person is sitting in front of it.
///
/// # What it deliberately is not
///
/// It carries **no data** — only a tag naming which endpoint went stale. The client refetches the
/// route it already used, so every JSON route stays the single source of truth and this cannot
/// drift from them or grow a second serialisation to keep in step. Losing an event costs one
/// slower refresh, never a wrong number: the 60-second poll stays exactly as it was, as the
/// backstop for a dropped stream, a sleeping phone, or a browser that never opened one.
///
/// Parent-side only. The child's page would benefit from the same immediacy when a request is
/// answered, but that page is unauthenticated by design, and an unauthenticated endpoint holding
/// a connection open per client is a denial-of-service surface this LAN service does not need to
/// grow. The child keeps their poll.
pub async fn events(State(state): State<AppState>) -> Response {
    use axum::response::sse::{Event, KeepAlive, Sse};

    let rx = state.events.subscribe();
    type Ev = Result<Event, std::convert::Infallible>;
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        match rx.recv().await {
            Ok(tag) => {
                let ev: Ev = Ok(Event::default().event(tag).data("1"));
                Some((ev, rx))
            }
            // Lagged: this subscriber fell behind and lost tags. Tell it to refetch everything
            // rather than ending the stream — the cost of an extra reload beats a dashboard that
            // silently stopped updating.
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                let ev: Ev = Ok(Event::default().event("all").data("1"));
                Some((ev, rx))
            }
            // The sender lives in `AppState` for the process lifetime, so this is unreachable in
            // practice; ending the stream lets the client's own reconnect handle it.
            Err(tokio::sync::broadcast::error::RecvError::Closed) => None,
        }
    });

    // Without the keep-alive, a connection idle for minutes — the normal state — can be dropped by
    // the OS or the browser, and the client would only find out at its next reconnect.
    //
    // `no-store` is set explicitly, overriding axum's own `no-cache`. `security.rs` fills in
    // `no-store` only where a handler has not already spoken, and `Sse` speaks — so this route
    // would otherwise have been the single `/api/*` response that permits storing, quietly, on the
    // strength of a default nobody here chose. The stream carries no data, only tags, so little
    // rides on it; the value is that "nothing under `/api` is ever stored" stays a rule without an
    // exception, rather than a rule with one that has to be remembered.
    let mut res = Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response();
    res.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    res
}

/// `GET /api/language` → the language the child's surfaces currently speak.
///
/// The dashboard needs this to show which option is selected. It is also on `/status`, but that is
/// the child's own endpoint and the parent's page should not have to read the child's to learn a
/// setting the parent owns.
pub async fn get_language(State(state): State<AppState>) -> Json<Value> {
    let language = crate::state::recover_read(&state.config).language;
    Json(json!({ "language": language.tag() }))
}

/// `POST /api/language` → set the language the **child's** surfaces speak.
///
/// Parent-authenticated for the reason in [`crate::config::Language`]: the child does not choose
/// the language of their own disclosure notice. Rejects a tag this build has no strings for rather
/// than falling back to English, which would look identical to the setting not having saved.
pub async fn set_language(
    State(state): State<AppState>,
    Json(body): Json<LanguageBody>,
) -> Result<Json<Value>, AppError> {
    let Some(language) = crate::config::Language::from_tag(&body.language) else {
        return Err(AppError::BadRequest(format!(
            "unsupported language {:?} — this build has strings for: en, nl",
            body.language
        )));
    };
    update_config(&state, move |c| c.language = language).await?;
    state
        .audit
        .record("language_changed", json!({ "language": language.tag() }));
    Ok(Json(json!({ "ok": true, "language": language.tag() })))
}

/// `POST /api/re-anchor` → re-record the trusted clock against this machine's *current* time zone.
///
/// # Why this is a route and not a CLI command
///
/// The anchor and the zone identity are recorded at install, and until now that was the only way to
/// set them. The README's instruction for a house move is "re-run `install`" — an elevated console,
/// physically at the child's PC, for a setting that has nothing to do with installing. A parent who
/// does not know the rule gets something worse than inconvenience: the service keeps enforcing
/// against the old zone, so the curfew is silently an hour or two out, in the direction the child
/// benefits from, with nothing on screen saying so.
///
/// A CLI command would not have helped. Writing `config.json` needs elevation because the data
/// directory is ACL-locked, so `nestwatch re-anchor` costs exactly what re-running `install` costs.
/// The service is already SYSTEM, so asking *it* to re-record needs no elevation at all — only the
/// session the child cannot obtain.
///
/// # Why it is safe to expose
///
/// This is the one operation where "the child could reach it" would be fatal, since re-anchoring to
/// a zone they just chose would launder the tamper into the trusted state. It sits behind
/// `require_auth` with everything else, so reaching it costs the parent's password; the child's
/// unauthenticated surfaces are a separate router. It is audited, and it records what it moved from
/// and to, because "the anchor changed" is exactly the line you want when a curfew starts behaving
/// oddly a month later.
pub async fn re_anchor(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let offset = crate::clock::current_offset_mins();
    let zone = crate::clock::current_zone_identity();

    let (was_offset, was_zone) = {
        let cfg = crate::state::recover_read(&state.config);
        (cfg.tz_offset_mins, cfg.tz_zone.clone())
    };

    let zone_for_cfg = zone.clone();
    update_config(&state, move |c| {
        c.tz_offset_mins = Some(offset);
        c.tz_zone = zone_for_cfg;
    })
    .await?;

    // Apply immediately. Without this the enforcers keep the old anchor until the service restarts,
    // so the parent would press the button, see it succeed, and watch the curfew stay wrong.
    crate::clock::set_anchor(offset);
    crate::clock::set_anchor_zone(zone.clone());

    state.audit.record(
        "clock_reanchored",
        json!({
            "from_offset_mins": was_offset,
            "to_offset_mins": offset,
            "from_zone": was_zone,
            "to_zone": zone,
        }),
    );

    Ok(Json(json!({
        "ok": true,
        "offset_mins": offset,
        "zone": zone,
    })))
}

/// `GET /api/export` → every screen-time rollup this install holds, as a downloadable file.
///
/// # Why this exists
///
/// The whole history lives in JSONL under a data directory `install` ACL-locks to SYSTEM and
/// Administrators. The dashboard renders slices of it and nothing offered it as a file, so for a
/// product whose thesis is that your data stays yours, "yours" meant "in a directory you need an
/// admin shell to reach, on a machine you may be replacing". `uninstall --purge` is documented as
/// irreversible — "every day of recorded screen time" — and had no escape hatch attached.
///
/// # Why it is a faithful dump and not the report
///
/// [`crate::screentime::build_report`] collapses duplicate dates by preferring the richer row, and
/// that rule is private to it. Restating it here would put one fact in two places — the defect
/// class this codebase keeps finding — and the two would drift the first time either changed.
/// It would also inherit `build_report`'s 365-day clamp and its exclusion of today, both correct
/// for a chart and both silent data loss in an export.
///
/// # What it cannot recover
///
/// There is no retention policy here, only rotation, and rotation destroys. `jsonl.rs` renames the
/// live file to `.1` once it passes 2 MiB and **clobbers any existing `.1`**, so each log keeps two
/// generations and everything older was deleted at rotation time, silently, long before anyone runs
/// `--purge`. An export cannot bring that back and must not imply otherwise.
///
/// **The 4 MiB across two generations is the fact. How many days that buys is a model.** A daily
/// row's size is set by how many apps, pages and groups the child used, so the horizon is a
/// property of the household rather than of the tool. Two independent estimates, from different
/// assumed name and title lengths, agreed on shape and differed by about 40%:
///
/// | a day with | bytes/day | days held |
/// |---|---|---|
/// | 2 apps, 1 page | 303–475 | ~8,800–13,800 (decades) |
/// | 8 apps, 10 pages | 1,114–1,537 | ~2,700–3,800 |
/// | 40 apps, 40 pages (`MAX_PAGES`) | 4,600–6,101 | ~690–910 (**~2–2.5 yr**) |
///
/// Quote it as a range with the assumption named, never as a figure: a parent will otherwise read
/// "7.5 years" as something the product guarantees.
///
/// One asymmetry, and it is narrower than it first looks. `usage.jsonl` carries the rollups among
/// session starts, stops, locks, warnings and grants, so it rotates far sooner and its copy of a
/// given day dies first — but `screentime.jsonl` holds nothing but rollups, so this only affects
/// installs predating that file. It is still why [`crate::screentime::history_rows`] reads both:
/// for those installs, reading one returns a partial history that looks complete.
///
/// So this returns the stored rows verbatim, from the same reader the report uses
/// ([`crate::screentime::history_rows`], which covers the rotated backup and the legacy
/// `usage.jsonl`). Duplicate dates are possible and are *preserved deliberately*; the manifest says
/// so, because an export that quietly reconciles is an export you cannot check the tool against.
pub async fn export(State(state): State<AppState>) -> Result<Response, AppError> {
    let screentime = state.screentime.clone();
    let usage = state.usage.clone();
    let rollups = spawn(move || crate::screentime::history_rows(&screentime, &usage)).await?;

    // A deliberate parent action against the most complete record this tool holds, and one that
    // leaves the machine. Few, human-driven, and exactly what the audit log is for — unlike the
    // capture timer, no path here can reach this in a loop.
    state
        .audit
        .record("export", json!({ "rollups": rollups.len() }));

    let today = crate::config::today();
    let body = json!({
        "nestwatch_version": crate::VERSION,
        "exported_on": today.to_string(),
        "rollup_count": rollups.len(),
        // Said in the file rather than only in the docs, because the file is what outlives both.
        "note": "Daily screen-time rollups exactly as stored, newest first. Rows from an install \
    that predates screentime.jsonl come from usage.jsonl, so the same date can appear twice; the \
    dashboard prefers whichever row carries more detail. Nothing here is reconciled or filtered.",
        "rollups": rollups,
    });

    let filename = format!("nestwatch-history-{today}.json");
    Ok((
        [
            (header::CONTENT_TYPE, "application/json".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        Json(body),
    )
        .into_response())
}

#[derive(Deserialize)]
pub struct ExtraTimeBody {
    minutes: u32,
    /// Who is granting. Absent means the parent pressed the dashboard button;
    /// a robot names itself (`studygo`) so the audit log stays honest.
    #[serde(default)]
    source: Option<String>,
}

/// Longest accepted `source` token.
const MAX_SOURCE_LEN: usize = 32;

/// Most distinct non-`parent` sources that may grant on one day. Bounds
/// [`Config::earned`], which lives in the persisted config: a compromised
/// parent session must not be able to grow that file without limit.
const MAX_EARNED_SOURCES: usize = 16;

/// Is `source` an acceptable grant-source token?
///
/// A bounded lowercase token rather than an enum, so the server stays ignorant
/// of what pushes to it — the next signal source (a chores app, a reading log)
/// costs no server change. The charset matters because the value is written
/// verbatim into the audit log: nothing here can fake a line break, a quote,
/// or a look-alike of another event's fields.
fn valid_source(source: &str) -> bool {
    !source.is_empty()
        && source.len() <= MAX_SOURCE_LEN
        && source
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
}

/// `POST /api/curfew/extend` → push tonight's bedtime back by `minutes`, once.
///
/// The control that was missing. Screen time had three ways to grant more — the bonus buttons,
/// approving a request, and offline codes — and bedtime had none, so a parent who wanted to allow
/// a late finish could only edit the window itself, which is permanent until they change it back.
/// Approving a screen-time request looked like it would work and did not, because the two limits
/// are independent; that is the confusion `grant_shadowed_by_curfew` explains and this endpoint
/// removes.
///
/// **Extends from the later of "now" and any extension already running**, so pressing +30 twice
/// gives an hour rather than the second press swallowing the first. Bounded by
/// `MAX_REQUEST_MINUTES`, the same cap the screen-time grants use.
///
/// Deliberately parent-only, with nothing on the child's page: `/ask` does not reveal the curfew
/// schedule at all — a bedtime handed to the child is a map for planning around it — so it does
/// not reveal an extension either. They simply notice the machine did not shut down.
pub async fn extend_curfew(
    State(state): State<AppState>,
    Json(body): Json<ExtraTimeBody>,
) -> Result<Json<Value>, AppError> {
    require_minutes(body.minutes, MAX_REQUEST_MINUTES)?;
    let minutes = body.minutes;
    // The trusted clock, so a child who changes the time zone cannot stretch the extension — the
    // same reading curfew itself enforces against.
    let now = crate::clock::now();
    let mut until = None;
    update_config(&state, |c| {
        let base = match c.curfew.extra_until {
            Some(t) if t > now => t,
            _ => now,
        };
        let t = base + chrono::Duration::minutes(i64::from(minutes));
        c.curfew.extra_until = Some(t);
        until = Some(t);
    })
    .await?;

    let until_local = until
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_default();
    state.audit.record(
        "curfew_extended",
        json!({ "minutes": minutes, "until": until_local }),
    );
    state.usage.record(
        "curfew_extended",
        json!({ "minutes": minutes, "until": until_local }),
    );
    Ok(Json(json!({
        "ok": true,
        "minutes": minutes,
        "until": until_local,
        // Read *after* the extension is stored, so the sentence describes the state the parent is
        // now in rather than the one they were in a moment ago.
        "budget_note": extension_shadowed_by_budget(&state, minutes).await,
    })))
}

/// What a parent should be told about a grant of `minutes` they have just made, given the curfew.
///
/// `None` when the grant will do what it looks like it does. Otherwise a plain sentence naming
/// what will actually happen, for the dashboard to show beside the confirmation.
///
/// **This exists because of a real evening.** A parent approved their child's request just after
/// 22:00 on a Saturday and the PC shut down anyway. Nothing was broken: screen time and bedtime
/// are independent limits, `curfew.rs` never reads the grant, and `should_abort_budget_shutdown`
/// deliberately refuses to cancel a shutdown while a window is open, so curfew stays the sole
/// authority over the one OS shutdown slot. Every part of that is correct and every part of it is
/// invisible — the handler returned `{"ok": true}` while the machine powered off underneath the
/// child. The information was available the whole time; nothing looked at it.
///
/// Reported rather than enforced: the grant still lands. Banking minutes during bedtime is a
/// reasonable thing to do on purpose (they survive to no later than midnight, when `DailyGrant`
/// resets), and refusing would be its own surprise. What must not happen is a promise being made
/// silently that the system cannot keep.
fn grant_shadowed_by_curfew(state: &AppState, minutes: u32) -> Option<String> {
    let curfew = crate::state::recover_read(&state.config).curfew.clone();
    // The trusted clock, not `Local::now()` — the same reading curfew enforces against, so this
    // cannot disagree with the enforcer about whether a window is open.
    let mins = curfew.cuts_grant_short_in(crate::clock::now(), minutes)?;
    Some(if mins == 0 {
        "Bedtime is in force now, so the PC will still shut down — screen time and bedtime are \
         separate limits. Use \"Later bedtime tonight\" on the Curfew card to move bedtime itself."
            .to_string()
    } else {
        format!(
            "Bedtime starts in {mins} min, so only about that much of this is usable tonight. \
             \"Later bedtime tonight\" on the Curfew card moves bedtime itself."
        )
    })
}

/// What a parent should be told about pushing bedtime back by `minutes`, given the screen-time
/// budget. `None` when the extension will do what it looks like it does.
///
/// **The mirror of [`grant_shadowed_by_curfew`], and the reason is the same evening.** That one
/// exists because approving screen time during a curfew window looked like it worked and did not.
/// The fix for it was this endpoint — a way to move bedtime itself — which then shipped with the
/// opposite hole: a parent whose child has no screen time left can push bedtime back, be told
/// "Bedtime pushed back 30 min", and watch the PC lock anyway. Same two independent limits, same
/// silent broken promise, now on the button the parent burned by it once will reach for first.
///
/// Reported rather than enforced, matching the other direction: the extension still lands. Moving
/// bedtime while the budget is spent is a sensible thing to do on purpose — a parent may well
/// grant bonus time next — and refusing would be its own surprise.
///
/// A tally that cannot be read yields `None` rather than an error: failing to load a sidecar must
/// not fail the parent's extension. The cost is a missing sentence, not a missing grant.
async fn extension_shadowed_by_budget(state: &AppState, minutes: u32) -> Option<String> {
    let today = crate::config::today();
    // Tally first, config second. The read guard is a `std::RwLock`, so it cannot be held across
    // the `await` — and taking it first meant cloning the whole `Rules` (blocklist, per-app limits,
    // groups, per-weekday budgets) to carry three fields past the suspension point. Loading first
    // leaves the guard entirely inside the synchronous tail, where a borrow is enough.
    let usage = spawn(move || crate::rules::Usage::load_for_today(today))
        .await
        .ok()?;
    let (left, budget_action) = {
        let cfg = crate::state::recover_read(&state.config);
        let left = cfg.rules.budget_cuts_extension_short(
            today,
            cfg.extra.for_day(today),
            &usage,
            minutes,
        )?;
        (left, cfg.rules.budget_action)
    };
    // Exhaustive rather than a `_` fallback. `Warn` cannot reach here — it interrupts nobody, so
    // `budget_cuts_extension_short` filters it — but a wildcard would have silently told a
    // Warn-configured household that their PC "will still lock" if that filter were ever relaxed,
    // and a fourth action would inherit whichever verb happened to be the default. Naming all
    // three makes the compiler ask the next person which one applies.
    let stops = match budget_action {
        crate::rules::EnforceAction::Shutdown => "shut down",
        crate::rules::EnforceAction::Lock => "lock",
        crate::rules::EnforceAction::Warn => return None,
    };
    Some(if left == 0 {
        format!(
            "Screen time is already used up, so the PC will still {stops} — screen time and \
             bedtime are separate limits. Use \"Add bonus time today\" on the Today card to give \
             minutes as well."
        )
    } else {
        format!(
            "Only {left} min of screen time is left, so that is about as much of tonight's later \
             bedtime as they can actually use. \"Add bonus time today\" on the Today card gives \
             more."
        )
    })
}

/// `POST /api/extra-time` → grant bonus minutes to today's budget directly (no child request
/// needed). Uses the exact same `DailyGrant` mechanism as approving a time request, so a mid-day
/// reboot keeps the grant and it resets tomorrow.
///
/// Two callers, told apart by `source`. Absent (the dashboard) means the parent pressed the
/// button, and pressing it twice means it twice — no dedup. A named source (`studygo`) is a
/// robot pushing an *earned* grant after a practice sync, and a robot's decision input —
/// "the threshold was crossed today" — is true all day once it is true at all, so its grant
/// lands **once per source per day**, latched against this machine's trusted clock. The day
/// the phone believes in is never consulted: the *Change the time zone* right is a
/// standard-user privilege, which is the whole reason [`crate::clock`] exists.
///
/// An `Idempotency-Key` header (IETF HTTPAPI draft semantics) additionally lets a retry whose
/// response was lost — a phone scheduler killed between the write and the reply — receive its
/// original outcome. The header is the courtesy; the day latch is the authority. See
/// [`crate::idempotency`].
pub async fn extra_time(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ExtraTimeBody>,
) -> Result<Json<Value>, AppError> {
    require_minutes(body.minutes, MAX_REQUEST_MINUTES)?;
    let source = body.source.unwrap_or_else(|| "parent".into());
    if !valid_source(&source) {
        return Err(AppError::BadRequest(
            "source must be 1-32 characters of a-z, 0-9, _ or -".into(),
        ));
    }
    let replay_key = match headers.get("idempotency-key") {
        None => None,
        Some(value) => {
            let key = value
                .to_str()
                .map_err(|_| AppError::BadRequest("idempotency key is not ASCII".into()))?;
            if key.is_empty() || key.len() > crate::idempotency::MAX_KEY_LEN {
                return Err(AppError::BadRequest(
                    "idempotency key must be 1-128 characters".into(),
                ));
            }
            Some(key.to_owned())
        }
    };
    if let Some(key) = &replay_key {
        let stored = recover_lock(&state.grant_replays).replay(key, std::time::Instant::now());
        if let Some(response) = stored {
            // The retry of a request that already ran: hand back what it got, do nothing again.
            return Ok(Json(response));
        }
    }

    let today = crate::config::today();
    let minutes = body.minutes;
    let robot = source != "parent";
    // Checked and latched inside the one place config is mutated, so two concurrent grants from
    // the same source serialize on `config_save_lock` and the second sees the first's latch.
    let mut granted = true;
    {
        let source = source.clone();
        try_update_config(&state, |c| {
            if robot {
                if c.earned.get(&source) == Some(&today) {
                    granted = false;
                    return Ok(());
                }
                if c.earned.values().filter(|day| **day == today).count() >= MAX_EARNED_SOURCES {
                    return Err(AppError::BadRequest(
                        "too many earned-time sources today".into(),
                    ));
                }
                c.earned.retain(|_, day| *day == today);
                c.earned.insert(source, today);
            }
            c.extra.add(today, minutes);
            Ok(())
        })
        .await?;
    }

    let response = if granted {
        state.audit.record(
            "extra_time_granted",
            json!({ "minutes": minutes, "source": source }),
        );
        state.usage.record(
            "extra_time_granted",
            json!({ "minutes": minutes, "source": source }),
        );
        // A second dashboard, or the same parent's other device, is showing a budget that just
        // moved.
        notify(&state, "usage");
        json!({ "ok": true, "minutes": minutes, "curfew_note": grant_shadowed_by_curfew(&state, minutes) })
    } else {
        // Not audited: with a valid session this outcome is free to trigger repeatedly, and it
        // records nothing a reader of the *grant* line does not already know. Same reasoning as
        // the rate-limited branch of `login` — the fifth site of that defect class.
        json!({ "ok": false, "reason": "already_granted_today" })
    };
    if let Some(key) = replay_key {
        recover_lock(&state.grant_replays).record(key, response.clone(), std::time::Instant::now());
    }
    Ok(Json(response))
}

/// Lock a std mutex, recovering from a poisoned one.
///
/// The same posture as [`crate::state::recover_read`]: a panic in another handler must not
/// permanently disable grants, and the cache's worst corrupt state costs a replay, never a
/// double grant — the persisted day latch holds regardless.
fn recover_lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
    // One hop to the blocking pool for both file reads, not two. The certificate check is a
    // single `stat` (the mtime proxy `cert::days_until_expiry` uses), so pairing it with the tally
    // load costs nothing extra and keeps the disk work off the runtime.
    let (usage, cert_days_left) = spawn(move || {
        (
            crate::rules::Usage::load_for_today(today),
            crate::cert::days_until_expiry(&crate::config::data_paths().cert),
        )
    })
    .await?;
    // Read the enforcer heartbeat here rather than inside the summary: it touches process
    // globals and the system clock, and keeping it at the edge is what lets `today_summary`
    // be tested in full. The certificate is at the edge for the same reason — it is a file.
    let enforcer_age_secs = crate::heartbeat::worst_age_secs();
    Ok(Json(crate::rules::today_summary(
        &rules,
        today,
        extra,
        &usage,
        enforcer_age_secs,
        cert_days_left,
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
    let (enabled, budget, language) = {
        let cfg = crate::state::recover_read(&state.config);
        let extra = cfg.extra.for_day(today);
        (
            cfg.rules.enabled,
            cfg.rules.effective_budget_mins(today, extra),
            cfg.language,
        )
    };
    let usage = spawn(move || crate::rules::Usage::load_for_today(today)).await?;
    // Onto the blocking pool for the same reason `usage` above is, and the same reason
    // `list_time_requests` spawns its `pending()`: `latest()` reads and parses the whole
    // time-request JSONL synchronously. This handler is the child's page polling once a minute
    // with no session, so leaving it inline parks a tokio worker on file I/O on the one route
    // anybody on the LAN can call without authenticating.
    let requests = state.time_requests.clone();
    let request = spawn(move || requests.latest()).await?;

    // The child's own last week, and how often they were watched. Both onto the blocking pool for
    // the same reason as the two reads above, and in one hop rather than two so this handler still
    // costs a single trip off the reactor.
    //
    // `RECENT_DAYS` completed days, totals only. Nothing here names an app: `recent_totals`
    // returns a type with no room for one, because this endpoint needs no session and anyone on
    // the home network can open the page it feeds.
    //
    // The view count is the durable half of a claim this product already makes in the moment —
    // Windows draws a border around the screen while it is being captured, so the child can see a
    // look happening. What they could not see was that it happened at all once the border went
    // away. Counts, never times: a list of times is a timetable to plan around, and this is a page
    // the adversary in the threat model can open. Nothing is lost by aggregating, because the
    // live signal was never this endpoint's to give — the border already gives it.
    let screentime = state.screentime.clone();
    let usage_log = state.usage.clone();
    let audit = state.audit.clone();
    let offset = *crate::clock::now().offset();
    let (recent_days, watched_today) = spawn(move || {
        let rows = crate::screentime::history_rows(&screentime, &usage_log);
        (
            crate::screentime::recent_totals(&rows, today, RECENT_DAYS),
            audit.views_on(today, offset),
        )
    })
    .await?;

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
        // What became of the child's newest request. `null` when they have never asked. This is
        // the only channel by which a *denial* ever reaches them — before it existed, denied and
        // ignored looked identical from this page.
        // A request the child made is their own data, not a rule — which is why it may be here at
        // all. Bedtime deliberately is NOT: `child_status_is_unauthenticated_and_leaks_no_rules`
        // forbids `curfew` on this endpoint, because a schedule handed to the child is a map for
        // planning around it. Showing "screen off at 21:00" here would be friendlier and was
        // written and then reverted; it is a change to that stated principle, not an addition to
        // it, so it needs deciding rather than slipping in beside an unrelated fix. The child is
        // still warned — the enforcer puts a countdown on their desktop at 15, 5 and 1 minutes.
        "request": request,
        // Which strings this page should render. Not negotiated from `Accept-Language`: that is set
        // in the child's own browser, and the child does not get to choose the language of the
        // notice telling them what is being watched.
        "language": language.tag(),
        // The child's own completed days, oldest first, `minutes: null` where nothing was measured.
        "recent_days": recent_days,
        // How many times a parent viewed this screen today. Views only — a kill or a lock is not
        // a look, and the child saw those happen anyway.
        "watched_today": watched_today,
    })))
}

/// How many completed days the child's own page shows.
///
/// A week, not the parent's 7/30/90. The purpose here is different: the parent's report is for
/// spotting a change, and the child's is for recognising their own pattern, which a week does and
/// a quarter buries. It is also the number that fits on a phone under the figure that page exists
/// to answer, without turning a one-question page into a second dashboard.
const RECENT_DAYS: u32 = 7;

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

/// `GET /api/policy` → this install's household settings, as a downloadable file.
///
/// The companion to `GET /api/export`, and the half that was missing. That one takes the *history*
/// off the machine; this takes the *setup*. Between them a parent can rebuild the PC, or set up a
/// second one, without re-entering every curfew window and per-app limit by hand — which until now
/// was the only option, because `config.json` sits in an ACL-locked directory reachable from an
/// elevated console on the child's PC and nowhere else.
///
/// `Content-Disposition: attachment` and a plain `<a download>` link on the dashboard, exactly as
/// `export` does: the session cookie rides along on a same-origin navigation, so there is no blob
/// handling and no JavaScript that can fail silently.
///
/// What is left out — the password hash, the port, the certificate's names, today's granted bonus
/// minutes, and both halves of the trusted-clock anchor — is documented on
/// [`crate::config::Policy`], because the exclusions are the design rather than an oversight.
pub async fn get_policy(State(state): State<AppState>) -> Result<Response, AppError> {
    let policy = crate::state::recover_read(&state.config).policy();
    let today = crate::config::today();

    state.audit.record(
        "policy_exported",
        json!({ "routines": policy.routines.len() }),
    );

    let body = json!({
        "nestwatch_version": crate::VERSION,
        "exported_on": today.to_string(),
        // Said in the file rather than only in the docs, because the file is what outlives both —
        // the same reason `export`'s manifest carries its note.
        "note": "Household settings only. Deliberately excludes the password, the port, the \
    certificate, today's granted minutes and the trusted-clock anchor: those describe one machine, and \
    restoring another machine's clock anchor would silently weaken curfew enforcement.",
        "policy": policy,
    });

    let filename = format!("nestwatch-settings-{today}.json");
    Ok((
        [
            (header::CONTENT_TYPE, "application/json".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
        ],
        Json(body),
    )
        .into_response())
}

/// The uploaded document. Accepts what [`get_policy`] produced, and little else.
///
/// `policy` is required, so an unrelated JSON file is refused by serde with a message naming the
/// missing field rather than being read as an empty policy — which would wipe the household's
/// settings and report success. `nestwatch_version` is optional and advisory; see [`set_policy`].
#[derive(Deserialize)]
pub struct PolicyImport {
    #[serde(default)]
    nestwatch_version: Option<String>,
    policy: crate::config::Policy,
}

/// `POST /api/policy` → replace the household settings from an exported document.
///
/// Validated as a whole before any of it is applied, through the **same** `validate` calls the
/// live editors use — see [`crate::config::Policy::validate`]. All-or-nothing: a partial restore
/// would leave a household with some of yesterday's settings and some of today's, which is a state
/// nobody chose and no screen displays.
///
/// # The version is reported, not enforced
///
/// A document from a *newer* build may contain settings this one has no field for, and serde drops
/// them silently — the quiet failure this codebase exists to avoid. The honest fix is not to refuse
/// the import (a parent restoring a backup after a downgrade has a real problem and refusing does
/// not solve it) but to say so: the response carries a `warning` naming both versions, and the
/// dashboard shows it. Refusing outright would also mean this endpoint needed a semver comparison,
/// which is a second implementation of something `app.js` already owns for the update check.
pub async fn set_policy(
    State(state): State<AppState>,
    Json(body): Json<PolicyImport>,
) -> Result<Json<Value>, AppError> {
    body.policy.validate().map_err(AppError::BadRequest)?;

    let warning = match body.nestwatch_version.as_deref() {
        Some(v) if v != crate::VERSION => Some(format!(
            "this file was exported by Nestwatch {v} and you are running {}. It was applied, but              any setting that version has and this one does not was not carried over.",
            crate::VERSION
        )),
        _ => None,
    };

    let audit_fields = json!({
        "from_version": body.nestwatch_version,
        "routines": body.policy.routines.len(),
        "curfew_enabled": body.policy.curfew.enabled,
        "daily_budget_mins": body.policy.rules.daily_budget_mins,
    });
    update_config(&state, move |c| c.apply_policy(body.policy)).await?;
    // One line for the whole restore rather than one per section: it is a single parent action,
    // and four rows would say it happened four times.
    state.audit.record("policy_imported", audit_fields);

    Ok(Json(json!({ "ok": true, "warning": warning })))
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
    require_minutes(body.minutes, MAX_REQUEST_MINUTES)?;
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
        // The reason `/api/events` exists: a child is now waiting on a person, and without this
        // that person finds out at their next minute boundary.
        notify(&state, "requests");
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
    // Two panels moved: the queue lost a row and today's budget grew.
    notify(&state, "requests");
    notify(&state, "usage");
    Ok(Json(
        json!({ "ok": true, "minutes": minutes, "curfew_note": grant_shadowed_by_curfew(&state, minutes) }),
    ))
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
    notify(&state, "requests");
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
    require_minutes(body.minutes, crate::timecode::MAX_CODE_MINUTES)?;
    let codes = state.time_codes.clone();
    let minutes = body.minutes;
    let code = spawn(move || codes.issue(minutes)).await?;
    let Some(code) = code else {
        return Err(AppError::BadRequest(format!(
            "at most {} codes can be active at once",
            crate::timecode::MAX_ACTIVE_CODES
        )));
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
/// logged in) but LAN-gated (outer router → `require_lan_peer`) and per-IP rate-limited.
///
/// **That rate limit is the primary defence, not a secondary one.** A code is six characters, so
/// the guessable space is about 1.07 billion; five attempts a minute is what turns that into
/// centuries. Loosening or removing the limiter therefore changes the security of time codes
/// directly, and `timecode::CODE_LEN` would have to be revisited with it — see that module's doc.
///
/// On a valid code the minutes are added to today's budget; the response reveals only whether it
/// worked (and how many minutes), never anything else.
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
    // Redeemed by the CHILD, so the parent has no other way to learn it happened until the poll.
    notify(&state, "usage");
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
