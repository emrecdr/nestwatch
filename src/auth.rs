//! Authentication: password hashing/verification, session-based login, the middleware
//! that guards `/api/*`, and a brute-force limiter for the login endpoint.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::HeaderMap;
use axum::http::header;
use axum::middleware::Next;
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Value, json};
use tower_sessions::Session;
use tower_sessions::cookie::time::OffsetDateTime;

use crate::error::AppError;
use crate::state::AppState;

/// Session key holding the "logged in" flag.
const AUTH_KEY: &str = "authenticated";

/// Minimum control-password length, enforced at install and on password change.
pub const MIN_PASSWORD_LEN: usize = 10;

// ---------------------------------------------------------------------------
// Password hashing (Argon2id)
// ---------------------------------------------------------------------------

/// Hash a plaintext password into a PHC string for storage (used at install time).
pub fn hash_password(password: &str) -> anyhow::Result<String> {
    use argon2::password_hash::SaltString;
    use argon2::password_hash::rand_core::OsRng;
    use argon2::{Argon2, PasswordHasher};

    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("failed to hash password: {e}"))?;
    Ok(hash.to_string())
}

/// Constant-time verification of a candidate password against a stored PHC hash.
pub fn verify_password(password: &str, phc_hash: &str) -> bool {
    use argon2::{Argon2, PasswordHash, PasswordVerifier};

    match PasswordHash::new(phc_hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Brute-force limiter
// ---------------------------------------------------------------------------

/// Rate-limits login attempts **per source IP**. A global counter would let any device on
/// the LAN lock out the legitimate parent (a denial-of-service the OWASP guidance warns
/// about), so failures are tracked per client: a device that spams wrong passwords throttles
/// only itself. The real barrier against guessing is the strong Argon2id password plus the
/// single-verify-at-a-time serialization in [`login`]; this limiter is abuse control.
///
/// Policy (the tunable bit): after `max_failures` consecutive wrong passwords from one IP,
/// that IP is refused for `lockout`. A correct password clears that IP's state immediately.
pub struct LoginLimiter {
    inner: Mutex<HashMap<IpAddr, Attempts>>,
    max_failures: u32,
    lockout: Duration,
}

#[derive(Default)]
struct Attempts {
    consecutive_failures: u32,
    locked_until: Option<Instant>,
}

impl LoginLimiter {
    pub fn new(max_failures: u32, lockout: Duration) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            max_failures,
            lockout,
        }
    }

    /// Lock the map, recovering from poison rather than panicking (mirrors
    /// [`crate::state::recover_read`]). Critical sections here are trivial and can't panic,
    /// and the release build aborts on panic anyway, so poison is a dev/test-only concern.
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<IpAddr, Attempts>> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// `Ok(())` if `ip` may attempt a login now, `Err` if it is currently locked out.
    pub fn check(&self, ip: IpAddr) -> Result<(), AppError> {
        match self.map().get(&ip).and_then(|a| a.locked_until) {
            Some(until) if Instant::now() < until => Err(AppError::TooManyAttempts),
            _ => Ok(()),
        }
    }

    /// Record a wrong password. Returns `true` if this attempt *triggered* the lockout, so the
    /// caller can audit that transition exactly once (see [`login`] — auditing every rejected
    /// request instead would let anyone flood the audit log off disk).
    pub fn record_failure(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut map = self.map();
        prune(&mut map, now);
        let a = map.entry(ip).or_default();
        a.consecutive_failures += 1;
        if a.consecutive_failures >= self.max_failures {
            a.locked_until = Some(now + self.lockout);
            a.consecutive_failures = 0;
            return true;
        }
        false
    }

    pub fn record_success(&self, ip: IpAddr) {
        self.map().remove(&ip);
    }
}

/// Drop entries that are neither failing nor currently locked, so the map stays bounded to
/// the handful of IPs actively misbehaving (tiny on a home LAN).
fn prune(map: &mut HashMap<IpAddr, Attempts>, now: Instant) {
    map.retain(|_, a| a.consecutive_failures > 0 || a.locked_until.is_some_and(|u| now < u));
}

impl Default for LoginLimiter {
    fn default() -> Self {
        // 5 wrong tries → locked out for 60 seconds.
        Self::new(5, Duration::from_secs(60))
    }
}

