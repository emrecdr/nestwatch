//! Shared application state, injected into every handler via axum's `State` extractor.
//!
//! Each field is an `Arc` so cloning the state (which axum does per request) is cheap and
//! all handlers share the same controller, config, and login limiter.

use std::sync::{Arc, RwLock};

use crate::auth::LoginLimiter;
use crate::config::Config;
use crate::control::SystemControl;
use crate::curfew::Curfew;

#[derive(Clone)]
pub struct AppState {
    /// The OS abstraction — real on Windows, fake elsewhere.
    pub control: Arc<dyn SystemControl>,
    /// Startup config (immutable fields: port, password hash).
    pub config: Arc<Config>,
    /// Brute-force protection for the login endpoint.
    pub limiter: Arc<LoginLimiter>,
    /// Serializes login attempts so limiter check + verify + record is atomic, and only one
    /// (memory-hard) Argon2 verification runs at a time.
    pub login_lock: Arc<tokio::sync::Mutex<()>>,
    /// Curfew settings, editable at runtime from the dashboard and read by the enforcer.
    pub curfew: Arc<RwLock<Curfew>>,
}
