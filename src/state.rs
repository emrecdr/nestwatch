//! Shared application state, injected into every handler via axum's `State` extractor.
//!
//! Each field is an `Arc` so cloning the state (which axum does per request) is cheap and
//! all handlers share the same controller, config, and login limiter.

use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::audit::AuditLog;
use crate::auth::LoginLimiter;
use crate::config::Config;
use crate::control::SystemControl;

#[derive(Clone)]
pub struct AppState {
    /// The OS abstraction — real on Windows, fake elsewhere.
    pub control: Arc<dyn SystemControl>,
    /// The single source of truth for all persisted settings (port, password hash, curfew,
    /// and every runtime-editable option). Handlers mutate it via `api::update_config`, which
    /// persists off the runtime; the enforcer reads it each tick. `port`/`password_hash` are
    /// simply never written.
    pub config: Arc<RwLock<Config>>,
    /// Brute-force protection for the login endpoint.
    pub limiter: Arc<LoginLimiter>,
    /// Serializes login attempts so limiter check + verify + record is atomic, and only one
    /// (memory-hard) Argon2 verification runs at a time.
    pub login_lock: Arc<tokio::sync::Mutex<()>>,
    /// Append-only security audit log (login attempts + sensitive actions).
    pub audit: Arc<AuditLog>,
}

impl AppState {
    /// Assemble the shared state from a loaded [`Config`] and a chosen controller. The config
    /// goes behind one `RwLock` (the single source of truth) and default login-protection is
    /// installed. This is the single place the aggregate is built, so `run`, the service, and
    /// tests can't drift.
    pub fn new(control: Arc<dyn SystemControl>, config: Config) -> Self {
        let audit = Arc::new(AuditLog::new(
            crate::config::data_paths().dir.join("audit.jsonl"),
        ));
        Self {
            control,
            config: Arc::new(RwLock::new(config)),
            limiter: Arc::new(LoginLimiter::default()),
            login_lock: Arc::new(tokio::sync::Mutex::new(())),
            audit,
        }
    }
}

/// Read-lock a curfew (or any) `RwLock`, recovering the inner value if a writer panicked.
/// The guarded data is always internally consistent, so a poisoned lock is safe to reuse
/// rather than propagate — a panicked writer must not permanently wedge curfew reads.
pub fn recover_read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Write-lock counterpart of [`recover_read`].
pub fn recover_write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
