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
//!     POST /api/curfew/extend
//!     GET  /api/audit
//!     GET  /api/usage
//!     GET  /api/usage/today
//!     GET  /api/screentime
//!     GET  /api/events        (SSE: names what changed; carries no data)
//!     GET  /api/export
//!     POST /api/re-anchor
//!     GET  POST /api/language
//!     POST /api/extra-time
//!     GET  POST /api/rules
//!     GET  POST /api/policy   (household settings: download / restore)
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
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use axum::{Router, middleware};
use axum_server::tls_rustls::RustlsConfig;
use hyper_util::rt::{TokioExecutor, TokioTimer};
use hyper_util::server::conn::auto::Builder as HyperBuilder;
use tower_sessions::cookie::SameSite;
use tower_sessions::cookie::time::Duration as CookieDuration;
use tower_sessions::{Expiry, SessionManagerLayer};

use crate::state::AppState;
use crate::{api, auth, cert, config, security, web};

/// Body cap for the two routes an **unauthenticated** caller can reach.
///
/// axum already applies a `DefaultBodyLimit` of 2 MB to every `Bytes`-derived extractor, `Json`
/// included, so this is a tightening rather than a missing bound — checked before writing it,
/// after expecting to find no limit at all.
///
/// 2 MB is the wrong number *here* because of who can send it. Everything else behind
/// `require_auth` is a request the parent made; these two are reachable by anyone on the LAN,
/// which includes the child, and the per-IP limiters cap the **rate** rather than the **size** —
/// so the product they permit is megabytes of parsing per minute from an unauthenticated caller.
///
/// 8 KiB is derived from the markup rather than picked. `assets/ask.html` caps the reason at
/// `maxlength="200"` and the code at `maxlength="12"`, so the largest honest body is a 200-char
/// reason: 800 bytes at 4 bytes per char, or about 1.2 KiB if every one of them JSON-escapes to
/// `\uXXXX`. This leaves more than five times that, and the server truncates to
/// `timereq::MAX_REASON_CHARS` afterwards regardless.
const CHILD_BODY_LIMIT: usize = 8 * 1024;

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
        .route("/curfew/extend", post(api::extend_curfew))
        .route("/audit", get(api::audit))
        .route("/usage", get(api::usage))
        .route("/usage/today", get(api::usage_today))
        .route("/screentime", get(api::screentime))
        .route("/events", get(api::events))
        .route("/export", get(api::export))
        .route("/re-anchor", post(api::re_anchor))
        .route("/language", get(api::get_language).post(api::set_language))
        .route("/extra-time", post(api::extra_time))
        .route("/rules", get(api::get_rules).post(api::set_rules))
        .route("/policy", get(api::get_policy).post(api::set_policy))
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
        .route(
            "/time-request",
            post(api::time_request).layer(DefaultBodyLimit::max(CHILD_BODY_LIMIT)),
        )
        .route(
            "/redeem-code",
            post(api::redeem_code).layer(DefaultBodyLimit::max(CHILD_BODY_LIMIT)),
        )
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
        let wake = state.enforcement_wake.subscribe();
        tokio::spawn(async move {
            crate::curfew::run_enforcer(control, config, usage, wake).await;
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
        let wake = state.enforcement_wake.subscribe();
        tokio::spawn(async move {
            crate::rules::run_rules_enforcer(control, config, usage, screentime, foreground, wake)
                .await;
            tracing::error!(
                "rules enforcer exited unexpectedly — usage rules are no longer enforced"
            );
        });
    }

    let router = build_router(state);

    tracing::info!("listening on https://0.0.0.0:{port} (reach it at https://<this-pc>:{port})");
    serve_http1_only(&tls);
    let mut server = axum_server::bind_rustls(addr, tls).http1_only();
    install_connection_timeouts(server.http_builder());
    server
        .handle(handle)
        // `_with_connect_info` populates `ConnectInfo<SocketAddr>` so the LAN gate, per-IP
        // login limiter, and audit log can see the true peer address. `Handle<SocketAddr>`
        // is unchanged.
        .serve(router.into_make_service_with_connect_info::<SocketAddr>())
        .await?;
    Ok(())
}

