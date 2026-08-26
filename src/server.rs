//! Router assembly and the TLS server.
//!
//! Route map:
//! ```text
//!   GET  /                      app shell (unauthenticated)
//!   GET  /session               is the caller logged in? (drives the UI)
//!   POST /login   POST /logout  auth endpoints
//!   GET  /p/{token}             one-time pairing from the install QR (unauthenticated,
//!                               LAN-gated, throttled; always redirects to /)
//!   GET  /ask                   child "request more time" page (unauthenticated, LAN-gated)
//!   GET  /status                child's own time left (unauthenticated, LAN-gated, no rules)
//!   POST /time-request          child submits a request (unauthenticated, LAN-gated, throttled)
//!   POST /redeem-code           child redeems a time code (unauthenticated, LAN-gated, throttled)
//!   /api/*                      guarded by `require_auth`:
//!     GET  /api/screenshot
//!     GET  /api/processes
//!     POST /api/processes/{pid}/kill
//!     POST /api/shutdown
//!     POST /api/lock
//!     GET  POST /api/curfew
//!     GET  /api/audit
//!     GET  /api/usage
//!     GET  /api/usage/today
//!     GET  /api/screentime
//!     GET  /api/export
//!     POST /api/re-anchor
//!     POST /api/extra-time
//!     GET  POST /api/rules
//!     GET  POST /api/routines
//!     POST /api/routines/{name}/apply  POST /api/routines/{name}/delete
//!     GET  /api/time-requests
//!     POST /api/time-requests/{id}/approve  POST /api/time-requests/{id}/deny
//!     GET  POST /api/time-codes
//!     POST /api/password
//!   *                           embedded static assets (fallback)
//! ```

use std::net::SocketAddr;

use anyhow::Result;
use axum::routing::{get, post};
use axum::{Router, middleware};
use axum_server::tls_rustls::RustlsConfig;
use tower_sessions::cookie::SameSite;
use tower_sessions::cookie::time::Duration as CookieDuration;
use tower_sessions::{Expiry, SessionManagerLayer};

use crate::state::AppState;
use crate::{api, auth, cert, config, security, web};

/// Build the full application router. Kept separate from [`serve`] so tests can drive it
/// directly without binding a socket or setting up TLS.
pub fn build_router(state: AppState) -> Router {
    // Sessions persist to the ACL-locked data dir, so a service restart, an upgrade, or a reboot
    // doesn't sign the parent out. Without that, every restart costs them two annoyances on a
    // phone: click through the self-signed-cert warning again, then retype a long passphrase.
    // 30-day inactivity ("remember this device") makes signing in a one-time cost per device.
    let session_layer = SessionManagerLayer::new(state.sessions.clone())
        .with_secure(true)
        .with_http_only(true)
        .with_same_site(SameSite::Strict)
        .with_expiry(Expiry::OnInactivity(CookieDuration::days(30)))
        .with_name("hh_session");

    let api = Router::new()
        .route("/screenshot", get(api::screenshot))
        .route("/processes", get(api::list_processes))
        .route("/processes/{pid}/kill", post(api::kill_process))
        .route("/shutdown", post(api::shutdown))
        .route("/lock", post(api::lock))
        .route("/curfew", get(api::get_curfew).post(api::set_curfew))
        .route("/audit", get(api::audit))
        .route("/usage", get(api::usage))
        .route("/usage/today", get(api::usage_today))
        .route("/screentime", get(api::screentime))
        .route("/export", get(api::export))
        .route("/re-anchor", post(api::re_anchor))
        .route("/extra-time", post(api::extra_time))
        .route("/rules", get(api::get_rules).post(api::set_rules))
        .route("/routines", get(api::list_routines).post(api::save_routine))
        .route("/routines/{name}/apply", post(api::apply_routine))
        .route("/routines/{name}/delete", post(api::delete_routine))
        .route("/time-requests", get(api::list_time_requests))
        .route(
            "/time-requests/{id}/approve",
            post(api::approve_time_request),
        )
        .route("/time-requests/{id}/deny", post(api::deny_time_request))
        .route(
            "/time-codes",
            get(api::list_time_codes).post(api::issue_time_code),
        )
        .route("/password", post(api::change_password))
        .route_layer(middleware::from_fn(auth::require_auth));

    Router::new()
        .route("/", get(web::index))
        .route("/session", get(auth::me))
        .route("/login", post(auth::login))
        .route("/logout", post(auth::logout))
        // One-time pairing from the install QR: unauthenticated by design, LAN-gated, and
        // throttled on the same per-IP limiter as /login.
        .route("/p/{token}", get(auth::pair))
        // Child-facing, unauthenticated but LAN-gated (see the outer layers below).
        .route("/ask", get(web::ask))
        .route("/status", get(api::child_status))
        .route("/time-request", post(api::time_request))
        .route("/redeem-code", post(api::redeem_code))
        .nest("/api", api)
        .fallback(web::static_handler)
        .layer(session_layer)
        // Reject anything the browser reports as coming from another origin, before the session
        // layer attaches cookie authority to it. `SameSite=Strict` can't see the port, so this
        // is what stops a same-site-different-port page from driving the bodyless POSTs.
        .layer(middleware::from_fn(security::require_same_origin))
        // Reject off-LAN clients before any session/auth work…
        .layer(middleware::from_fn(security::require_lan_peer))
        // …and stamp security headers on every response (outermost, so even the 403 above
        // and 404s carry them).
        .layer(middleware::map_response(security::set_security_headers))
        .with_state(state)
}

