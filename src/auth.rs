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
use sha2::{Digest, Sha256};
use tower_sessions::Session;
use tower_sessions::cookie::time::OffsetDateTime;
use tower_sessions::session::{Id, Record};

use crate::error::AppError;
use crate::state::AppState;

/// Session key holding the "logged in" flag.
const AUTH_KEY: &str = "authenticated";

/// How long a session survives without use, in days.
///
/// Owned here rather than written inline where the session layer is built, because
/// [`describe_session`] *derives* last activity by subtracting it from the stored expiry. Two
/// copies of this number would not fail a test — they would quietly shift every "last seen" in
/// the parent's dashboard by the difference, which is a wrong answer that looks like a right one.
///
/// Thirty days is at NIST SP 800-63B's ceiling for a single-factor (AAL1) session, which is what
/// a password-only login is. It was chosen when a stolen cookie meant somebody already inside the
/// house; `docs/REMOTE-ACCESS.md` records that this premise is what remote access changes, and
/// per-device revocation is the compensation that made keeping it defensible.
pub const SESSION_IDLE_DAYS: i64 = 30;

/// Session key holding how this device announced itself, for the *Signed-in devices* card.
///
/// A JSON object: `first_seen` (unix seconds) and `user_agent`. Written where the session is
/// created, so it describes the device that signed in rather than whichever one asked last.
///
/// **Not the source IP, and that is not an oversight.** `docs/REMOTE-ACCESS.md` records that a
/// router which masquerades tunnel traffic collapses every remote visitor into one address — so
/// under exactly the deployment this identity exists to serve, an IP identifies the router and
/// not the device. OWASP's session guidance says the same thing from the other direction:
/// binding a session to client properties is a detection layer, "a skilled attacker can bypass
/// these controls" through NAT, and it is not a primary defence. The identity here is therefore
/// *minted with the session* and carried in it, never inferred from the packet.
const DEVICE_KEY: &str = "device";

/// Session key holding what this session is *allowed to do* — see [`crate::pairing::Scope`].
///
/// Until `O89` this did not exist, and [`AUTH_KEY`] was the whole authorisation model: one
/// boolean, written identically by [`login`] and [`pair`], read by [`require_auth`]. That made a
/// paired device and the parent indistinguishable, which was fine while the only things pairing
/// were the parent's own browser and phone — and stopped being fine when a third-party
/// application started keeping the cookie and replaying it.
///
/// **A session carrying [`AUTH_KEY`] but not this key is refused.** That is the migration, and it
/// is deliberately the closed direction: every session predating this change is unscoped, and
/// honouring an unscoped session would leave the hole open in precisely the installs that have
/// one. The cost is a password re-login in the browser and one fresh QR per paired device; the
/// alternative is a fix that does not apply to anybody who already has the bug.
const SCOPE_KEY: &str = "scope";

/// Minimum control-password length, enforced at install and on password change.
///
/// Eight, not more, on purpose. This password guards a LAN-only service behind an Argon2id hash
/// with per-IP throttling — an attacker has to already be on the home network to try it at all,
/// and gets a handful of guesses a minute. A longer minimum mostly buys a password on a sticky
/// note, which is a worse outcome than a short one the parent can actually recall.
pub const MIN_PASSWORD_LEN: usize = 8;