/// Stop advertising HTTP/2 in the TLS handshake, because this server no longer speaks it.
///
/// # This is not optional decoration on `http1_only()` — without it the dashboard breaks
///
/// `axum-server` hard-codes `alpn_protocols = ["h2", "http/1.1"]` inside `config_from_der`, which
/// is where `RustlsConfig::from_pem_file` ends up, and its `Server::http1_only()` changes only
/// which hyper builder is used. **Nothing connects the two.** So `http1_only()` on its own leaves
/// the handshake offering h2 first, every current browser takes it, and the connection is then
/// served by an HTTP/1.1 parser that receives an h2 preface. That is not a degraded experience, it
/// is a blank page for everyone — shipped as a one-line cleanup.
///
/// `O81` recommended that one line, having verified the `hyper-util` half. The ALPN layer sits
/// above what was verified.
///
/// # Why this can be four lines rather than a rebuilt config
///
/// `rustls::ServerConfig` derives `Clone`, and `axum-server` exposes `get_inner` /
/// `reload_from_config` for exactly this kind of swap. So the certificate loading, the key
/// parsing and the process-wide `ring` provider selection all stay where they are, and the only
/// thing that changes is the one field that has to change. Rebuilding the config by hand would
/// duplicate all three for no benefit and one more place to get the provider wrong.
fn serve_http1_only(tls: &RustlsConfig) {
    let mut config = (*tls.get_inner()).clone();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    tls.reload_from_config(std::sync::Arc::new(config));
}

/// Give a stalled connection a way to die.
///
/// # The hole this closes, and why nothing reported it
///
/// `hyper` documents a **30-second default** for `header_read_timeout`, and it is real —
/// `http1::Builder::new` sets `Dur::Default(Some(30s))`. It is also **inert here**, and the
/// reason is one line further on: applying it runs through `Time::check`, which returns `None`
/// (with an internal `warn!` nobody sees) whenever no `Timer` was installed. `axum-server`
/// constructs its builder as `Builder::new(TokioExecutor::new())` and never calls `.timer(..)`,
/// so the default resolved to nothing and every connection had **no read timeout at all**.
///
/// **Why it survived is the asymmetry one arm further down `Time::check`.** A `Dur::Default` with
/// no timer warns and returns `None`; a `Dur::Configured` with no timer **panics**. So a maintainer
/// who had ever written the value out explicitly would have crashed on the first connection and
/// found this immediately. Inheriting it silently is the only way to hold this bug. Verified in
/// `hyper 1.10.1`, the version this lockfile pins — and it is the **only** `Dur::Default` in
/// hyper's server code, so nothing else was ever going to close these connections either.
/// (Both facts from an independent read by the concurrent session on this repo, which reached the
/// same conclusion from the dependency sources while this was being measured over a socket.)
///
/// Measured before writing this, against the real binary on a scratch data dir rather than
/// reasoned about. A connection that completed the TLS handshake and then sent **zero bytes**,
/// and one that sent `GET / HTTP/1.1\r\nHost: …\r\n` and then stopped mid-header, were both still
/// open after 65 seconds. A complete request on the same build answered `200` throughout, so the
/// probe was measuring the server and not itself.
///
/// **Both protocols, because `axum-server` offers `h2` in ALPN** (`tls_rustls/mod.rs` sets
/// `alpn_protocols = ["h2", "http/1.1"]`). An h1-only fix is one an attacker steps around by
/// choosing the other protocol, which is worse than no fix: it looks closed. An idle `h2`
/// connection — preface plus an empty `SETTINGS` and then silence — was measured open at 66s too.
///
/// # What each half is worth
///
/// * **h1** costs no invented number. Installing the timer activates hyper's own documented
///   default, so the value stays hyper's to choose and this line stays a wiring fix.
/// * **h2 has no timer-dependent default** — checked, `http2.rs` contains no `Dur::Default` — so
///   this half sets a policy, and the numbers are derived rather than picked: the same 30 seconds
///   h1 already uses, so a client cannot get a longer grace merely by negotiating the other
///   protocol. It reaps a peer that stops answering `PING`.
///
/// # What it does NOT fix, and this half matters more than the half it does
///
/// **A connection that sends nothing at all is still held forever.** Re-measured after this
/// change: the partial-header case now closes between 10s and 31s, and the idle `h2` case closes
/// at ~60s, but the zero-byte case was still open at 66s — exactly as before.
///
/// The cause is above hyper, in `hyper-util`'s protocol sniffing, and no builder setting reaches
/// it. `auto::Builder::serve_connection` only constructs an h1 or h2 `Conn` once it knows which;
/// until then it parks in `ReadVersion`, a future that polls for the 24 bytes of the h2 preface
/// **with no timeout of its own**. Send one byte that is not `P` and it resolves instantly and
/// the h1 timer arms — which is why the partial-header case is now covered. Send nothing and
/// neither protocol's timeout machinery is ever built.
///
/// So this does not change an attacker's cheapest strategy, which was always "connect, say
/// nothing". Recorded as `O81` rather than papered over, with the two candidate fixes:
///
/// * A first-byte deadline on the accepted stream — an `Accept` wrapper whose `AsyncRead` fails
///   the connection if no plaintext byte arrives in N seconds. Complete, and it is ~90 lines of
///   hand-written pinning in the TLS path of a service whose worst outcome is supposed to be a PC
///   that keeps working. Not landed during this pass on that ground alone.
/// * `http1_only()`, which is one line and **verified in `hyper-util` source** to skip
///   `ReadVersion` entirely (`version: Some(Version::H1)` takes the `serve_connection` arm
///   directly). It closes the case completely and costs HTTP/2 — which is a product decision, not
///   a cleanup: `/api/events` holds one connection open per tab, and h1.1 browsers cap at six per
///   origin, so a parent with several tabs is exactly who would pay for it.
///
/// # The size of the thing, measured rather than assumed
///
/// 300 stalled connections cost this process **12.5 MB of RSS (~42 KB each)** and 300 handles,
/// opened at ~1,100/s from one client — and the dashboard still answered instantly throughout.
/// So this was never the instant lock-out it first looked like. It is an unbounded leak that
/// nothing reclaims, and one machine's worth of ephemeral ports (~16k on Windows' default dynamic
/// range) is roughly 690 MB held for as long as the attacker cares to hold it.
///
/// A **responsive** client holding idle h2 connections open, answering every `PING`, is unbounded
/// too. That is true of every h2 server and is what a connection *limit* is for, not a timeout.
fn install_connection_timeouts(builder: &mut HyperBuilder<TokioExecutor>) {
    // Mirrors hyper's own h1 default, so the two protocols expire a stalled peer alike.
    const H2_KEEPALIVE: std::time::Duration = std::time::Duration::from_secs(30);

    // Installing the timer is the whole h1 fix; no timeout value is set here on purpose.
    builder.http1().timer(TokioTimer::new());
    builder
        .http2()
        .timer(TokioTimer::new())
        .keep_alive_interval(Some(H2_KEEPALIVE))
        .keep_alive_timeout(H2_KEEPALIVE);
}

