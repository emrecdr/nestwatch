//! Authentication: password hashing/verification, session-based login, the middleware
//! that guards `/api/*`, and a brute-force limiter for the login endpoint.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Value, json};
use tower_sessions::Session;

use crate::error::AppError;
use crate::state::AppState;

/// Session key holding the "logged in" flag.
const AUTH_KEY: &str = "authenticated";

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

/// Rate-limits login attempts. Because there is exactly one user/password, a single
/// global counter is sufficient (no per-IP bookkeeping needed on a home LAN).
///
/// Policy (the tunable bit): after `max_failures` consecutive wrong passwords, logins are
/// refused for `lockout` before another attempt is allowed. A correct password resets the
/// counter immediately.
pub struct LoginLimiter {
    inner: Mutex<LimiterState>,
    max_failures: u32,
    lockout: Duration,
}

struct LimiterState {
    consecutive_failures: u32,
    locked_until: Option<Instant>,
}

impl LoginLimiter {
    pub fn new(max_failures: u32, lockout: Duration) -> Self {
        Self {
            inner: Mutex::new(LimiterState {
                consecutive_failures: 0,
                locked_until: None,
            }),
            max_failures,
            lockout,
        }
    }

    /// `Ok(())` if a login attempt is currently allowed, `Err` if locked out.
    pub fn check(&self) -> Result<(), AppError> {
        let state = self.inner.lock().unwrap();
        match state.locked_until {
            Some(until) if Instant::now() < until => Err(AppError::TooManyAttempts),
            _ => Ok(()),
        }
    }

    pub fn record_failure(&self) {
        let mut state = self.inner.lock().unwrap();
        state.consecutive_failures += 1;
        if state.consecutive_failures >= self.max_failures {
            state.locked_until = Some(Instant::now() + self.lockout);
            state.consecutive_failures = 0;
        }
    }

    pub fn record_success(&self) {
        let mut state = self.inner.lock().unwrap();
        state.consecutive_failures = 0;
        state.locked_until = None;
    }
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
    session: Session,
    Json(body): Json<LoginRequest>,
) -> Result<Json<Value>, AppError> {
    // Serialize attempts: makes limiter check→verify→record atomic (so concurrent requests
    // can't all slip past the gate) and ensures only one Argon2 verify runs at a time.
    let _guard = state.login_lock.lock().await;

    state.limiter.check()?;

    // Argon2 is memory-hard/CPU-heavy — never run it on the async runtime.
    let hash = state.config.password_hash.clone();
    let candidate = body.password;
    let ok = tokio::task::spawn_blocking(move || verify_password(&candidate, &hash)).await?;

    if ok {
        state.limiter.record_success();
        // Rotate the session id on privilege change (defeats session fixation).
        session.cycle_id().await?;
        session.insert(AUTH_KEY, true).await?;
        Ok(Json(json!({ "ok": true })))
    } else {
        state.limiter.record_failure();
        Err(AppError::Unauthorized)
    }
}

/// `POST /logout` — clear the session.
pub async fn logout(session: Session) -> Result<Json<Value>, AppError> {
    session.flush().await?;
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
        let limiter = LoginLimiter::new(3, Duration::from_secs(60));
        assert!(limiter.check().is_ok());
        limiter.record_failure();
        limiter.record_failure();
        assert!(limiter.check().is_ok(), "still allowed below threshold");
        limiter.record_failure(); // 3rd failure trips the lockout
        assert!(limiter.check().is_err(), "locked out at threshold");

        // A short lockout for testing clears the state on success.
        let limiter = LoginLimiter::new(1, Duration::from_secs(60));
        limiter.record_failure();
        assert!(limiter.check().is_err());
        limiter.record_success();
        assert!(limiter.check().is_ok(), "success clears the lockout");
    }
}
