//! Router assembly and the TLS server.
//!
//! Route map:
//! ```text
//!   GET  /                      app shell (unauthenticated)
//!   GET  /session               is the caller logged in? (drives the UI)
//!   POST /login   POST /logout  auth endpoints
//!   GET  /ask                   child "request more time" page (unauthenticated, LAN-gated)
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
//!     POST /api/extra-time
//!     GET  POST /api/rules
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
use tower_sessions::{Expiry, MemoryStore, SessionManagerLayer};

use crate::state::AppState;
use crate::{api, auth, cert, config, security, web};

/// Build the full application router. Kept separate from [`serve`] so tests can drive it
/// directly without binding a socket or setting up TLS.
pub fn build_router(state: AppState) -> Router {
    // In-memory session store: sessions live only while the process runs, which is exactly
    // right for a single-user LAN tool (a reboot logs you out — harmless).
    let session_layer = SessionManagerLayer::new(MemoryStore::default())
        .with_secure(true)
        .with_http_only(true)
        .with_same_site(SameSite::Strict)
        .with_expiry(Expiry::OnInactivity(CookieDuration::hours(12)))
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
        .route("/extra-time", post(api::extra_time))
        .route("/rules", get(api::get_rules).post(api::set_rules))
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
        // Child-facing, unauthenticated but LAN-gated (see the outer layers below).
        .route("/ask", get(web::ask))
        .route("/time-request", post(api::time_request))
        .route("/redeem-code", post(api::redeem_code))
        .nest("/api", api)
        .fallback(web::static_handler)
        .layer(session_layer)
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
    let tls = RustlsConfig::from_pem_file(&paths.cert, &paths.key).await?;

    let port = crate::state::recover_read(&state.config).port;
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    // Curfew enforcement runs alongside the server for the whole process lifetime.
    // run_enforcer loops forever; if it ever returns, surface that loudly.
    {
        let control = state.control.clone();
        let config = state.config.clone();
        tokio::spawn(async move {
            crate::curfew::run_enforcer(control, config).await;
            tracing::error!("curfew enforcer exited unexpectedly — curfew is no longer enforced");
        });
    }

    // Usage-rules enforcement (screen-time budget, blocklist, per-app limits) runs in parallel.
    {
        let control = state.control.clone();
        let config = state.config.clone();
        let usage = state.usage.clone();
        tokio::spawn(async move {
            crate::rules::run_rules_enforcer(control, config, usage).await;
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
