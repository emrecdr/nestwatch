//! Provider probes: run a program as the child on a timer, and judge what it prints.
//!
//! The half of a gate the phone cannot do. Voortgang signs in to StudyGo and can forward the
//! session, but a phone in a pocket cannot ask every fifteen minutes what the child has done;
//! the PC can. `config::Probe` names a program in the program directory, and once a minute
//! [`run_scheduler`] asks which probes are due. For each one it launches the program **as the
//! child** through `SystemControl::run_probe`, with the provider's deposited secret on stdin,
//! reads back `{"questions": N, "minutes": M}`, and hands the two numbers to `Config::earn` — the
//! same judge a push from the phone gets. Nothing here decides a reward, and nothing here believes
//! the probe about anything but those two numbers.
//!
//! **What it deliberately does not do.** It does not run while nobody is signed in: there is no
//! session to launch into, and no practice can be underway at this machine. **An unreadable session
//! state is also "do not run", which is the opposite of what the enforcers do with the same
//! answer** — `rules` treats an `Err` from `session_state` as `Active` so a failure can never hand
//! out unlimited time, and this treats it as absent so a failure can never spend a request on a
//! third party's API on the strength of something it could not read. Both fail toward the outcome
//! that costs least, and for these two loops that is opposite directions. It does not run a
//! source already paid in full for the day: that is a request to a third party which can change
//! nothing, and the provider's own risk register names request volume as its exposure. And it
//! does not keep a probe's failure to itself — every run leaves a [`ProbeStatus`] the dashboard
//! reads, because the designed failure mode of this whole feature is *the base budget*, and a
//! parent has to be able to see that the link is down rather than infer it from a quiet child.
//!
//! **What it costs a household that never opted in.** The scheduler wakes once a minute, reads
//! the config, finds no provider naming a probe, and sleeps. No controller call, no disk, no
//! process. That is the whole of it, and `tests/probe_runner.rs` pins the *nothing configured*
//! case as a section rather than leaving it to inspection.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, FixedOffset, NaiveDate};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::{Earn, Probe};
use crate::state::AppState;

/// Most bytes a deposited secret may hold.
///
/// A StudyGo session token is a JWT of a kilobyte or two; eight is room for a longer one without
/// letting the route that deposits it write a file of any size into the data dir.
pub const MAX_SECRET_BYTES: usize = 8 * 1024;

/// How often the scheduler looks for due probes. A parent's interval is in minutes, so a minute
/// is the finest it needs to be — and the check itself is a read of the config.
const SCHEDULER_TICK: std::time::Duration = std::time::Duration::from_secs(60);

/// What a probe reported: the two numbers the registry judges, and the only two it reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Progress {
    pub questions: u32,
    pub minutes: u32,
}

/// How one run came out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// Minutes added to today's budget by `Config::earn`.
    Granted(u32),
    /// The registry's ordinary refusal — the same three reasons a push can get.
    Refused(&'static str),
    /// The probe could not be run, did not answer, or answered something that is not an answer.
    Failed(String),
}

/// The last run of one provider's probe, kept in memory for the dashboard.
///
/// In memory only, deliberately: a restart forgets it, and the first tick after a restart runs
/// every due probe again, which is the right thing for a service that may have been down for
/// hours. The persisted facts — what was earned — live in the config, where `Config::earn` put
/// them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeStatus {
    pub at: DateTime<FixedOffset>,
    pub reported: Option<Progress>,
    pub outcome: ProbeOutcome,
}

impl ProbeStatus {
    /// The shape `GET /api/providers` carries under `probe_status`, one field per outcome, so a
    /// row can say *granted 16*, *refused: daily_cap_reached* or *error: …* without parsing a
    /// sentence.
    pub fn to_json(&self) -> Value {
        let mut out = serde_json::Map::new();
        out.insert("at".into(), json!(self.at.to_rfc3339()));
        if let Some(progress) = self.reported {
            out.insert("questions".into(), json!(progress.questions));
            out.insert("minutes".into(), json!(progress.minutes));
        }
        match &self.outcome {
            ProbeOutcome::Granted(minutes) => out.insert("granted".into(), json!(minutes)),
            ProbeOutcome::Refused(reason) => out.insert("refused".into(), json!(reason)),
            ProbeOutcome::Failed(error) => out.insert("error".into(), json!(error)),
        };
        Value::Object(out)
    }
}

