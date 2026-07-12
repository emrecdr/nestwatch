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

    pub fn record_failure(&self, ip: IpAddr) {
        let now = Instant::now();
        let mut map = self.map();
        prune(&mut map, now);
        let a = map.entry(ip).or_default();
        a.consecutive_failures += 1;
        if a.consecutive_failures >= self.max_failures {
            a.locked_until = Some(now + self.lockout);
            a.consecutive_failures = 0;
        }
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

    if let Err(e) = state.limiter.check(ip) {
        state.audit.record(
            "auth_failure",
            json!({ "src_ip": ip, "reason": "rate_limited" }),
        );
        return Err(e);
    }

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
        state.limiter.record_failure(ip);
        state.audit.record(
            "auth_failure",
            json!({ "src_ip": ip, "reason": "bad_password" }),
        );
        Err(AppError::Unauthorized)
    }
}

/// `POST /logout` — clear the session.
pub async fn logout(
    State(state): State<AppState>,
    session: Session,
) -> Result<Json<Value>, AppError> {
    session.flush().await?;
    state.audit.record("logout", json!({}));
    Ok(Json(json!({ "ok": true })))
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

/// Middleware guarding `/api/*`: 401 unless the session is authenticated.
pub async fn require_auth(
    session: Session,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let authenticated = session.get::<bool>(AUTH_KEY).await?.unwrap_or(false);

    if authenticated {
        Ok(next.run(request).await)
    } else {
        Err(AppError::Unauthorized)
    }
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