/// Why a proposed password was rejected — with the numbers, so the message can name them.
///
/// Length is counted in `char`s, not bytes: a password of eight accented letters is eight
/// characters to the person typing it and up to sixteen bytes to `len()`. Reporting bytes would
/// tell a parent their eight-character password is "sixteen characters", which is worse than
/// saying nothing.
#[derive(Debug, PartialEq, Eq)]
pub enum PasswordProblem {
    /// Nothing was entered.
    Empty,
    /// Shorter than [`MIN_PASSWORD_LEN`]; carries the count actually measured.
    TooShort { got: usize },
    /// Long enough, but a password an attacker would reach early. Carries the pattern named, so
    /// the message can say *which* rule it tripped rather than "too weak".
    Guessable { why: &'static str },
}

impl PasswordProblem {
    /// The message shown to the parent. States what was measured rather than only what was
    /// required: "at least 8 characters" cannot be acted on by someone who believes they typed
    /// ten, which is exactly the report that prompted this. A count they can compare against
    /// their own turns an argument into an observation.
    pub fn message(&self) -> String {
        match self {
            Self::Empty => format!(
                "no password entered — nestwatch needs at least {MIN_PASSWORD_LEN} characters.\n\
                 If you were pasting, the terminal may not have received it; try typing it."
            ),
            Self::TooShort { got } => {
                let short_by = MIN_PASSWORD_LEN - got;
                format!(
                    "that password is too short.\n  \
                     counted:  {got} character{}\n  \
                     minimum:  {MIN_PASSWORD_LEN} characters\n  \
                     add {short_by} more character{}.\n\
                     If that count is lower than what you typed, a keystroke did not reach the \
                     terminal — retype it slowly rather than pasting.",
                    if *got == 1 { "" } else { "s" },
                    if short_by == 1 { "" } else { "s" },
                )
            }
            Self::Guessable { why } => format!(
                "that password is long enough, but {why}.\n\
                 It is not the length that is the problem — a guess like that is tried in the \
                 first handful of attempts.\n\
                 Two or three unrelated words work well: \"kettle-harbour-91\"."
            ),
        }
    }
}

/// Passwords an attacker tries first. Deliberately short: this is the head of the distribution,
/// not a dictionary. A full breach corpus is megabytes, and the shipped binary is optimised for
/// size — the patterns below plus per-IP throttling cover the realistic case, which is someone
/// guessing by hand at the console rather than running a wordlist over the LAN.
const COMMON: &[&str] = &[
    "password",
    "passw0rd",
    "p@ssword",
    "p@ssw0rd",
    "letmein",
    "welcome",
    "iloveyou",
    "sunshine",
    "princess",
    "football",
    "baseball",
    "superman",
    "batman",
    "dragon",
    "monkey",
    "trustno1",
    "qwerty",
    "qwertyuiop",
    "azerty",
    "asdfgh",
    "asdfghjkl",
    "zxcvbnm",
    "1q2w3e4r",
    "qazwsx",
    "abc123",
    "123abc",
    "admin",
    "administrator",
    "changeme",
    "default",
    "secret",
    "master",
    "computer",
    "internet",
    "whatever",
    "freedom",
    "starwars",
    "pokemon",
    "minecraft",
    "roblox",
    "nestwatch",
    "hosthealth",
    "parent",
    "family",
    "screentime",
];

/// Reject the passwords a child at the keyboard would actually try, without imposing composition
/// rules.
///
/// NIST SP 800-63B Rev 4 (final, July 2025) *prohibits* requiring mixed character classes and
/// requires a blocklist instead — so there is deliberately no "must contain a digit" rule here.
/// An all-digit password is allowed: eight digits is 10^8, and against Argon2id behind per-IP
/// throttling that is not the weak link. `12345678` is, and that is what this catches.
fn guessable(pw: &str) -> Option<&'static str> {
    let lower = pw.to_lowercase();

    if COMMON.contains(&lower.as_str()) {
        return Some("it is one of the passwords tried first in any guessing attempt");
    }
    // A common word with digits pinned on the end ("password123", "dragon2024") is the same
    // guess with one more step, so strip a trailing run of digits and re-check.
    let stem = lower.trim_end_matches(|c: char| c.is_ascii_digit());
    if stem.len() >= 4 && COMMON.contains(&stem) {
        return Some("it is a common password with digits added, which is guessed just as early");
    }
    let chars: Vec<char> = lower.chars().collect();
    if chars.windows(2).all(|w| w[0] == w[1]) {
        return Some("it is the same character repeated");
    }
    // Runs in either direction: 12345678, 87654321, abcdefgh.
    let run = |step: i32| {
        chars
            .windows(2)
            .all(|w| (w[1] as i32) - (w[0] as i32) == step)
    };
    if run(1) || run(-1) {
        return Some("it is a single run of consecutive characters");
    }
    // A short block repeated to reach the length ("abcabcabc", "12121212") has only as much
    // variety as the block.
    for block in 1..=chars.len() / 2 {
        if chars.len().is_multiple_of(block) && chars.chunks(block).all(|c| c == &chars[..block]) {
            return Some("it is a short sequence repeated to fill the length");
        }
    }
    None
}