/// Per-provider [`ProbeStatus`], keyed by provider name. Held in `AppState`.
pub type StatusMap = Arc<Mutex<BTreeMap<String, ProbeStatus>>>;

/// Where probes live: the one directory the child can execute from and cannot write to.
///
/// On Windows that is the program directory `install::harden_program_dir` locks — SYSTEM and
/// Administrators full, Users read-and-execute — which is exactly the property a probe needs. On
/// a dev machine it is `probes/` inside the data dir, which is the developer's own.
pub fn probe_dir() -> PathBuf {
    #[cfg(windows)]
    {
        crate::install::install_dir()
    }
    #[cfg(not(windows))]
    {
        crate::config::data_paths().dir.join("probes")
    }
}

fn secret_path(data_dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    // The name is a path segment. Every caller has validated it as a provider name already;
    // refusing here as well is what makes this function safe to call from anywhere.
    if !crate::api::valid_provider_name(name) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{name:?} is not a provider name"),
        ));
    }
    Ok(data_dir.join("secrets").join(name))
}

/// Keep `secret` for the provider `name`, replacing any earlier one.
///
/// A file of its own under the data dir rather than a field in `config.json`, because the config
/// leaves the machine: `GET /api/policy` and `GET /api/export` both carry it, and a session token
/// for the child's account must ride in neither. On Windows the data dir's ACL is what makes the
/// file private (SYSTEM and Administrators only); on the platforms where a mode means something
/// the directory and the file are the owner's alone as well.
pub fn store_secret(data_dir: &Path, name: &str, secret: &[u8]) -> std::io::Result<()> {
    if secret.len() > MAX_SECRET_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("secret is larger than {MAX_SECRET_BYTES} bytes"),
        ));
    }
    let path = secret_path(data_dir, name)?;
    let dir = path.parent().expect("a secret path has a directory");
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    crate::config::write_atomic(&path, secret)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// The secret deposited for `name`, or `None` when there is none.
pub fn read_secret(data_dir: &Path, name: &str) -> std::io::Result<Option<Vec<u8>>> {
    match std::fs::read(secret_path(data_dir, name)?) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// When the secret for `name` was deposited, or `None` when there is none.
///
/// The file's own modification time: a deposit replaces the file, so this is the age of the
/// session the probe is using — what a parent needs to know when it stops working roughly every
/// ten days.
pub fn secret_deposited_at(
    data_dir: &Path,
    name: &str,
) -> std::io::Result<Option<std::time::SystemTime>> {
    match std::fs::metadata(secret_path(data_dir, name)?) {
        Ok(meta) => meta.modified().map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Forget the secret for `name`. `Ok(false)` when there was none — uninstalling a provider that
/// never deposited one is not an error.
pub fn delete_secret(data_dir: &Path, name: &str) -> std::io::Result<bool> {
    match std::fs::remove_file(secret_path(data_dir, name)?) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Read a probe's answer: one JSON object with `questions` and `minutes`, both non-negative
/// integers. Unknown fields are ignored — a newer probe may say more — and anything else is
/// refused rather than coerced, because the writer runs as the child.
pub fn parse_output(bytes: &[u8]) -> Result<Progress, String> {
    serde_json::from_slice::<Progress>(bytes)
        .map_err(|e| format!("probe output is not {{\"questions\", \"minutes\"}}: {e}"))
}

/// Whether a probe last run at `last` is due at `now`, given its interval.
///
/// A `last` in the future — the clock went backwards — reads as due rather than as "not until
/// it catches up": the cost of being wrong that way is one request, and the cost the other way
/// is a child who cannot earn for as long as the clock was set forward by.
pub fn due(
    now: DateTime<FixedOffset>,
    last: Option<DateTime<FixedOffset>>,
    every_mins: u32,
) -> bool {
    match last {
        None => true,
        Some(last) => {
            let since = now - last;
            since < chrono::Duration::zero()
                || since >= chrono::Duration::minutes(i64::from(every_mins))
        }
    }
}

/// One tick of the scheduler: run every probe that is due at `now`, and record what happened.
///
/// Separated from the timer so `tests/probe_runner.rs` can drive a whole day through it by hand,
/// the way the enforcer tests drive theirs. `now` is the trusted clock's reading in production
/// and whatever the test says it is; `today` is derived from it so the two cannot disagree.
pub async fn run_once(state: &AppState, now: DateTime<FixedOffset>) {
    let today = now.date_naive();
    // Decided from a snapshot of the config and the status map, both released before anything
    // blocks. A provider is due when it names a probe, is switched on, could still earn today,
    // and its interval has passed.
    let due_now: Vec<(String, Probe)> = {
        let cfg = crate::state::recover_read(&state.config);
        let status = crate::api::recover_lock(&state.probe_status);
        cfg.providers
            .iter()
            .filter_map(|(name, provider)| {
                let probe = provider.probe.as_ref()?;
                if !provider.enabled || provider.exhausted_for(today, cfg.earned.get(name)) {
                    return None;
                }
                let last = status.get(name).map(|s| s.at);
                due(now, last, probe.every_mins).then(|| (name.clone(), probe.clone()))
            })
            .collect()
    };
    if due_now.is_empty() {
        return;
    }
    // Only while the child is signed in and at the machine. Nothing is marked when he is not, so
    // the first minute after he signs in runs whatever was waiting.
    let control = state.control.clone();
    match tokio::task::spawn_blocking(move || control.session_state()).await {
        Ok(Ok(crate::control::SessionState::Active)) => {}
        Ok(Ok(_)) => return,
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "probe: session state unknown, not running");
            return;
        }
        Err(e) => {
            tracing::error!(error = %e, "probe: session query panicked");
            return;
        }
    }
    for (name, probe) in due_now {
        let (reported, outcome) = run_one(state, &name, &probe, today).await;
        match &outcome {
            ProbeOutcome::Granted(minutes) => {
                tracing::info!(provider = %name, minutes, "probe grant")
            }
            ProbeOutcome::Refused(reason) => {
                tracing::debug!(provider = %name, reason, "probe refused")
            }
            ProbeOutcome::Failed(error) => {
                tracing::warn!(provider = %name, error = %error, "probe failed")
            }
        }
        crate::api::recover_lock(&state.probe_status).insert(
            name,
            ProbeStatus {
                at: now,
                reported,
                outcome,
            },
        );
    }
}

/// Run one provider's probe and judge its answer.
async fn run_one(
    state: &AppState,
    name: &str,
    probe: &Probe,
    today: NaiveDate,
) -> (Option<Progress>, ProbeOutcome) {
    let data_dir = crate::config::data_paths().dir;
    let exe = probe_dir().join(&probe.exe);
    let control = state.control.clone();
    let source = name.to_string();
    // The secret is read and handed over on the blocking pool, and never held here: it exists
    // in this process for exactly as long as it takes to write it into the probe's stdin.
    let output = tokio::task::spawn_blocking(move || {
        let secret = read_secret(&data_dir, &source)
            .map_err(|e| format!("reading the deposited secret: {e}"))?
            .unwrap_or_default();
        control.run_probe(&exe, secret).map_err(|e| e.to_string())
    })
    .await;
    let output = match output {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) => return (None, ProbeOutcome::Failed(error)),
        Err(e) => {
            return (
                None,
                ProbeOutcome::Failed(format!("probe task panicked: {e}")),
            );
        }
    };
    let progress = match parse_output(&output) {
        Ok(progress) => progress,
        Err(error) => return (None, ProbeOutcome::Failed(error)),
    };
    // Judged inside the config critical section, exactly as a push is: the provider it is read
    // from is the one the latch is written against, and a push arriving in the same instant
    // serializes behind this on `config_save_lock`.
    let mut verdict = None;
    let source = name.to_string();
    let reported = Some((progress.questions, progress.minutes));
    let persisted = crate::api::try_update_config(state, |c| {
        verdict = Some(
            c.earn(&source, today, reported)
                .map_err(crate::error::AppError::BadRequest)?,
        );
        Ok(())
    })
    .await;
    let outcome = match (persisted, verdict) {
        (Ok(()), Some(Earn::Granted(minutes))) => {
            // The same two records a pushed grant leaves, plus the one fact that distinguishes
            // them — absent from a push's line, so nothing that reads those changes shape.
            let line = json!({ "minutes": minutes, "source": name, "probe": true });
            state.audit.record("extra_time_granted", line.clone());
            state.usage.record("extra_time_granted", line);
            crate::api::notify(state, "usage");
            ProbeOutcome::Granted(minutes)
        }
        (Ok(()), Some(Earn::Refused(reason))) => ProbeOutcome::Refused(reason),
        // The provider was switched off or removed between the snapshot and the judgement, or
        // the config could not be saved. Either way nothing was granted.
        (Err(e), _) => ProbeOutcome::Failed(e.to_string()),
        (Ok(()), None) => ProbeOutcome::Failed("the registry gave no verdict".into()),
    };
    (Some(progress), outcome)
}

/// Run [`run_once`] once a minute for the life of the service.
///
/// A plain interval, not a `heartbeat` enforcer: this loop enforces nothing, and its silent
/// death is *the base budget* — the outcome the design chose for every failure. What a parent
/// needs to see is not "the scheduler is alive" but "this provider's last run was at …", which
/// is what [`ProbeStatus::at`] on the dashboard says.
pub async fn run_scheduler(state: AppState) {
    let mut ticker = tokio::time::interval(SCHEDULER_TICK);
    // See the note in `rules`: without this a resume from sleep replays every missed tick.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        run_once(&state, crate::clock::now()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::ScratchDir;

    #[test]
    fn a_secret_round_trips_through_its_own_file() {
        let dir = ScratchDir::new("probe-secret");
        assert_eq!(read_secret(dir.path(), "studygo").unwrap(), None);
        // Asserted beside its sibling, because *absent* is the common case — every provider
        // without a probe — and the one caller of this reads it through `.ok().flatten()`, which
        // cannot tell an error from an absence. So nothing but this line would notice if a missing
        // deposit started reporting a fault. Mutation testing found exactly that.
        assert_eq!(secret_deposited_at(dir.path(), "studygo").unwrap(), None);
        store_secret(dir.path(), "studygo", b"tok").unwrap();
        assert!(
            secret_deposited_at(dir.path(), "studygo")
                .unwrap()
                .is_some(),
            "a deposited secret has an age"
        );
        assert_eq!(
            read_secret(dir.path(), "studygo").unwrap(),
            Some(b"tok".to_vec())
        );
        assert!(dir.path().join("secrets").join("studygo").is_file());
        store_secret(dir.path(), "studygo", b"tok2").unwrap();
        assert_eq!(
            read_secret(dir.path(), "studygo").unwrap(),
            Some(b"tok2".to_vec()),
            "a later deposit replaces the earlier one"
        );
        assert!(delete_secret(dir.path(), "studygo").unwrap());
        assert!(
            !delete_secret(dir.path(), "studygo").unwrap(),
            "deleting twice is not an error"
        );
        assert_eq!(read_secret(dir.path(), "studygo").unwrap(), None);
        assert_eq!(secret_deposited_at(dir.path(), "studygo").unwrap(), None);
    }

    /// The provider name is a path segment, so it is validated here as well as at the door.
    ///
    /// Every caller has already checked it — the route validates before touching this — which is
    /// exactly why this needs its own test: a second guard whose first line of defence never fails
    /// is a guard nothing exercises, and it is the one that makes these three functions safe to
    /// call from anywhere. A name that could climb out of `secrets/` would put an arbitrary file
    /// under the data directory, or read one.
    #[test]
    fn a_name_that_could_leave_the_directory_is_refused_by_every_entry_point() {
        let dir = ScratchDir::new("probe-secret-escape");
        for bad in [
            "..", "../evil", "..\\evil", "a/b", "a\\b", "", "parent", "Studygo", "a b",
        ] {
            assert!(
                store_secret(dir.path(), bad, b"tok").is_err(),
                "store_secret must refuse {bad:?}"
            );
            assert!(
                read_secret(dir.path(), bad).is_err(),
                "read_secret must refuse {bad:?}"
            );
            assert!(
                secret_deposited_at(dir.path(), bad).is_err(),
                "secret_deposited_at must refuse {bad:?}"
            );
            assert!(
                delete_secret(dir.path(), bad).is_err(),
                "delete_secret must refuse {bad:?}"
            );
        }
        // Nothing was created anywhere while all of that was being refused.
        assert!(!dir.path().join("secrets").exists());
        assert!(!dir.path().join("evil").exists());
    }

    /// The bound has to fit what it exists to hold.
    ///
    /// `MAX_SECRET_BYTES` is written as an expression, and an arithmetic slip in it is invisible to
    /// every test that spells the limit symbolically — which is all of them, correctly. What is
    /// *not* arbitrary is the size of the thing being stored: a StudyGo session is a JWT of a
    /// kilobyte or two, so a limit that cannot hold two kilobytes has broken the feature while
    /// every bound test still passes. Mutation testing found this by turning the `*` into a `+`.
    #[test]
    fn the_bound_has_room_for_a_real_session_token() {
        let dir = ScratchDir::new("probe-secret-real");
        let token = vec![b'j'; 2 * 1024];
        assert!(
            store_secret(dir.path(), "studygo", &token).is_ok(),
            "a 2 KiB session token must fit in MAX_SECRET_BYTES ({MAX_SECRET_BYTES})"
        );
        assert_eq!(read_secret(dir.path(), "studygo").unwrap(), Some(token));
    }

    /// Where a probe is resolved is a security property, not a convenience.
    ///
    /// The whole argument for accepting a bare file name — see [`Probe`] — is that it is joined to
    /// a directory the child cannot write to. An empty or relative directory would resolve the
    /// probe against the service's working directory instead, which is not that directory and is
    /// not locked by anything. Mutation testing replaced this function's body with
    /// `Default::default()` and nothing failed: the one test that used it compared
    /// `probe_dir().join(..)` against `probe_dir().join(..)`, which agrees with itself however
    /// wrong it is.
    #[test]
    fn a_probe_is_resolved_inside_an_absolute_directory() {
        let dir = probe_dir();
        assert!(
            dir.is_absolute(),
            "a probe must resolve against an absolute path, not the service's working directory: \
             {dir:?}"
        );
        #[cfg(windows)]
        assert!(dir.ends_with("HostHealth"), "{dir:?}");
        #[cfg(not(windows))]
        assert!(dir.ends_with("probes"), "{dir:?}");
    }

    /// A secret that cannot be reached is not a secret that is absent.
    ///
    /// All three of these match `NotFound` specifically, and mutation testing showed nothing
    /// noticed when that guard was widened to every error. Each has a different cost. For
    /// [`read_secret`] the probe would run with an empty credential instead of the run being
    /// recorded as failed — the silent-failure shape the status line exists to prevent. For
    /// [`secret_deposited_at`] the card would say no session was ever deposited. For
    /// [`delete_secret`] it is worse than cosmetic: `api::delete_provider` audits what this
    /// returns, so a credential that survived an uninstall would be recorded as one that was
    /// never there — and destroying the credential is what uninstalling promises.
    ///
    /// Unix only, because the fault is induced by putting a *file* where the `secrets` directory
    /// belongs: that is `ENOTDIR` here, while Windows reports the same shape as a missing path and
    /// would make this assert the opposite of what it means.
    #[cfg(unix)]
    #[test]
    fn a_secret_that_cannot_be_reached_is_an_error_not_an_absence() {
        let dir = ScratchDir::new("probe-secret-unreadable");
        std::fs::write(dir.path().join("secrets"), b"not a directory").unwrap();
        assert!(
            read_secret(dir.path(), "studygo").is_err(),
            "an unreadable deposit must not read as 'no secret deposited'"
        );
        assert!(
            secret_deposited_at(dir.path(), "studygo").is_err(),
            "nor must the age of one"
        );
        assert!(
            delete_secret(dir.path(), "studygo").is_err(),
            "nor must a deletion that could not happen report that there was nothing to delete"
        );
    }

    #[test]
    fn a_secret_past_the_bound_is_refused_and_nothing_is_written() {
        let dir = ScratchDir::new("probe-secret-big");
        assert!(store_secret(dir.path(), "studygo", &vec![b'x'; MAX_SECRET_BYTES + 1]).is_err());
        assert_eq!(read_secret(dir.path(), "studygo").unwrap(), None);
        assert!(store_secret(dir.path(), "studygo", &vec![b'x'; MAX_SECRET_BYTES]).is_ok());
    }

    /// On the platforms where a mode means something, the file is the owner's alone. On Windows
    /// the data directory's ACL is what does this job, for every file in it.
    #[cfg(unix)]
    #[test]
    fn a_secret_file_is_private_to_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = ScratchDir::new("probe-secret-mode");
        store_secret(dir.path(), "studygo", b"tok").unwrap();
        let mode = std::fs::metadata(dir.path().join("secrets/studygo"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    /// Two integers in an object. Extra fields are a newer probe; anything else is not an answer.
    #[test]
    fn the_answer_is_two_integers_and_nothing_else_is_believed() {
        assert_eq!(
            parse_output(br#"{"questions":12,"minutes":24}"#).unwrap(),
            Progress {
                questions: 12,
                minutes: 24
            }
        );
        assert_eq!(
            parse_output(b" {\"minutes\":0,\"questions\":0,\"extra\":true}\n").unwrap(),
            Progress {
                questions: 0,
                minutes: 0
            },
            "unknown fields are a newer probe, not a lie"
        );
        for bad in [
            &b""[..],
            b"null",
            b"[]",
            b"12",
            br#"{"questions":12}"#,
            br#"{"questions":-1,"minutes":2}"#,
            br#"{"questions":1.5,"minutes":2}"#,
            br#"{"questions":"12","minutes":24}"#,
            br#"{"questions":12,"minutes":24} trailing"#,
            b"not json",
        ] {
            assert!(
                parse_output(bad).is_err(),
                "{:?} must be refused",
                String::from_utf8_lossy(bad)
            );
        }
    }

    #[test]
    fn a_probe_is_due_when_never_run_or_when_its_interval_has_passed() {
        let t0 = chrono::DateTime::parse_from_rfc3339("2026-09-08T16:00:00+02:00").unwrap();
        assert!(due(t0, None, 15));
        // No time at all has passed, which is the boundary between "the clock went backwards" and
        // "it has not been long enough". Not due: a scheduler that fired twice on one instant must
        // not spend two requests on it. The mutation run that would have judged this timed out
        // under load rather than answering, so it is pinned here by hand.
        assert!(!due(t0, Some(t0), 15));
        assert!(!due(t0 + chrono::Duration::minutes(14), Some(t0), 15));
        assert!(due(t0 + chrono::Duration::minutes(15), Some(t0), 15));
        assert!(
            due(t0 - chrono::Duration::minutes(1), Some(t0), 15),
            "a clock that went backwards reads as due, never as not-until-it-catches-up"
        );
    }

    /// What the dashboard reads. One field per outcome, so a row can say *granted 16*,
    /// *refused: daily_cap_reached* or *error: …* without parsing a sentence.
    #[test]
    fn a_status_says_what_happened_in_one_field_each() {
        let at = chrono::DateTime::parse_from_rfc3339("2026-09-08T16:00:00+02:00").unwrap();
        let reported = Some(Progress {
            questions: 12,
            minutes: 24,
        });
        let granted = ProbeStatus {
            at,
            reported,
            outcome: ProbeOutcome::Granted(16),
        };
        assert_eq!(
            granted.to_json(),
            serde_json::json!({
                "at": "2026-09-08T16:00:00+02:00",
                "questions": 12,
                "minutes": 24,
                "granted": 16
            })
        );
        let refused = ProbeStatus {
            at,
            reported,
            outcome: ProbeOutcome::Refused("daily_cap_reached"),
        };
        assert_eq!(refused.to_json()["refused"], "daily_cap_reached");
        assert!(refused.to_json().get("granted").is_none());
        let failed = ProbeStatus {
            at,
            reported: None,
            outcome: ProbeOutcome::Failed("probe exited with code 3".into()),
        };
        assert_eq!(failed.to_json()["error"], "probe exited with code 3");
        assert!(failed.to_json().get("questions").is_none());
    }
}