// ---------------------------------------------------------------------------
// Handlers + middleware
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct LoginRequest {
    password: String,
}

/// `POST /login` — verify the password and mark the session authenticated.
pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    session: Session,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Result<Json<Value>, AppError> {
    let ip = peer.ip();
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Serialize attempts: makes limiter check→verify→record atomic (so concurrent requests
    // can't all slip past the gate) and ensures only one Argon2 verify runs at a time.
    let _guard = state.login_lock.lock().await;

    // Deliberately NOT audited: this path is reached once per request while an IP is locked out
    // and short-circuits before Argon2, so it is nearly free to trigger. Recording it would let
    // anyone on the LAN append unbounded lines and roll the audit log (and its single backup)
    // off disk in seconds, destroying the record of every real login, kill, and shutdown. The
    // lockout itself is audited once, on the transition, below.
    state.limiter.check(ip)?;

    // Argon2 is memory-hard/CPU-heavy — never run it on the async runtime.
    let hash = crate::state::recover_read(&state.config)
        .password_hash
        .clone();
    let candidate = body.password;
    let ok = tokio::task::spawn_blocking(move || verify_password(&candidate, &hash)).await?;

    if ok {
        state.limiter.record_success(ip);
        state
            .audit
            .record("auth_success", json!({ "src_ip": ip, "user_agent": ua }));
        // Rotate the session id on privilege change (defeats session fixation).
        session.cycle_id().await?;
        session.insert(AUTH_KEY, true).await?;
        Ok(Json(json!({ "ok": true })))
    } else {
        let locked_out = state.limiter.record_failure(ip);
        state.audit.record(
            "auth_failure",
            json!({ "src_ip": ip, "reason": "bad_password", "locked_out": locked_out }),
        );
        Err(AppError::Unauthorized)
    }
}

/// `POST /logout` — clear the session.
pub async fn logout(
    State(state): State<AppState>,
    session: Session,
) -> Result<Json<Value>, AppError> {
    // Only audit a logout that actually ended a signed-in session. `/logout` is unauthenticated
    // and unthrottled, and needs neither a body nor a valid cookie — auditing every call let
    // anyone on the LAN append unbounded lines and roll the audit log (plus its single backup)
    // off disk in about 20 seconds, destroying the record of every login, kill and shutdown.
    // Third instance of this bug class here, after `login` and `pair`.
    let was_authenticated = session.get::<bool>(AUTH_KEY).await?.unwrap_or(false);
    session.flush().await?;
    if was_authenticated {
        state.audit.record("logout", json!({}));
    }
    Ok(Json(json!({ "ok": true })))
}

/// `GET /p/{token}` — redeem a one-time pairing token from the install QR code and land in the
/// dashboard already signed in. Unauthenticated (that's the point) but LAN-gated like every
/// other route, and rate-limited on the same per-IP limiter as `login` so the 80-bit token can't
/// be ground at speed.
///
/// **Always redirects to `/`**, whether or not the token was valid: a failed pair simply shows
/// the login page. Returning a distinguishable error would confirm to a guesser that a pairing
/// is currently pending, and the parent doesn't benefit from the distinction either — if the
/// scan didn't work they just sign in normally. Redirecting also strips the token out of the
/// address bar, so it doesn't linger in browser history.
pub async fn pair(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    session: Session,
    axum::extract::Path(token): axum::extract::Path<String>,
) -> Result<Response, AppError> {
    use axum::response::IntoResponse;
    let ip = peer.ip();
    let redirect = || Ok(axum::response::Redirect::to("/").into_response());

    // Same lock `login` uses, for the same reason: without it, check → redeem → record_failure
    // is not atomic, so a burst of concurrent guesses all pass the gate before any of them
    // registers a failure. Measured: 500 concurrent bad tokens got 66 attempts past a 5-per-60s
    // limiter. Pairing is rare, so serializing it costs nothing.
    let _guard = state.login_lock.lock().await;

    if state.limiter.check(ip).is_err() {
        return redirect();
    }

    let path = crate::config::data_paths().pairing;
    // Off-runtime: this touches the filesystem (read + unlink).
    let ok = tokio::task::spawn_blocking(move || crate::pairing::redeem(&path, &token)).await?;

    if !ok {
        // Audit the lockout transition only, never the individual attempt: a per-attempt record
        // lets anyone on the LAN append unbounded lines and roll the audit log off disk. Same
        // reasoning as the rate-limited branch of `login`.
        if state.limiter.record_failure(ip) {
            state.audit.record(
                "pair_failed",
                json!({ "src_ip": ip.to_string(), "locked_out": true }),
            );
        }
        return redirect();
    }

    state.limiter.record_success(ip);
    // Same privilege transition as a password login: rotate the id, then mark authenticated.
    session.cycle_id().await?;
    session.insert(AUTH_KEY, true).await?;
    state
        .audit
        .record("paired", json!({ "src_ip": ip.to_string() }));
    redirect()
}

/// `GET /session` — lets the UI decide whether to show login or the dashboard.
pub async fn me(session: Session) -> Json<Value> {
    let authenticated = session
        .get::<bool>(AUTH_KEY)
        .await
        .ok()
        .flatten()
        .unwrap_or(false);
    Json(json!({ "authenticated": authenticated }))
}

/// Session key holding the last time we refreshed the expiry (unix seconds). See [`require_auth`].
const SEEN_KEY: &str = "seen";

/// How stale the `seen` stamp may get before we refresh it. Every refresh costs one session
/// write, so this trades expiry precision for disk writes: a device used daily is written to
/// once every 5 days, not once per request.
const SLIDING_REFRESH_SECS: i64 = 5 * 86_400;

/// Middleware guarding `/api/*`: 401 unless the session is authenticated.
///
/// Also implements the *sliding* part of the "remember this device" expiry. `tower-sessions`
/// recomputes `expiry_date` only when a session is **saved**, and it only saves when the session
/// was **modified** — reading is explicitly not activity. So `Expiry::OnInactivity(30 days)`
/// alone behaves as a hard 30-day cutoff from login, even for someone using the dashboard daily.
/// Touching a timestamp here marks the session modified, which refreshes both the stored expiry
/// and the browser cookie. Stepped coarsely so this isn't a write per request.
pub async fn require_auth(
    session: Session,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let authenticated = session.get::<bool>(AUTH_KEY).await?.unwrap_or(false);
    if !authenticated {
        return Err(AppError::Unauthorized);
    }

    let now = OffsetDateTime::now_utc().unix_timestamp();
    let seen = session.get::<i64>(SEEN_KEY).await?.unwrap_or(0);
    if now.saturating_sub(seen) >= SLIDING_REFRESH_SECS {
        session.insert(SEEN_KEY, now).await?;
    }

    Ok(next.run(request).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_then_verify_round_trips() {
        let hash = hash_password("s3cret-pw").unwrap();
        assert!(verify_password("s3cret-pw", &hash));
        assert!(!verify_password("wrong", &hash));
    }

    #[test]
    fn limiter_locks_after_threshold_and_resets_on_success() {
        let ip: IpAddr = "192.168.1.10".parse().unwrap();
        let limiter = LoginLimiter::new(3, Duration::from_secs(60));
        assert!(limiter.check(ip).is_ok());
        limiter.record_failure(ip);
        limiter.record_failure(ip);
        assert!(limiter.check(ip).is_ok(), "still allowed below threshold");
        limiter.record_failure(ip); // 3rd failure trips the lockout
        assert!(limiter.check(ip).is_err(), "locked out at threshold");

        // A short lockout for testing clears the state on success.
        let limiter = LoginLimiter::new(1, Duration::from_secs(60));
        limiter.record_failure(ip);
        assert!(limiter.check(ip).is_err());
        limiter.record_success(ip);
        assert!(limiter.check(ip).is_ok(), "success clears the lockout");
    }

    #[test]
    fn one_ip_lockout_does_not_affect_another() {
        let attacker: IpAddr = "192.168.1.66".parse().unwrap();
        let parent: IpAddr = "192.168.1.20".parse().unwrap();
        let limiter = LoginLimiter::new(2, Duration::from_secs(60));

        // Attacker trips their own lockout…
        limiter.record_failure(attacker);
        limiter.record_failure(attacker);
        assert!(limiter.check(attacker).is_err(), "attacker locked out");

        // …but the parent's IP is unaffected (this is the DoS the global counter allowed).
        assert!(limiter.check(parent).is_ok(), "parent NOT locked out");
    }
}