/// Check a proposed password, returning what is wrong with it rather than a bare bool.
pub fn check_password(pw: &str) -> Result<(), PasswordProblem> {
    let got = pw.chars().count();
    if got == 0 {
        return Err(PasswordProblem::Empty);
    }
    if got < MIN_PASSWORD_LEN {
        return Err(PasswordProblem::TooShort { got });
    }
    if let Some(why) = guessable(pw) {
        return Err(PasswordProblem::Guessable { why });
    }
    Ok(())
}

/// A warning about a password that is *acceptable* but probably not what the parent meant.
///
/// Never rejects and never rewrites: silently trimming a password would mean the one that works
/// at install is not the one they typed. Leading and trailing spaces are invisible in a masked
/// prompt and are the classic paste artifact, so they are worth naming out loud.
pub fn password_caution(pw: &str) -> Option<String> {
    let lead = pw.starts_with(char::is_whitespace);
    let trail = pw.ends_with(char::is_whitespace);
    let where_ = match (lead, trail) {
        (true, true) => "starts and ends",
        (true, false) => "starts",
        (false, true) => "ends",
        (false, false) => return None,
    };
    Some(format!(
        "note: your password {where_} with a space. That is allowed and it counts as a \
         character — but it is invisible, and you will have to type it every time you sign in."
    ))
}

/// Explain why two entries differ, without echoing either of them.
///
/// The lengths are safe to show: the parent is at their own console, has just typed both, and
/// "12 vs 11" points straight at a trailing keystroke while "both 12" points at a typo in the
/// middle. Bailing with a bare "passwords do not match" makes them guess.
pub fn describe_mismatch(first: &str, second: &str) -> String {
    let (a, b) = (first.chars().count(), second.chars().count());
    let detail = if a == b {
        format!("both are {a} characters, so one character differs somewhere in the middle")
    } else {
        format!("the first is {a} characters, the confirmation is {b}")
    };
    format!("the two entries do not match — {detail}.")
}

// ---------------------------------------------------------------------------
// Password hashing (Argon2id)
// ---------------------------------------------------------------------------