#[cfg(test)]
mod tests {
    // Imported *inside* the test module, below the `#[cfg(test)]` cut, deliberately.
    // `tests/scanner_guards.rs` decides whether a file has "adopted" srcscan by looking for the
    // `use` item in the file's PRODUCTION half, and an adopted file is skipped wholesale. A
    // top-of-file import here would therefore switch that guard off for `server.rs` — silently,
    // and for a scanner (the route guard below) that genuinely reads line-oriented text.
    use crate::srcscan::{find_tokens, production_source};

    /// This file's own source, so the guard below reads the router as written rather than as
    /// remembered.
    const SERVER_RS: &str = include_str!("server.rs");

    /// The connection timeouts are wired in, on both protocols.
    ///
    /// # Why this is a source scan and not a behavioural test
    ///
    /// The property is "a stalled connection eventually dies", and the shortest honest
    /// observation of it costs **thirty seconds of wall clock** — hyper owns the h1 value and it
    /// is not injectable, so a real test would add half a minute to every CI run to assert one
    /// boolean. The behaviour was verified by probe instead, against the real binary on a scratch
    /// data dir, and both directions were watched: before the fix a partial-header connection was
    /// open at 65s, after it the same connection closed between 10s and 31s.
    ///
    /// What a probe run by hand cannot do is notice when somebody deletes the call. That is what
    /// this is for, and it is the same trade `only_the_known_child_facing_routes_are_reachable…`
    /// below already makes: pin the wiring in the text, because the text is where the information
    /// exists.
    ///
    /// **Both protocols are asserted, and that is the point rather than completeness.**
    /// `axum-server` offers `h2` in ALPN, so a timer installed on h1 alone is one an attacker
    /// steps around by negotiating the other protocol — a fix that looks closed and is not. Half
    /// of this guard exists to make that specific regression loud.
    #[test]
    fn a_stalled_connection_is_given_a_way_to_die_on_both_protocols() {
        let src = production_source(SERVER_RS);

        assert_eq!(
            find_tokens(
                src,
                &[
                    "install_connection_timeouts",
                    "(",
                    "server",
                    ".http_builder"
                ]
            )
            .len(),
            1,
            "`serve_with_handle` no longer installs the connection timeouts. Without that call \
             hyper's own 30s `header_read_timeout` default silently resolves to None — it is \
             gated on a Timer that nothing else here installs — and every connection is held \
             until the peer closes it."
        );

        for (tokens, why) in [
            (
                [".http1", "(", ")", ".timer", "("].as_slice(),
                "h1 has no timer, so hyper's `header_read_timeout` default is inert again",
            ),
            (
                [".http2", "(", ")", ".timer", "("].as_slice(),
                "h2 has no timer, so its keep-alive below cannot fire",
            ),
            (
                [".keep_alive_interval", "("].as_slice(),
                "h2 never probes an idle peer, so an h2 connection is held forever",
            ),
            (
                [".keep_alive_timeout", "("].as_slice(),
                "h2 probes but never gives up, so a peer that stops answering is still held",
            ),
        ] {
            assert!(
                !find_tokens(src, tokens).is_empty(),
                "{tokens:?} is gone from server.rs — {why}"
            );
        }
    }