/// Ensure the TLS cert exists, then bind and serve over HTTPS until terminated.
pub async fn serve(state: AppState) -> Result<()> {
    serve_with_handle(state, axum_server::Handle::new()).await
}

/// Like [`serve`], but with a caller-supplied handle so an external controller (e.g. the
/// Windows service) can trigger graceful shutdown.
pub async fn serve_with_handle(
    state: AppState,
    handle: axum_server::Handle<SocketAddr>,
) -> Result<()> {
    let paths = config::data_paths();
    cert::ensure_cert(&paths.cert, &paths.key)?;
    cert::warn_if_expiring(&paths.cert);
    let tls = RustlsConfig::from_pem_file(&paths.cert, &paths.key).await?;

    // Install the trusted-clock anchor before the enforcers start, so the very first tick
    // already resists a shifted timezone.
    {
        let cfg = crate::state::recover_read(&state.config);
        if let Some(mins) = cfg.tz_offset_mins {
            crate::clock::set_anchor(mins);
        }
        // Order matters only in that both must be in place before the first tick; the zone is the
        // half that actually detects a substituted timezone, the offset the half that answers it.
        crate::clock::set_anchor_zone(cfg.tz_zone.clone());
    }

    let port = crate::state::recover_read(&state.config).port;
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    // Curfew enforcement runs alongside the server for the whole process lifetime.
    // run_enforcer loops forever; if it ever returns, surface that loudly.
    {
        let control = state.control.clone();
        let config = state.config.clone();
        let usage = state.usage.clone();
        tokio::spawn(async move {
            crate::curfew::run_enforcer(control, config, usage).await;
            tracing::error!("curfew enforcer exited unexpectedly — curfew is no longer enforced");
        });
    }

    // Per-app foreground time. The watcher lives in the child's session (a Session-0 service
    // cannot see their desktop at all), reports over a pipe, and is respawned by the supervisor
    // when the child kills it or signs out. On a non-Windows dev build there is no watcher, so the
    // feed simply never reports — and a feed that never reports records the day as *unmeasured*
    // rather than as zero, which is the behaviour we want anyway.
    let foreground = crate::foreground::Feed::new();
    #[cfg(windows)]
    {
        let feed = foreground.clone();
        // A plain OS thread, not a tokio task: it blocks on a pipe read for the life of a login
        // session, which would park a runtime worker indefinitely.
        std::thread::spawn(move || crate::session::run_watcher_supervisor(feed));
    }

    // Usage-rules enforcement (screen-time budget, blocklist, per-app limits) runs in parallel.
    {
        let control = state.control.clone();
        let config = state.config.clone();
        let usage = state.usage.clone();
        let screentime = state.screentime.clone();
        let foreground = foreground.clone();
        tokio::spawn(async move {
            crate::rules::run_rules_enforcer(control, config, usage, screentime, foreground).await;
            tracing::error!(
                "rules enforcer exited unexpectedly — usage rules are no longer enforced"
            );
        });
    }

    let router = build_router(state);

    tracing::info!("listening on https://0.0.0.0:{port} (reach it at https://<this-pc>:{port})");
    axum_server::bind_rustls(addr, tls)
        .handle(handle)
        // `_with_connect_info` populates `ConnectInfo<SocketAddr>` so the LAN gate, per-IP
        // login limiter, and audit log can see the true peer address. `Handle<SocketAddr>`
        // is unchanged.
        .serve(router.into_make_service_with_connect_info::<SocketAddr>())
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    /// This file's own source, so the guard below reads the router as written rather than as
    /// remembered.
    const SERVER_RS: &str = include_str!("server.rs");

    /// Every route reachable **without a session**, exactly.
    ///
    /// Each one is unauthenticated on purpose and the reason is worth keeping next to the list:
    ///
    /// * `/` and `/session` — the login page itself, and the question "am I signed in?".
    /// * `/login`, `/logout` — the transition either way.
    /// * `/p/{token}` — the install QR's one-time pairing redemption; being signed out is the
    ///   entire point. Rate-limited on the same per-IP limiter as `/login`.
    /// * `/ask`, `/status`, `/time-request`, `/redeem-code` — the child's page and the three calls
    ///   it makes. The child has no password and must not need one.
    ///
    /// Everything else lives under `/api`, behind `require_auth`.
    const UNAUTHENTICATED: &[&str] = &[
        "/",
        "/ask",
        "/login",
        "/logout",
        "/p/{token}",
        "/redeem-code",
        "/session",
        "/status",
        "/time-request",
    ];

    /// The authentication boundary is real but invisible where it can be broken.
    ///
    /// `api.rs` holds both audiences interleaved — `screenshot`, `kill_process` and `shutdown` sit
    /// in the same file as `child_status`, `time_request` and `redeem_code`. What separates them is
    /// not in that file at all: it is *which of the two routers below* a route is registered on.
    /// The `/api` nest carries `route_layer(require_auth)`; the outer router does not, and both
    /// look identical at the definition site.
    ///
    /// So adding a handler to the wrong router is a one-line mistake with no local evidence, and
    /// one direction of that mistake exposes a parent capability — killing a process, shutting the
    /// machine down, reading the audit log — to a page the child can reach with no password. The
    /// child owns an account on this PC and is on this LAN; that is the threat model, not a
    /// hypothetical.
    ///
    /// This guard pins the unauthenticated set so it can only change deliberately. It reads the
    /// source rather than the built `Router` because axum exposes no way to enumerate routes or
    /// ask which layers apply to one — the information exists only in the text.
    #[test]
    fn only_the_known_child_facing_routes_are_reachable_without_a_session() {
        // Everything after the `require_auth` layer is the outer, unauthenticated router. Splitting
        // on the layer itself rather than on a line number means moving routes around cannot
        // silently change what this test is looking at.
        // Drop this test module before scanning. `include_str!` pulls in the whole file, comments
        // and all, so the prose below describing `.route("` reads as a route registration and the
        // guard reports itself. That is not hypothetical — it is what this test did on its first
        // run, and it is the same trap `no_alpine_template_inside_svg` hit when the comment
        // explaining it contained the markup it forbids. A source scan must exclude its own text.
        let router_src = SERVER_RS
            .split_once("#[cfg(test)]")
            .map_or(SERVER_RS, |(before, _)| before);

        let (guarded, open) = router_src
            .split_once("route_layer(middleware::from_fn(auth::require_auth))")
            .expect("the /api router must apply require_auth — if this moved, this guard is stale");

        assert!(
            guarded.contains(".route(\"/screenshot\""),
            "sanity: the guarded half should hold the parent's routes"
        );

        // `.route("` only: `.nest(` and `.fallback(` are not routes, and the fallback is the static
        // asset handler, which is deliberately public.
        let mut found: Vec<&str> = open
            .match_indices(".route(\"")
            .map(|(i, m)| {
                let rest = &open[i + m.len()..];
                &rest[..rest.find('"').expect("unterminated route path literal")]
            })
            .collect();
        found.sort_unstable();
        found.dedup();

        let mut expected: Vec<&str> = UNAUTHENTICATED.to_vec();
        expected.sort_unstable();

        assert_eq!(
            found, expected,
            "the set of routes reachable without a session has changed.\n\nIf you added a route to \
             the outer router deliberately, add it to UNAUTHENTICATED above and say in the comment \
             why it needs no password. If you did not, you have registered it on the wrong router \
             and it is now reachable by the child."
        );
    }
}