/// Hash a plaintext password into a PHC string for storage (used at install time).
/// Hash a password with Argon2id at the library defaults.
///
/// # The defaults are the policy, and that was checked rather than assumed
///
/// `Argon2::default()` is Argon2id, `m = 19456 KiB (19 MiB)`, `t = 2`, `p = 1`, 32-byte output —
/// read out of `argon2 0.6.0`'s `Params::DEFAULT` rather than inferred. That is **exactly** OWASP's
/// current minimum configuration in the Password Storage Cheat Sheet, whose two sanctioned options
/// are `m=19456, t=2, p=1` and `m=47104, t=1, p=1`. So this needs no tuning today; what it needs is
/// for the next person to know it is a deliberate match and not an untouched default.
///
/// # Raising it later is safe, and this is the non-obvious part
///
/// Verification does **not** use these parameters. `password-hash`'s blanket `PasswordVerifier`
/// impl calls `T::Params::try_from(hash)`, so it re-derives from the parameters stored in the PHC
/// string itself. Raising the cost here therefore cannot lock out an existing install — old hashes
/// keep verifying at the strength they were made with.
///
/// The flip side is the thing to remember: there is **no rehash-on-login**, so an install created
/// today would keep a 19 MiB hash forever after a future bump, with nothing on screen saying so.
/// Not implemented, deliberately — it would put a config write on the login path to buy nothing
/// while the parameters sit at policy. If they are ever raised, that is the moment to add it, and
/// this paragraph is why.
///
/// # The salt is no longer this function's business
///
/// `password-hash 0.6` moved salt generation inside `hash_password`, which now draws
/// `RECOMMENDED_SALT_LEN` bytes straight from `getrandom` and returns an error if the OS random
/// source fails. That is **16 bytes — byte-for-byte what the old `SaltString::generate(&mut
/// OsRng)` produced**, checked rather than assumed, so nothing about the stored hash got weaker
/// when the two lines above it disappeared.
///
/// It is also one less thing to hold right: a caller can no longer pass a reused, short, or
/// non-random salt, because there is nowhere left to pass one.
pub fn hash_password(password: &str) -> anyhow::Result<String> {
    use argon2::{Argon2, PasswordHasher};

    let hash = Argon2::default()
        .hash_password(password.as_bytes())
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

/// Wrong tries before a device is locked out.
///
/// Named rather than left as literals inside `default()`, because the lockout duration is also
/// stated to the person it locks out — `app.js` tells them to "wait a minute" — and a number
/// buried in a constructor call is not something a reader or a test can connect that sentence to.
/// A sibling repository lost this exact pairing with no constant at all, where the only trace of
/// the limit was the word "minute" inside a sentence: a literal at least announces itself as a
/// number, prose does not manage even that.
pub const LOGIN_MAX_FAILS: u32 = 5;
/// How long a device stays locked out after [`LOGIN_MAX_FAILS`] wrong tries.
///
/// Pinned against the message a parent reads by
/// `web::tests::the_lockout_a_parent_is_told_to_wait_matches_the_one_enforced`.
pub const LOGIN_LOCKOUT: Duration = Duration::from_secs(60);

impl Default for LoginLimiter {
    fn default() -> Self {
        Self::new(LOGIN_MAX_FAILS, LOGIN_LOCKOUT)
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
        // The password is the parent's, so this is the unrestricted scope. Written here rather
        // than defaulted in `require_auth`, so that "no scope" keeps meaning "refuse".
        session
            .insert(SCOPE_KEY, crate::pairing::Scope::Dashboard)
            .await?;
        remember_device(&session, &headers).await?;
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
    headers: HeaderMap,
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
    let redeemed =
        tokio::task::spawn_blocking(move || crate::pairing::redeem(&path, &token)).await?;

    let Some(scope) = redeemed else {
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
    };

    state.limiter.record_success(ip);
    // Rotate the id, mark authenticated, and record **what this pairing is worth**. That last
    // step is the one this used to be missing: without it `pair` and `login` left identical
    // session state, so a paired device held the parent's whole capability table and an
    // integration could grant as `source=parent`, skipping the registry and the day latch.
    session.cycle_id().await?;
    session.insert(AUTH_KEY, true).await?;
    session.insert(SCOPE_KEY, scope.clone()).await?;
    remember_device(&session, &headers).await?;
    // The audit says which kind, because "a device paired" and "an integration was installed on
    // a device" are different events to meet in a log a month later.
    let kind = match &scope {
        crate::pairing::Scope::Dashboard => json!("dashboard"),
        crate::pairing::Scope::Integration { source } => json!({ "integration": source }),
    };
    state
        .audit
        .record("paired", json!({ "src_ip": ip.to_string(), "scope": kind }));
    redirect()
}

/// `GET /session` — lets the UI decide whether to show login or the dashboard.
///
/// Also carries the running version, so the dashboard can say which build is on that PC. Until
/// now that was only answerable at the machine itself (`version`, or `doctor`'s header), which is
/// the wrong place: the parent asking "is this up to date?" is holding a phone, somewhere else.
///
/// The *number* travels; this machine never goes looking for a newer one, and that distinction
/// is the whole design. A version check from here would be the monitored PC contacting GitHub,
/// and "nothing leaves the house" is a promise in the README, on the project page, and in
/// `SECURITY.md`. The dashboard's check button asks GitHub from the *parent's* browser, on the
/// parent's own device, and only on a click — which is why nothing in this crate gained an
/// outbound client, and why `connect-src` names exactly one external host. See
/// [`crate::security`], where that is pinned by a test.
///
/// Sent whether or not the caller is signed in: it is the same string printed at install, on the
/// console, by an unauthenticated `version` command. It reveals nothing a LAN attacker could not
/// read off the login page's own assets.
///
/// # `scope`, and what it is honestly worth
///
/// Also reports what the session may do, because a pairing link does not say what it is worth:
/// a dashboard QR and an integration QR are byte-identical in form, and the scope lives in
/// `pairing.json` on this machine, never in the URL. So an integration app handed the *wrong* QR
/// pairs successfully and silently holds the parent's whole authority — which is precisely the
/// posture scoping removed. Reading this field lets that app notice and refuse.
///
/// **This defends against a mistake, not an attacker, and the difference matters.** Nothing here
/// can stop a client that declines to check, and an app that actually wanted parent authority
/// would simply ask the parent for a dashboard link. The guarantee is unchanged and is still the
/// credential: an integration pairing *cannot* reach the rest of the API whatever the client
/// believes. What this adds is that an honest client can tell it was given more than it asked
/// for, which is the case a household actually hits — a parent scanning whichever QR was on
/// screen. It is the client-side half of audience validation, which is the standing advice for
/// exactly this: a credential issued for one purpose must not be silently usable for another.
///
/// ## Against the nearest specification
///
/// The closest written thing is **RFC 7662, OAuth 2.0 Token Introspection**, and this departs
/// from it twice on purpose. Cited because it is the only standard description of "ask what a
/// credential is worth", not because this is an OAuth deployment — there is no authorisation
/// server here, no bearer token, no client registration and no refresh.
///
/// 1. *`scope` is an object; RFC 7662's is a space-separated string.* That string is an artifact
///    of OAuth carrying scopes through form-encoded parameters, and nothing here has that
///    constraint. The two facts a client needs are *what may it do* and *who is it*, which OAuth
///    splits across `scope` and `client_id`; inventing an OAuth vocabulary for a two-kind system
///    would promise semantics this service does not have. An object also makes absence
///    unambiguous — `null` is "no authority recorded", one value to test rather than two fields
///    to correlate.
///
/// 2. *The endpoint is unauthenticated; RFC 7662 says introspection MUST NOT be.* Its reason is
///    token scanning, and that attack needs a `token` **parameter** — somewhere to put a guess.
///    There is none here: a caller can only ask about the cookie they already present. `/session`
///    has answered `authenticated` to anyone on the LAN since `0.1.0`, so the validity oracle
///    predates this field and is not widened by it. Anyone who guessed a session id already holds
///    the authority this would describe, and learning its name tells them nothing a single
///    request would not.
///
/// A caller with no session reads `null`. An *authenticated* caller reading `null` is holding a
/// session minted before scopes existed — which `require_auth` refuses, so the honest answer to
/// that pair is "re-pair", and it is distinguishable from "this build has no scopes" only by the
/// field being present at all.
pub async fn me(session: Session) -> Json<Value> {
    let authenticated = session
        .get::<bool>(AUTH_KEY)
        .await
        .ok()
        .flatten()
        .unwrap_or(false);
    // What this session is allowed to do, so a client can check it got what it asked for. See
    // the `scope` note in this function's doc comment for why an *honest* client is the whole
    // audience.
    let scope = match session.get::<crate::pairing::Scope>(SCOPE_KEY).await {
        Ok(Some(crate::pairing::Scope::Dashboard)) => json!({ "kind": "dashboard" }),
        Ok(Some(crate::pairing::Scope::Integration { source })) => {
            json!({ "kind": "integration", "source": source })
        }
        // Authenticated but unscoped is a session from before scopes existed, which `require_auth`
        // refuses. Reported as null rather than omitted, so a client can tell "this build has no
        // scopes" (field absent) from "your session predates them" (field present, null) — the
        // second needs re-pairing and the first does not.
        _ => Value::Null,
    };
    Json(json!({
        "authenticated": authenticated,
        "version": crate::VERSION,
        "scope": scope,
    }))
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
/// Record who just signed in, on the session itself.
///
/// Called from both authentication paths, next to the `AUTH_KEY` and `SCOPE_KEY` writes, so a
/// session cannot exist without a description of the device that made it.
///
/// **The user agent is attacker-influenced text**, and `O77` flagged it before this was written:
/// it arrives in a request header, is stored, and is later rendered in the parent's dashboard.
/// Two bounds apply. It is truncated to [`MAX_USER_AGENT`] here, so a caller cannot grow the
/// session file — the same reasoning as every other cap in `config.rs`, and it matters more here
/// because the store is rewritten whole on each save. And it is rendered through Alpine's text
/// binding, never a markup sink, so the string is displayed rather than interpreted.
async fn remember_device(session: &Session, headers: &HeaderMap) -> Result<(), AppError> {
    let user_agent: String = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .chars()
        .take(MAX_USER_AGENT)
        .collect();
    session
        .insert(
            DEVICE_KEY,
            json!({
                "first_seen": OffsetDateTime::now_utc().unix_timestamp(),
                "user_agent": user_agent,
            }),
        )
        .await?;
    Ok(())
}

/// Longest user-agent string kept. Real ones are under 200 characters; this is generous and
/// still bounds what one request can add to a file that is rewritten in full on every save.
const MAX_USER_AGENT: usize = 256;

/// A stable, public name for a session that is **not** the session id.
///
/// The *Signed-in devices* card has to give the parent something to click "revoke" on, and the
/// obvious handle — the session id — is a live credential: putting it in an API response and
/// then into the DOM would hand a working cookie to anything that can read the page, in the one
/// feature whose entire purpose is containing a leaked cookie.
///
/// So this is a salted SHA-256 of the id, which is what OWASP's session guidance recommends for
/// exactly this ("use a salted-hash of the session ID instead of the session ID itself in order
/// to allow for session-specific log correlation"). Twelve hex characters is 48 bits — far more
/// than enough to tell apart the handful of devices one household signs in, and useless as a
/// credential.
///
/// **The salt is per-process and random**, so handles change when the service restarts. That is
/// harmless — a handle is only ever used between one render of the card and the click that
/// follows it — and it means a handle observed once cannot be replayed against a later run.
fn session_handle(id: &Id) -> String {
    static SALT: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    let salt = SALT.get_or_init(|| {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).expect("the OS random source must work");
        bytes
    });
    let mut hasher = Sha256::new();
    hasher.update(salt);
    hasher.update(id.to_string().as_bytes());
    hasher
        .finalize()
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What the *Signed-in devices* card shows for one session.
///
/// `handle` is [`session_handle`], never the id. `current` lets the card warn a parent that they
/// are about to sign themselves out, which is the one revocation they will regret.
pub fn describe_session(record: &Record, current: Option<&Id>) -> Value {
    let device = record.data.get(DEVICE_KEY);
    let first_seen = device
        .and_then(|d| d.get("first_seen"))
        .and_then(Value::as_i64);
    // Truncated on the way in (see `remember_device`), so this is a bounded string by
    // construction. It is still attacker-influenced text on its way to the parent's dashboard,
    // and the card renders it through Alpine's text binding rather than any HTML sink.
    let user_agent = device
        .and_then(|d| d.get("user_agent"))
        .and_then(Value::as_str);
    // Last activity is *derived* rather than stored: `expiry_date` is recomputed on every save,
    // so it is last-save plus the inactivity window. Coarse — `SLIDING_REFRESH_SECS` means saves
    // are days apart — and deliberately not tightened, because making it precise would turn a
    // read-only dashboard visit into a disk write per session per request, which is the property
    // this store's module doc exists to protect. "Active this week" is what the card can honestly
    // say, and it is enough to tell a live phone from one retired in August.
    let last_seen = record.expiry_date.unix_timestamp() - SESSION_IDLE_DAYS * 86_400;
    json!({
        "handle": session_handle(&record.id),
        "current": current == Some(&record.id),
        "scope": record.data.get(SCOPE_KEY).cloned().unwrap_or(Value::Null),
        "first_seen": first_seen,
        "last_seen": last_seen,
        "expires": record.expiry_date.unix_timestamp(),
        "user_agent": user_agent,
    })
}

/// Find the session whose [`session_handle`] is `handle`.
///
/// Linear over a household's sessions, which is a handful. Compared on the *derived* handle so
/// the caller never sees an id, and so a caller guessing handles learns nothing: a miss and a
/// hit are the same lookup.
pub fn session_by_handle(
    store: &crate::sessionstore::FileSessionStore,
    handle: &str,
) -> Option<Id> {
    store
        .snapshot()
        .into_iter()
        .find(|r| session_handle(&r.id) == handle)
        .map(|r| r.id)
}

/// May a scoped integration reach this request?
///
/// **The allowlist is three routes and the third is the one that gets forgotten.** An integration
/// pushes to `/api/extra-time`, and then *reads the grant back* from `/api/usage/today` — it
/// refuses to tell a parent a number the PC does not show, which is the agreed mitigation for a
/// replayed idempotency key crossing midnight (`O85`). An allowlist written from the obvious
/// sentence — "the phone pushes grants" — contains only the first, silently disables the
/// read-back, and **every test in both repositories still passes**. It was named as a trap by the
/// session that maintains that client before this was written, which is the only reason it is
/// not one here. The third route is `GET /p/{token}` itself, which never reaches this function
/// because pairing is unauthenticated by design.
///
/// `/api/usage/today` stays open to `Scope::Dashboard` for everyone else: it is also the Android
/// client's, and narrowing a shared route for one caller would break a full dashboard to bound an
/// integration.
///
/// Matched on the path as this layer sees it, which is *inside* `.nest("/api", …)`. That is
/// asserted by a test rather than assumed, because the difference between the nested and the
/// original path is exactly one silent prefix.
fn integration_may_reach(method: &axum::http::Method, path: &str) -> bool {
    matches!(
        (method.as_str(), path),
        ("POST", "/extra-time" | "/api/extra-time") | ("GET", "/usage/today" | "/api/usage/today")
    )
}

pub async fn require_auth(
    session: Session,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let authenticated = session.get::<bool>(AUTH_KEY).await?.unwrap_or(false);
    if !authenticated {
        return Err(AppError::Unauthorized);
    }

    // Fail closed on a session with no scope: it predates `SCOPE_KEY`, so what it may do was
    // never recorded and cannot now be inferred. See that constant for why this is the right
    // direction rather than the convenient one.
    let Some(scope) = session.get::<crate::pairing::Scope>(SCOPE_KEY).await? else {
        return Err(AppError::Unauthorized);
    };
    if let crate::pairing::Scope::Integration { .. } = &scope
        && !integration_may_reach(request.method(), request.uri().path())
    {
        return Err(AppError::Forbidden(
            "this pairing may push earned time and read today's total, nothing else".into(),
        ));
    }
    // Handed to the handler so a grant is attributed to the credential that made it rather than
    // to a name the request chose. `extra_time` is the one reader.
    let mut request = request;
    request.extensions_mut().insert(scope);

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

    /// The boundary itself, pinned in both directions.
    ///
    /// A parent reported entering ten characters and being told they needed ten. The rule was
    /// never off by one — this proves the exact-minimum case passes — which is what established
    /// that the input was not what it appeared, and why every rejection now reports the count it
    /// actually measured.
    #[test]
    fn exactly_the_minimum_is_accepted_and_one_short_is_not() {
        // Not a run and not a repeat: the blocklist rejects those regardless of length, and an
        // earlier draft of this test used "abcdefgh" and was rejected for exactly that.
        let at_min: String = "kw7pxq2mzv".chars().take(MIN_PASSWORD_LEN).collect();
        assert_eq!(at_min.chars().count(), MIN_PASSWORD_LEN);
        assert_eq!(
            check_password(&at_min),
            Ok(()),
            "exactly the minimum must pass"
        );

        let one_short: String = at_min.chars().take(MIN_PASSWORD_LEN - 1).collect();
        assert_eq!(
            check_password(&one_short),
            Err(PasswordProblem::TooShort {
                got: MIN_PASSWORD_LEN - 1
            }),
        );
    }

    /// The reported count must be characters, not bytes. Eight accented letters are eight
    /// characters to the person typing them and sixteen bytes to `len()`; reporting bytes would
    /// tell a parent their eight-character password is sixteen characters long.
    #[test]
    fn length_is_counted_in_characters_not_bytes() {
        // Distinct accented letters, so this exercises the byte-vs-char count and not the
        // repeated-character rule.
        let pw: String = "éàüñöçèìâô".chars().take(MIN_PASSWORD_LEN).collect();
        assert_eq!(pw.chars().count(), MIN_PASSWORD_LEN);
        assert!(pw.len() > MIN_PASSWORD_LEN, "precondition: multibyte");
        assert_eq!(check_password(&pw), Ok(()));

        let short: String = pw.chars().take(MIN_PASSWORD_LEN - 2).collect();
        assert_eq!(
            check_password(&short),
            Err(PasswordProblem::TooShort {
                got: MIN_PASSWORD_LEN - 2
            }),
            "the count in the message must match what the parent typed",
        );
    }

    /// The message has to carry the measured number, or it cannot resolve the disagreement that
    /// prompted it.
    #[test]
    fn the_too_short_message_states_the_measured_count() {
        let msg = PasswordProblem::TooShort { got: 6 }.message();
        assert!(msg.contains('6'), "must name what it counted; got:\n{msg}");
        assert!(
            msg.contains(&MIN_PASSWORD_LEN.to_string()),
            "must name the minimum; got:\n{msg}"
        );
    }

    /// NIST SP 800-63B Rev 4 prohibits composition rules, so digits-only is allowed — but the
    /// guesses an attacker actually starts with are not.
    #[test]
    fn guessable_patterns_are_rejected_but_composition_is_not_required() {
        for bad in [
            "12345678",
            "87654321",
            "abcdefgh",
            "aaaaaaaa",
            "abcabcabc",
            "12121212",
            "password",
            "PASSWORD",
            "password123",
            "qwertyuiop",
            "nestwatch",
        ] {
            assert!(
                matches!(check_password(bad), Err(PasswordProblem::Guessable { .. })),
                "{bad:?} should be rejected as guessable"
            );
        }

        // All lowercase, no digits, no symbols — allowed, because requiring otherwise is exactly
        // what Rev 4 prohibits.
        assert_eq!(check_password("kettleharbour"), Ok(()));
        // All digits, but not a pattern: 10^8 behind Argon2id and per-IP throttling is not the
        // weak link, so this is a real password even though it looks like one.
        assert_eq!(check_password("83920147"), Ok(()));
    }

    /// A mismatch message that only says "they do not match" makes the parent guess which entry
    /// was wrong. Different lengths point at a stray keystroke; equal lengths point at a typo.
    #[test]
    fn mismatch_explains_which_way_without_echoing_the_password() {
        let m = describe_mismatch("kettle-harbour", "kettle-harbou");
        assert!(
            m.contains("14") && m.contains("13"),
            "should give both lengths; got: {m}"
        );
        assert!(
            !m.contains("kettle"),
            "must never echo the password itself; got: {m}"
        );

        let same = describe_mismatch("kettle-harbour", "kettle-harbouX");
        assert!(
            same.contains("somewhere in the middle"),
            "equal lengths should point at a character difference; got: {same}"
        );
    }

    /// Invisible whitespace is the classic paste artifact. Warn, never trim: a password silently
    /// rewritten at install is not the one the parent typed.
    #[test]
    fn whitespace_is_flagged_but_never_removed() {
        assert!(password_caution(" kettleharbour").is_some());
        assert!(password_caution("kettleharbour ").is_some());
        assert!(password_caution("kettleharbour").is_none());
        // Still a valid password — the caution is advice, not a rejection.
        assert_eq!(check_password(" kettleharbour"), Ok(()));
    }

    #[test]
    fn hash_then_verify_round_trips() {
        let hash = hash_password("s3cret-pw").unwrap();
        assert!(verify_password("s3cret-pw", &hash));
        assert!(!verify_password("wrong", &hash));
    }

    /// A password hashed by the **previous** Argon2 release still opens the dashboard.
    ///
    /// This is the one test in the crate whose absence would have been catastrophic rather than
    /// merely bad. `password_hash` is written once, at `install`, and never rewritten — there is
    /// no rehash-on-login (see [`hash_password`]) and, more to the point, **no password reset that
    /// does not require elevated physical access to the child's PC**. A verifier that quietly
    /// stopped accepting existing hashes would lock every parent out of their own install, and the
    /// round-trip test above cannot see it: that one hashes and verifies with the same build, so it
    /// passes just as happily if both halves changed together.
    ///
    /// The literal below is therefore not a fixture to regenerate. It was produced by
    /// `argon2 0.5.3` — the version shipped through v0.5.1 — and must keep verifying under every
    /// version after it. If a future bump breaks this, the bump is wrong, not the constant.
    ///
    /// The parameters embedded in it are also the assertion that OWASP's floor did not move
    /// underneath us: `m=19456,t=2,p=1` is what [`hash_password`]'s docs claim, read out of the
    /// string a real install would hold.
    #[test]
    fn a_password_hashed_by_the_previous_argon2_release_still_verifies() {
        const FROM_0_5_3: &str = "$argon2id$v=19$m=19456,t=2,p=1$\
             7Itvk+mWceKLAFADi8WW3Q$cNMAgu1uAtY8lx2dXmbutZxoqWyhiIuNbn06lvqUdZI";

        assert!(
            verify_password("correct horse battery staple", FROM_0_5_3),
            "a hash written by argon2 0.5.3 no longer verifies — every existing install is \
             locked out, and the only way back in is an elevated console on the managed PC"
        );
        assert!(
            !verify_password("wrong", FROM_0_5_3),
            "the legacy hash accepted a password it should have refused"
        );

        // The current build must still *write* what it claims to write, or the constant above
        // stops describing new installs and this test slowly becomes archaeology.
        let fresh = hash_password("correct horse battery staple").unwrap();
        assert!(
            fresh.contains("$argon2id$v=19$m=19456,t=2,p=1$"),
            "fresh hashes no longer carry OWASP's minimum parameters: {fresh}"
        );
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