    /// Serving only HTTP/1.1 and advertising only HTTP/1.1 must stay one decision.
    ///
    /// These are two calls into two different crates and nothing but this test connects them.
    /// Removing either one alone is silent at compile time and catastrophic at runtime, in
    /// opposite directions:
    ///
    /// * `.http1_only()` without [`serve_http1_only`] — the handshake still offers `h2`, every
    ///   current browser takes it, and an HTTP/1.1 parser then receives an h2 preface. **The
    ///   dashboard is blank for everyone.** This is the shape `O81` recommended, having verified
    ///   the `hyper-util` half; the ALPN layer sits above what was verified.
    /// * [`serve_http1_only`] without `.http1_only()` — the handshake correctly offers only
    ///   `http/1.1`, but `hyper-util` is back in `auto` mode, so it parks in `ReadVersion`
    ///   waiting for an h2 preface that ALPN has guaranteed will never arrive. Neither protocol's
    ///   timeout machinery is built and the connection that sends nothing is held forever again —
    ///   the exact leak this pair was written to close.
    ///
    /// Verified over a socket against the real binary rather than argued: with both calls in
    /// place, a client offering `h2,http/1.1` negotiates `http/1.1` and gets `200 OK`, a
    /// connection that sends zero bytes is closed after **30.0 s** (it was open at 66 s), and an
    /// `h2`-only client is refused at the handshake with TLS alert 120 rather than hung.
    #[test]
    fn serving_one_protocol_and_advertising_it_cannot_drift_apart() {
        let src = production_source(SERVER_RS);

        assert!(
            !find_tokens(
                src,
                &["bind_rustls", "(", "addr", ",", "tls", ")", ".http1_only"]
            )
            .is_empty(),
            "the server no longer restricts itself to HTTP/1.1, but `serve_http1_only` still \
             narrows ALPN to it — so hyper is back in `auto` mode waiting for an h2 preface that \
             can never arrive, and a connection sending nothing is held forever again"
        );
        assert!(
            !find_tokens(src, &["serve_http1_only", "(", "&", "tls", ")"]).is_empty(),
            "ALPN is no longer narrowed to http/1.1 while the server still serves only HTTP/1.1. \
             The handshake offers h2, every current browser takes it, and an h1 parser then gets \
             an h2 preface — the dashboard is blank for everyone"
        );
        assert!(
            !find_tokens(src, &["alpn_protocols", "=", "vec", "!"]).is_empty(),
            "`serve_http1_only` no longer sets `alpn_protocols`, so it does nothing at all — \
             `axum-server` hard-codes `[h2, http/1.1]` in `config_from_der` and only an explicit \
             overwrite removes h2"
        );
    }

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

        // `.route(` only: `.nest(` and `.fallback(` are not routes, and the fallback is the static
        // asset handler, which is deliberately public. `.route_layer(` does not match — the `(` is
        // part of the needle.
        //
        // **The path literal is not necessarily the next character.** This scanned for `.route("`
        // until a per-route `.layer(...)` pushed two registrations past rustfmt's 100 columns and
        // it reformatted them across four lines each. Those two routes then matched nothing, and
        // the failure was loud only because they were already in `UNAUTHENTICATED`. The dangerous
        // direction is silent: a *new* multi-line route that nobody adds to the list is absent from
        // both sides of the comparison, so the guard passes while an unauthenticated route sits
        // there unguarded. Any per-route middleware — a body limit, a rate limit, a timeout —
        // triggers that reformatting, so it is an ordinary edit rather than an exotic one.
        let mut found: Vec<&str> = open
            .match_indices(".route(")
            .map(|(i, m)| {
                let rest = open[i + m.len()..].trim_start();
                let rest = rest.strip_prefix('"').unwrap_or_else(|| {
                    panic!(
                        "a `.route(` whose first argument is not a string literal: {}",
                        &rest[..rest.len().min(60)]
                    )
                });
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
