//! Shared application state, injected into every handler via axum's `State` extractor.
//!
//! Each field is an `Arc` so cloning the state (which axum does per request) is cheap and
//! all handlers share the same controller, config, and login limiter.

use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::audit::AuditLog;
use crate::auth::LoginLimiter;
use crate::config::Config;
use crate::control::SystemControl;
use crate::screentime::ScreentimeLog;
use crate::sessionstore::FileSessionStore;
use crate::timecode::TimeCodes;
use crate::timereq::{SubmitLimiter, TimeRequests};
use crate::usage::UsageLog;

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
    /// Serializes `api::update_config` so mutate-then-persist is one critical section.
    ///
    /// The `config` lock above cannot do this job: it is a std `RwLock`, so it must be released
    /// before the `.await` that persists, which leaves a window where a second handler mutates
    /// and saves in between. Both writes then land, but in whichever order the blocking pool
    /// finishes them — so the *earlier* snapshot can be written last, silently reverting the
    /// later change on disk while memory still shows it. The parent sees their setting applied,
    /// and it is gone after the next restart.
    ///
    /// This is the same shape as `login_lock`, for the same reason: a read-modify-write that
    /// spans an await is only atomic if something holds across the await.
    pub config_save_lock: Arc<tokio::sync::Mutex<()>>,
    /// Fan-out for "something a dashboard is showing has changed".
    ///
    /// The dashboard polls two endpoints every 60 seconds, which sets the floor on how long a
    /// child waits after asking for more time — the one interaction on this service that a person
    /// is actually sitting in front of. This carries a tag naming *what* changed; the client
    /// refetches the endpoint it already uses, so the JSON routes stay the single source of truth
    /// and the stream is only ever a hint to ask again.
    ///
    /// `broadcast` rather than a per-client queue: a send with no subscribers is a no-op, so a
    /// handler never blocks or fails because nobody is watching, and a slow reader lags rather
    /// than stalling the writer. Tags are `&'static str`, so a notification allocates nothing.
    pub events: tokio::sync::broadcast::Sender<&'static str>,
    /// Append-only security audit log (login attempts + sensitive actions).
    pub audit: Arc<AuditLog>,
    /// Collapses the live view's timer-driven frames into one audit line per window, so a timer
    /// cannot evict the security history. See [`crate::audit::LiveViewAudit`].
    pub live_audit: Arc<crate::audit::LiveViewAudit>,
    /// Append-only usage-history log (daily screen-time, sessions, enforcement events).
    pub usage: Arc<UsageLog>,
    /// Append-only daily screen-time rollups. Separate from `usage` so point-in-time events
    /// cannot rotate the daily history out — see `screentime.rs`.
    pub screentime: Arc<ScreentimeLog>,
    /// The child's "request more time" queue (parent approves/denies in the dashboard).
    pub time_requests: Arc<TimeRequests>,
    /// Per-IP throttle for the unauthenticated child request endpoint.
    pub time_req_limiter: Arc<SubmitLimiter>,
    /// Parent-minted, single-use redeemable time codes the child can cash in on the LAN page.
    pub time_codes: Arc<TimeCodes>,
    /// Per-IP throttle for the unauthenticated code-redeem endpoint (also blunts brute-forcing).
    pub code_limiter: Arc<SubmitLimiter>,
    /// Per-IP throttle for the child's unauthenticated `/status` poll. More generous than the
    /// others (the page polls once a minute) but present, because each call does file I/O on the
    /// shared blocking pool.
    pub status_limiter: Arc<SubmitLimiter>,
    /// Tells the two enforcers to re-evaluate immediately, rather than at their next 30s tick.
    ///
    /// Sent by `api::try_update_config` — the single place config is mutated — so every parent
    /// action that can invalidate a pending shutdown reaches the loops promptly: extending
    /// bedtime, granting screen time, pausing the rules, switching curfew off. Without it the
    /// abort could arrive after the warning countdown had already expired.
    pub enforcement_wake: Arc<crate::heartbeat::Waker>,
    /// Login sessions, persisted so a service restart or reboot doesn't sign the parent out.
    /// Held here (rather than built inside `build_router`) so tests can swap in an ephemeral
    /// store and never touch the real data dir.
    pub sessions: FileSessionStore,
}

impl AppState {
    /// Assemble the shared state from a loaded [`Config`] and a chosen controller. The config
    /// goes behind one `RwLock` (the single source of truth) and default login-protection is
    /// installed. This is the single place the aggregate is built, so `run`, the service, and
    /// tests can't drift.
    pub fn new(control: Arc<dyn SystemControl>, config: Config) -> Self {
        let crate::config::DataPaths { dir, sessions, .. } = crate::config::data_paths();
        let sessions = FileSessionStore::new(sessions);
        let audit = Arc::new(AuditLog::new(dir.join("audit.jsonl")));
        let usage = Arc::new(UsageLog::new(dir.join("usage.jsonl")));
        let screentime = Arc::new(ScreentimeLog::new(dir.join("screentime.jsonl")));
        let time_requests = Arc::new(TimeRequests::new(dir.join("time_requests.jsonl")));
        let time_codes = Arc::new(TimeCodes::new(dir.join("time_codes.jsonl")));
        Self {
            control,
            config: Arc::new(RwLock::new(config)),
            limiter: Arc::new(LoginLimiter::default()),
            login_lock: Arc::new(tokio::sync::Mutex::new(())),
            config_save_lock: Arc::new(tokio::sync::Mutex::new(())),
            audit,
            live_audit: Arc::new(crate::audit::LiveViewAudit::default()),
            // Capacity 16: a lagging subscriber loses old tags rather than blocking anyone, and a
            // dropped tag costs at most one stale panel until the 60s poll catches it. The stream
            // is a hint to refetch, so losing one is a delay, never a wrong number on screen.
            events: tokio::sync::broadcast::channel(16).0,
            usage,
            screentime,
            time_requests,
            time_req_limiter: Arc::new(SubmitLimiter::default()),
            time_codes,
            code_limiter: Arc::new(SubmitLimiter::default()),
            // 30/min: the /ask page polls once a minute plus a refresh after each redeem, so this
            // is far above real use while still capping a scripted flood.
            status_limiter: Arc::new(SubmitLimiter::new(30, std::time::Duration::from_secs(60))),
            enforcement_wake: Arc::new(tokio::sync::watch::channel(0).0),
            sessions,
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
