//! Persisted configuration and the on-disk locations the app uses.
//!
//! Config is a tiny JSON file holding the listen port and the Argon2 password *hash*
//! (never the plaintext). It lives alongside the TLS cert/key in a per-user data dir:
//! `%PROGRAMDATA%\HostHealth` on Windows (bland, low-profile), `~/.config/nestwatch` on dev.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
// `Datelike` is for `at.weekday()` in `scheduled_routine_at`; `Timelike` is not needed because
// `at.time()` comes from `DateTime` itself.
use chrono::{DateTime, Datelike, FixedOffset, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::curfew::Curfew;

pub const DEFAULT_PORT: u16 = 8443;

/// The current local calendar day — the single key the grant writer (approve handler) and
/// reader (rules enforcer) both use.
///
/// Delegates to [`crate::clock`], which resists the timezone being changed underneath us: a
/// standard Windows user can change the time zone with no UAC prompt, and this date is what
/// decides when the day's budget resets.
pub fn today() -> NaiveDate {
    crate::clock::today()
}

/// Extra screen-time minutes granted for a single day (via an approved time request). The
/// "only counts today" rule lives here, in one place, so the approve handler (writer) and the
/// rules enforcer (reader) can't drift.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DailyGrant {
    /// The local day the grant applies to (`None` = nothing granted yet).
    #[serde(default)]
    pub date: Option<NaiveDate>,
    /// Minutes granted for `date`.
    #[serde(default)]
    pub minutes: u32,
}

impl DailyGrant {
    /// Minutes granted for `today` — `0` unless the stored grant is for today.
    pub fn for_day(&self, today: NaiveDate) -> u32 {
        if self.date == Some(today) {
            self.minutes
        } else {
            0
        }
    }

    /// Add `minutes` to today's grant, resetting first if the stored grant is for another day.
    pub fn add(&mut self, today: NaiveDate, minutes: u32) {
        if self.date != Some(today) {
            self.date = Some(today);
            self.minutes = 0;
        }
        // Saturating for the same reason as the budget math: release builds wrap silently, and a
        // wrapped grant would subtract time instead of adding it.
        self.minutes = self.minutes.saturating_add(minutes);
    }
}

/// Largest number of saved routines we keep (bounds the config).
pub const MAX_ROUTINES: usize = 20;
/// Largest number of scheduled windows one routine may carry.
///
/// Exists for the same reason [`MAX_ROUTINES`] does — to bound the config file — and the two
/// multiply, so this is the factor that decides the ceiling. Eight covers a different window on
/// every weekday with a spare, which is more shape than any real "homework hour" needs.
pub const MAX_SCHEDULE_WINDOWS: usize = 8;
/// Longest routine name we accept.
pub const MAX_ROUTINE_NAME: usize = 40;

/// A saved, named preset of usage [`Rules`](crate::rules::Rules) — e.g. "Homework", "Weekend" —
/// that the parent can apply to the live rules with one click.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Routine {
    pub name: String,
    pub rules: crate::rules::Rules,
    /// When this routine applies **by itself**, as `[start, end)` windows with a day selector —
    /// the same [`Window`](crate::curfew::Window) the curfew uses, evaluated by the same
    /// predicate.
    ///
    /// Empty is the default and is what every routine saved before this existed loads as, so a
    /// routine with no schedule behaves exactly as routines always have: it does nothing until a
    /// parent presses **Apply**.
    ///
    /// # Why a schedule selects rules instead of writing them
    ///
    /// The obvious implementation is a timer that calls the same code path as the Apply button.
    /// It is wrong here in three ways, and all three are quiet. It would overwrite whatever the
    /// parent had just edited by hand, every thirty seconds, with no way to tell an automatic
    /// write from a deliberate one. It would write `config.json` and an audit line on a timer,
    /// which is the property `screenshot_taken` needed a coalescer to fix. And it would destroy
    /// the base rules, so there would be nothing to go back to when the window closed.
    ///
    /// So nothing is written. [`Config::rules_at`] *chooses* which `Rules` are in force at an
    /// instant, exactly as [`Curfew::is_active_at`](crate::curfew::Curfew::is_active_at) chooses
    /// whether a window is closed, and `rules` on this struct stays the parent's off-schedule
    /// default.
    #[serde(default)]
    pub schedule: Vec<crate::curfew::Window>,
}

/// An installed integration that may push earned bonus time.
///
/// Deliberately tiny: an integration is enable/disable plus the reward its
/// signal earns. It carries no endpoint, no credential, and no code — the
/// gathering happens off this machine (on the parent's phone) and arrives as
/// an authenticated push, which is what keeps the monitored PC from ever
/// dialing out. See `docs/PLUGIN-SYSTEM.md` for why this shape and not a
/// loaded module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provider {
    /// Whether this provider may currently grant. A disabled provider's push
    /// is refused, so turning an integration off is one switch, not a
    /// re-pairing.
    pub enabled: bool,
    /// Minutes one met-threshold push is worth. The parent's policy, applied
    /// on this machine rather than trusted from the client.
    pub minutes: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    pub port: u16,
    /// Argon2 PHC string (`$argon2id$v=19$...`). Verified against on login.
    pub password_hash: String,
    /// Closed time window enforcement. Defaulted so pre-existing configs still load.
    #[serde(default)]
    pub curfew: Curfew,
    /// Screen-time budget, app blocklist, and per-app limits.
    #[serde(default)]
    pub rules: crate::rules::Rules,
    /// Extra minutes granted to *today's* budget (via an approved time request).
    #[serde(default)]
    pub extra: DailyGrant,
    /// The local day each non-`parent` grant source last granted, so an earned
    /// bonus (a phone pushing "practice done") lands **once per source per
    /// day** — judged against *this* machine's trusted clock, never a day the
    /// pushing device computed, for the same reason [`crate::clock`] exists.
    ///
    /// Self-pruning: the grant handler drops entries for other days before
    /// inserting, so the map never outgrows one day's sources. `parent` is
    /// deliberately absent — a human pressing the button twice means it twice.
    #[serde(default)]
    pub earned: std::collections::BTreeMap<String, NaiveDate>,
    /// Installed integrations that may push earned bonus time. A provider is
    /// *data*, not code: a name, an on/off switch, and the reward its signal
    /// is worth — the "declarative plugin" of `docs/PLUGIN-SYSTEM.md`.
    ///
    /// **The reward lives here, on the trusted machine, not in the push.**
    /// A pushing client says only *that* its threshold was met; how many
    /// minutes that earns is the parent's policy, set once per provider and
    /// read here — so a compromised or spoofed client cannot choose its own
    /// reward. A `source` with no enabled provider grants nothing.
    #[serde(default)]
    pub providers: std::collections::BTreeMap<String, Provider>,
    /// Saved rule presets the parent can switch between (Homework / Bedtime / Weekend …).
    #[serde(default)]
    pub routines: Vec<Routine>,
    /// UTC offset in minutes recorded at install — the anchor [`crate::clock`] checks the OS
    /// timezone against. `None` on configs written before this existed, which degrades to plain
    /// local time (the old behavior) rather than guessing an offset for an install that may have
    /// legitimately moved.
    #[serde(default)]
    pub tz_offset_mins: Option<i32>,
    /// The machine's time-zone *identity* at install — the zone it is set to, not the offset that
    /// implies. [`crate::clock`] compares this each tick, and a mismatch is tampering.
    ///
    /// This is the load-bearing half: an offset is ambiguous (Amsterdam in winter and London in
    /// summer are both +60), so an offset check cannot tell a substituted zone from an honest one.
    /// The value is opaque — nothing parses it, everything compares it — and it folds in the
    /// "adjust for DST automatically" flag, which moves the offset without moving the zone name.
    ///
    /// `None` on configs written before this existed, and on non-Windows, which degrades to the
    /// offset tolerance alone (the old behaviour) rather than guessing.
    #[serde(default)]
    pub tz_zone: Option<String>,
    /// Which language the child's own surfaces speak — `/ask` and the desktop countdown warnings.
    /// The dashboard stays English. Defaults to English, so an install that never sets it behaves
    /// exactly as it always did.
    #[serde(default)]
    pub language: Language,
    /// The addresses baked into the current certificate as SANs. Lets `install` tell "the cert
    /// still covers this machine" (reuse it, keeping the fingerprint stable) from "the LAN address
    /// changed" (reissue, because otherwise the browser adds a name-mismatch error on top of the
    /// expected trust warning). Empty on configs written before this existed.
    #[serde(default)]
    pub cert_sans: Vec<String>,
}

/// Which language the **child's** surfaces speak.
///
/// # Why this is a setting and not detected
///
/// This is the first presentation setting in a `Config` that is otherwise entirely enforcement and
/// infrastructure, so it is worth saying why it earns the place rather than being derived.
///
/// The obvious alternative is to read `Accept-Language` for the web page and the Windows UI
/// language for the desktop warnings, which would need no setting at all. It is wrong here for a
/// reason specific to this product: `Accept-Language` is set in the child's own browser. The most
/// important sentence on `/ask` is the one telling the child what is being watched, and letting the
/// person being watched choose the language of their own disclosure notice gets the ownership
/// exactly backwards. The parent configures what the child is told, the same way they configure
/// everything else here.
///
/// Deliberately an enum and not a free string. A locale this build has no strings for would fall
/// back silently to English, which looks identical to the setting not having been saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// The default, and what every install had before this existed.
    #[default]
    En,
    Nl,
}

impl Language {
    /// Every variant, for tests that must cover all of them.
    ///
    /// Exists because the alternative is each test hand-writing `[Language::En, Language::Nl]`,
    /// which keeps passing when a third language is added and quietly stops testing it — the
    /// tautological-fixture trap `tests/spawn_paths.rs` was written to close. Adding a variant
    /// without extending this list fails `all_lists_every_language_variant` below, which counts
    /// the variants in this file's own source rather than trusting the list.
    pub const ALL: [Language; 2] = [Language::En, Language::Nl];

    /// The BCP-47 tag, for `<html lang>` and for the client's string table.
    pub fn tag(self) -> &'static str {
        match self {
            Language::En => "en",
            Language::Nl => "nl",
        }
    }

    /// Parse a tag from the API. `None` for anything this build has no strings for — the caller
    /// rejects it rather than quietly serving English.
    pub fn from_tag(tag: &str) -> Option<Self> {
        match tag {
            "en" => Some(Language::En),
            "nl" => Some(Language::Nl),
            _ => None,
        }
    }
}

/// Resolved on-disk locations, derived from [`data_dir`].
pub struct DataPaths {
    pub dir: PathBuf,
    pub config: PathBuf,
    pub cert: PathBuf,
    pub key: PathBuf,
    /// Pending one-time pairing token (hash only). Written by `install` / `pair`, consumed by
    /// the service — they're separate processes, so this file is the handover.
    pub pairing: PathBuf,
    /// Persisted login sessions, so a service restart doesn't sign the parent out.
    pub sessions: PathBuf,
}

pub fn data_paths() -> DataPaths {
    let dir = data_dir();
    DataPaths {
        config: dir.join("config.json"),
        cert: dir.join("cert.pem"),
        key: dir.join("key.pem"),
        pairing: dir.join("pairing.json"),
        sessions: dir.join("sessions.json"),
        dir,
    }
}

fn data_dir() -> PathBuf {
    // Explicit override, honored ONLY in debug builds (tests/dev). The shipped release
    // service deliberately ignores it, so the location it reads the password hash / TLS key
    // from can't be redirected via an environment variable.
    #[cfg(debug_assertions)]
    if let Some(dir) = std::env::var_os("NESTWATCH_DATA_DIR") {
        return PathBuf::from(dir);
    }
    #[cfg(windows)]
    {
        // Machine-wide (ProgramData), NOT %APPDATA%: `install` runs as the parent/admin
        // while the service runs as SYSTEM, and they must resolve to the same directory.
        // Bland folder name so nothing on the child's disk advertises the tool's purpose.
        std::env::var_os("PROGRAMDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
            .join("HostHealth")
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(".config"))
            .unwrap_or_else(|| PathBuf::from("."))
            .join("nestwatch")
    }
}

/// The part of a [`Config`] that describes a **household** rather than a machine.
///
/// # What it is for
///
/// There was no way to back a setup up or move it. `GET /api/export` carries screen-time history
/// and nothing else, `config.json` lives in an ACL-locked directory a parent reaches only from an
/// elevated console on the child's PC, and `uninstall --purge` deletes it irreversibly. So
/// rebuilding the PC — or setting up a second one — meant re-entering every curfew window, every
/// per-app limit, every group and every routine by hand. Routines make that worse rather than
/// better: they are the most laborious thing in the config and the most worth keeping.
///
/// # What is deliberately NOT in it
///
/// The exclusions are the design, not an oversight. Everything omitted describes *this machine*
/// or *this moment*, and carrying it to another PC would be wrong in a way nobody would notice:
///
/// * `password_hash` — a secret. An exported file is meant to be copied about.
/// * `port` — a property of the install, and `install --port N` is where it is chosen.
/// * `cert_sans` — describes the certificate this machine actually holds.
/// * `tz_offset_mins` / `tz_zone` — **the load-bearing one.** These are the trusted-clock anchor,
///   recorded at install against the machine the child sits at. Importing another machine's anchor
///   would leave the enforcer comparing against a zone this PC is not in, which is exactly the
///   state a child gains two hours of evening from. A restore must never be able to weaken the
///   clock; `POST /api/re-anchor` is the only way to move it, and it reads the machine.
/// * `extra` — today's granted bonus minutes. Restoring a stale grant would hand back time that
///   was already spent, or on another day entirely.
///
/// `Curfew::extra_until` needs the same treatment and cannot be excluded by leaving a field out,
/// because it lives *inside* `Curfew`. [`Config::policy`] clears it on the way out and
/// [`Config::apply_policy`] ignores whatever the file says on the way in. It suppresses bedtime
/// until a given instant, so a hand-edited file carrying one far in the future would switch the
/// curfew off — and it would look like a restore rather than like a bypass.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Policy {
    #[serde(default)]
    pub curfew: Curfew,
    #[serde(default)]
    pub rules: crate::rules::Rules,
    #[serde(default)]
    pub routines: Vec<Routine>,
    #[serde(default)]
    pub language: Language,
}

impl Policy {
    /// Reject anything the live endpoints would reject, before any of it is applied.
    ///
    /// Routes through the **same** `validate` calls `POST /api/curfew`, `POST /api/rules` and
    /// `POST /api/routines` use, rather than restating the rules. A second set of bounds here
    /// would be a second thing to keep in step, and the direction it would drift is the dangerous
    /// one: an import path that accepted a config the live editor refuses is a way to write a
    /// value no form can produce.
    ///
    /// All-or-nothing on purpose. A partial restore leaves a household with some of yesterday's
    /// settings and some of today's, which is a state nobody chose and nothing displays.
    pub fn validate(&self) -> Result<(), String> {
        self.curfew.validate().map_err(|e| format!("curfew: {e}"))?;
        self.rules.validate().map_err(|e| format!("rules: {e}"))?;
        if self.routines.len() > MAX_ROUTINES {
            return Err(format!(
                "too many routines: {} (limit {MAX_ROUTINES})",
                self.routines.len()
            ));
        }
        for r in &self.routines {
            let name = r.name.trim();
            if name.is_empty() || name.chars().count() > MAX_ROUTINE_NAME {
                return Err(format!("invalid routine name: {:?}", r.name));
            }
            r.rules
                .validate()
                .map_err(|e| format!("routine {:?}: {e}", r.name))?;
        }
        Ok(())
    }
}

impl Config {
    /// The rules actually in force at `at` — the base rules, unless a scheduled routine covers
    /// that instant.
    ///
    /// This is the single definition of "which rules apply right now". Every surface that reports
    /// a limit goes through it, because a dashboard showing the base budget while the enforcer
    /// counts down a routine's is the failure this codebase keeps meeting: not a wrong number, but
    /// a true number measuring something other than what the reader assumes.
    ///
    /// # Pause wins, and it wins first
    ///
    /// A paused install returns the base rules — which carry `enabled = false` — before any
    /// schedule is consulted, so a window opening cannot quietly restart enforcement the parent
    /// switched off for the evening. That ordering matches the button's promise ("pause the whole
    /// rules enforcer with one toggle"), matches [`crate::api::apply_routine`], which has always
    /// carried the pause state across an Apply rather than letting the routine set it, and matches
    /// what the parental-control tools families already use do with their own pause controls.
    ///
    /// # First match wins
    ///
    /// Windows may overlap; the earliest routine in `routines` order wins, which is the order the
    /// parent sees and controls in the dashboard. A "last match" or "most specific match" rule
    /// would both need the parent to model something they cannot see on the page.
    ///
    /// # Why the borrow is sound
    ///
    /// The returned rules are used whole, `enabled` included, so a scheduled routine carrying
    /// `enabled = false` would silently stand enforcement down. It cannot: `save_routine`
    /// normalises the flag to `true` on the way in, and a routine's stored `enabled` has never
    /// meant anything anyway — `apply_routine` overwrites it on every Apply. Routines written
    /// before that normalisation existed cannot reach this path either, because they have no
    /// schedule and an empty schedule never matches.
    pub fn rules_at(&self, at: DateTime<FixedOffset>) -> &crate::rules::Rules {
        self.scheduled_routine_at(at)
            .map_or(&self.rules, |r| &r.rules)
    }

    /// The name of the scheduled routine in force at `at`, if one is.
    ///
    /// Split from [`Config::rules_at`] rather than returned alongside it because the enforcer
    /// wants only the rules and would have to ignore half a tuple on every tick. Both delegate to
    /// [`Config::scheduled_routine_at`], so they cannot disagree about which routine is active —
    /// a disagreement that would put a routine's name on the dashboard beside a different
    /// routine's budget.
    ///
    /// Read by `usage_today`, so the card that shows a budget also says what put it there.
    pub fn active_routine_at(&self, at: DateTime<FixedOffset>) -> Option<&str> {
        self.scheduled_routine_at(at).map(|r| r.name.as_str())
    }

    /// The one place a schedule is evaluated. See [`Config::rules_at`] for the two rules it
    /// encodes — pause first, then first match wins.
    fn scheduled_routine_at(&self, at: DateTime<FixedOffset>) -> Option<&Routine> {
        if !self.rules.enabled {
            return None;
        }
        self.routines.iter().find(|r| {
            // An empty schedule is "manual only" and must never match — `any_window_active` would
            // already answer `false` for an empty slice, but saying so here is what makes the
            // legacy-routine argument in `rules_at` true by construction rather than by a
            // property of another function.
            !r.schedule.is_empty()
                && crate::curfew::any_window_active(&r.schedule, at.time(), at.weekday())
        })
    }

    /// This install's household settings, ready to hand to a parent as a file.
    pub fn policy(&self) -> Policy {
        let mut curfew = self.curfew.clone();
        // Tonight's extension is not a setting. See [`Policy`].
        curfew.extra_until = None;
        Policy {
            curfew,
            rules: self.rules.clone(),
            routines: self.routines.clone(),
            language: self.language,
        }
    }

    /// Overwrite the household settings from `policy`, preserving everything machine-local.
    ///
    /// Two fields are taken from the **live** config rather than from the document, and both are
    /// the same kind of thing — state a person set a moment ago that a restore has no business
    /// reaching:
    ///
    /// * `curfew.extra_until` — a bedtime extension the parent granted tonight. Also the field a
    ///   crafted file would use to switch bedtime off, so it is ignored rather than merely
    ///   preserved.
    /// * `rules.enabled` — the pause toggle. `apply_routine` already decided this case: pausing is
    ///   "a temporary override, not something a preset should flip", and a restore is the same
    ///   shape. A parent who paused enforcement ten minutes ago does not expect a settings restore
    ///   to resume it behind them.
    pub fn apply_policy(&mut self, policy: Policy) {
        let paused = !self.rules.enabled;
        let extra_until = self.curfew.extra_until;

        self.curfew = policy.curfew;
        self.curfew.extra_until = extra_until;
        self.rules = policy.rules;
        self.rules.enabled = !paused;
        self.routines = policy.routines;
        self.language = policy.language;
    }

    pub fn load() -> Result<Self> {
        let path = data_paths().config;
        let raw = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "could not read config at {} — run `nestwatch install` first",
                path.display()
            )
        })?;
        let cfg: Config = serde_json::from_str(&raw).context("config file is malformed")?;
        if cfg.curfew.enabled
            && let Err(e) = cfg.curfew.validate()
        {
            tracing::warn!("curfew is enabled but invalid ({e}); it will not be enforced");
        }
        if let Err(e) = cfg.rules.validate() {
            tracing::warn!("usage rules are invalid ({e}); they will not be enforced");
        }
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let paths = data_paths();
        std::fs::create_dir_all(&paths.dir)
            .with_context(|| format!("could not create {}", paths.dir.display()))?;
        let json = serde_json::to_string_pretty(self)?;
        write_atomic(&paths.config, json.as_bytes())
            .with_context(|| format!("could not write {}", paths.config.display()))?;
        Ok(())
    }
}

/// Write `contents` to `path` atomically: fill a *private* temp file, flush it to disk, then
/// `rename` over the destination. A same-directory rename is atomic on NTFS and POSIX, so a
/// crash or power cut mid-write can never leave a truncated file — which matters most for
/// `config.json`: an unreadable config makes the service fail to start (locking the parent out
/// until reinstall), and a torn `usage_state.json` silently resets the day's budget. The temp
/// file is created inside the ACL-hardened data dir, so it's no more readable than the target.
///
/// **The temp name is unique per call, and must stay that way.** It used to be
/// `path.with_extension("tmp")` — correct against the adversary this function was written for,
/// a crash, and useless against the one it actually meets: a second writer. `config.json` has
/// eight of them (every `api::update_config` caller; `redeem_code` is unauthenticated and
/// child-reachable), and `update_config` releases the config lock before persisting. Two of
/// them called `File::create` on the same `config.tmp`, truncating under each other and
/// interleaving at overlapping offsets, and the rename published the blend. Measured over 300
/// rounds: 98 corrupt files, and the loser's rename failed every round because the winner had
/// already renamed the shared temp away. Atomicity and mutual exclusion are separate
/// properties; `sync_all` only ever bought the first.
///
/// Pinned by `concurrent_writers_never_interleave_into_one_file`.
pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    // Process id keeps two nestwatch processes apart (an `install` running beside the service);
    // the counter keeps two threads within this one apart. Neither alone is enough.
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));

    let fill = || -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents)?;
        // Flush the bytes to disk BEFORE the rename, or the rename could be persisted while the
        // contents are still buffered — exposing an empty file after a power cut.
        f.sync_all()
    };

    // On failure the scratch file is this call's alone, so removing it cannot disturb another
    // writer — and leaving it would litter the data dir one file per failed save.
    let result = fill().and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`Language::ALL`] really does list every variant.
    ///
    /// Derived from this file's own source rather than from a second hand-written list, for the
    /// reason `tests/spawn_paths.rs` gives: a fixture that mirrors a list in the code passes
    /// forever once the two drift, and the drift is silent. Adding `De` to the enum and not to
    /// `ALL` fails here, which is what stops every message test in `rules.rs` and `curfew.rs`
    /// from quietly skipping the new language.
    ///
    /// The check itself is in `testutil` because `ShotTier` needs the same one — copying it would
    /// have been the exact duplication both guards exist to forbid. It has two callers, so read
    /// `all_lists_every_shot_tier` before changing it.
    #[test]
    fn all_lists_every_language_variant() {
        crate::testutil::assert_all_lists_every_variant(
            include_str!("config.rs"),
            "pub enum Language {",
            Language::ALL.len(),
        );
    }

    #[test]
    fn config_round_trips_through_json() {
        let cfg = Config {
            port: 8443,
            password_hash: "$argon2id$abc".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back.port, 8443);
        assert_eq!(back.password_hash, "$argon2id$abc");
    }

    #[test]
    fn write_atomic_replaces_and_leaves_no_temp() {
        let dir = crate::testutil::ScratchDir::new("atomic");
        let path = dir.join("data.json");

        write_atomic(&path, b"first").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
        // A second write replaces the contents in place…
        write_atomic(&path, b"second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        // …and never leaves a scratch file behind. Checked by listing the directory rather
        // than probing one name: temp names now carry a pid and counter, so asserting that
        // `data.tmp` is absent would pass without testing anything.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .filter(|n| n != "data.json")
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    /// The test above proves atomicity against a *crash*: one writer, interrupted. It says
    /// nothing about a second writer, and the two are different properties.
    ///
    /// `config.json` has eight concurrent writers — every handler that calls `update_config`,
    /// which releases the config lock before persisting. One of them, `redeem_code`, is
    /// unauthenticated and child-reachable. When the temp path was derived from the target,
    /// every writer shared one `config.tmp`: `File::create` truncated it under whoever was
    /// mid-write, the two payloads interleaved at overlapping offsets, and the rename published
    /// the mixture. Measured before the fix, over 300 rounds: 98 files matched neither writer
    /// (one captured sample opened with B's bytes, closed with A's, and would not parse), and
    /// the loser's rename failed with ENOENT every single round.
    ///
    /// A corrupt config.json is the worst outcome this file has: the service will not start,
    /// which locks the parent out until reinstall.
    #[test]
    fn concurrent_writers_never_interleave_into_one_file() {
        let dir = crate::testutil::ScratchDir::new("atomic-conc");
        let path = dir.join("config.json");

        // Different lengths, so a torn write cannot accidentally look intact: if the shorter
        // payload lands over the longer one, the tail of the longer survives past its end.
        let a = format!(r#"{{"who":"A","pad":"{}"}}"#, "A".repeat(40_000));
        let b = format!(r#"{{"who":"B","pad":"{}"}}"#, "B".repeat(8_000));

        for round in 0..64 {
            let (pa, pb) = (path.clone(), path.clone());
            let (ca, cb) = (a.clone(), b.clone());
            let ha = std::thread::spawn(move || write_atomic(&pa, ca.as_bytes()));
            let hb = std::thread::spawn(move || write_atomic(&pb, cb.as_bytes()));
            let (ra, rb) = (ha.join().unwrap(), hb.join().unwrap());

            // Neither writer may fail. Sharing one temp path made the loser's rename ENOENT.
            ra.unwrap_or_else(|e| panic!("round {round}: writer A failed: {e}"));
            rb.unwrap_or_else(|e| panic!("round {round}: writer B failed: {e}"));

            // Last writer wins is fine. A blend of both is not.
            let got = std::fs::read_to_string(&path).unwrap();
            assert!(
                got == a || got == b,
                "round {round}: config.json is neither writer's content -- {} bytes, \
                 {} 'A' bytes and {} 'B' bytes in one file",
                got.len(),
                got.matches('A').count(),
                got.matches('B').count(),
            );
        }

        // Every writer's scratch file must be cleaned up, not just the winner's.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .filter(|n| n != "config.json")
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
    }

    #[test]
    fn config_without_new_fields_still_loads() {
        // Simulates a config.json written before curfew/rules existed.
        let legacy = r#"{"port":8443,"password_hash":"$argon2id$abc"}"#;
        let cfg: Config = serde_json::from_str(legacy).unwrap();
        assert!(!cfg.curfew.enabled);
        assert_eq!(cfg.rules.daily_budget_mins, 0);
        assert_eq!(cfg.extra.minutes, 0);
        // Upgrade safety: a config predating the `enabled` field must load as *enabled*, so an
        // upgrade never silently pauses screen-time enforcement.
        assert!(cfg.rules.enabled);
        assert!(cfg.routines.is_empty());
    }

    /// An instant to ask `rules_at` about. RFC3339 so the offset is explicit — these are
    /// selection tests, and a test whose answer depended on the machine's zone would be testing
    /// the wrong thing.
    fn at(s: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(s).expect("test timestamp")
    }

    /// A routine that applies between `start` and `end` on **every** day.
    ///
    /// The day selector is left empty on purpose: `window_active` and its day-attribution rule
    /// already have a thorough sweep in `curfew.rs`, and repeating it here would test that
    /// function twice while testing *selection* — which is what these tests are about — once.
    fn scheduled(name: &str, budget: u32, start: &str, end: &str) -> Routine {
        Routine {
            name: name.into(),
            rules: crate::rules::Rules {
                daily_budget_mins: budget,
                ..Default::default()
            },
            schedule: vec![crate::curfew::Window {
                start: start.into(),
                end: end.into(),
                days: Default::default(),
            }],
        }
    }

    /// A config whose base budget is 120, plus whatever routines are given.
    fn with_routines(routines: Vec<Routine>) -> Config {
        Config {
            rules: crate::rules::Rules {
                daily_budget_mins: 120,
                ..Default::default()
            },
            routines,
            ..Default::default()
        }
    }

    #[test]
    fn a_scheduled_routine_is_in_force_inside_its_window_and_not_outside_it() {
        let cfg = with_routines(vec![scheduled("Homework", 30, "16:00", "18:00")]);

        assert_eq!(
            cfg.rules_at(at("2026-09-02T17:00:00+02:00"))
                .daily_budget_mins,
            30,
            "inside the window the routine's budget is the one in force"
        );
        assert_eq!(
            cfg.active_routine_at(at("2026-09-02T17:00:00+02:00")),
            Some("Homework")
        );

        assert_eq!(
            cfg.rules_at(at("2026-09-02T19:00:00+02:00"))
                .daily_budget_mins,
            120,
            "outside it the base rules are, and nothing has been overwritten to get there"
        );
        assert_eq!(cfg.active_routine_at(at("2026-09-02T19:00:00+02:00")), None);
        // The end is exclusive, like every other window in this crate.
        assert_eq!(
            cfg.rules_at(at("2026-09-02T18:00:00+02:00"))
                .daily_budget_mins,
            120
        );
    }

    /// Pausing is a promise about the whole enforcer, so a window opening must not undo it.
    ///
    /// The failure this pins is quiet in the worst way: the parent switches enforcement off for the
    /// evening, and at 16:00 a schedule switches it back on with a 30-minute budget the child then
    /// runs out of.
    #[test]
    fn pause_beats_a_schedule() {
        let mut cfg = with_routines(vec![scheduled("Homework", 30, "16:00", "18:00")]);
        cfg.rules.enabled = false;

        let inside = at("2026-09-02T17:00:00+02:00");
        assert!(
            !cfg.rules_at(inside).enabled,
            "a paused install stays paused inside a scheduled window"
        );
        assert_eq!(cfg.rules_at(inside).daily_budget_mins, 120);
        assert_eq!(
            cfg.active_routine_at(inside),
            None,
            "and the dashboard is not told a routine is running while nothing is enforced"
        );
    }

    /// Overlap resolves by list order, which is the order the parent sees on the page.
    #[test]
    fn the_first_matching_routine_wins_when_windows_overlap() {
        let cfg = with_routines(vec![
            scheduled("Homework", 30, "16:00", "18:00"),
            scheduled("Quiet", 10, "17:00", "19:00"),
        ]);
        let both = at("2026-09-02T17:30:00+02:00");
        assert_eq!(cfg.rules_at(both).daily_budget_mins, 30);
        assert_eq!(cfg.active_routine_at(both), Some("Homework"));
        // …and the second still applies where the first does not reach.
        let only_second = at("2026-09-02T18:30:00+02:00");
        assert_eq!(cfg.rules_at(only_second).daily_budget_mins, 10);
        assert_eq!(cfg.active_routine_at(only_second), Some("Quiet"));
    }

    /// Every routine saved before schedules existed loads with an empty one, and an empty schedule
    /// must never match — otherwise upgrading the binary would silently automate presets the
    /// parent had only ever pressed by hand.
    #[test]
    fn a_routine_with_no_schedule_is_never_selected_automatically() {
        let cfg = with_routines(vec![Routine {
            name: "Weekend".into(),
            rules: crate::rules::Rules {
                daily_budget_mins: 240,
                ..Default::default()
            },
            schedule: Vec::new(),
        }]);
        for t in [
            "2026-09-02T00:00:00+02:00",
            "2026-09-02T12:00:00+02:00",
            "2026-09-02T23:59:00+02:00",
        ] {
            assert_eq!(cfg.rules_at(at(t)).daily_budget_mins, 120, "at {t}");
            assert_eq!(cfg.active_routine_at(at(t)), None, "at {t}");
        }
    }

    /// The name on the dashboard and the budget being enforced come from the same routine.
    ///
    /// They are two public functions, so nothing but this stops them drifting into naming one
    /// routine while enforcing another — which would be a worse dashboard than showing no name.
    #[test]
    fn the_named_routine_is_the_one_whose_rules_are_in_force() {
        let cfg = with_routines(vec![
            scheduled("Homework", 30, "16:00", "18:00"),
            scheduled("Wind down", 45, "20:00", "22:00"),
        ]);
        for t in [
            "2026-09-02T15:00:00+02:00",
            "2026-09-02T17:00:00+02:00",
            "2026-09-02T19:00:00+02:00",
            "2026-09-02T21:00:00+02:00",
        ] {
            let instant = at(t);
            let expected = match cfg.active_routine_at(instant) {
                Some(name) => {
                    cfg.routines
                        .iter()
                        .find(|r| r.name == name)
                        .expect("named routine exists")
                        .rules
                        .daily_budget_mins
                }
                None => cfg.rules.daily_budget_mins,
            };
            assert_eq!(
                cfg.rules_at(instant).daily_budget_mins,
                expected,
                "at {t} the named routine and the enforced budget disagree"
            );
        }
    }

    #[test]
    fn routines_round_trip_through_json() {
        let cfg = Config {
            routines: vec![Routine {
                name: "Homework".into(),
                rules: crate::rules::Rules {
                    daily_budget_mins: 30,
                    ..Default::default()
                },
                schedule: vec![crate::curfew::Window {
                    start: "16:00".into(),
                    end: "18:00".into(),
                    days: Default::default(),
                }],
            }],
            ..Default::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back.routines.len(), 1);
        assert_eq!(back.routines[0].name, "Homework");
        assert_eq!(back.routines[0].rules.daily_budget_mins, 30);
        // The schedule has to survive the file, or the automation silently reverts to manual on
        // the next service restart — which looks exactly like a parent misremembering setting it.
        assert_eq!(back.routines[0].schedule.len(), 1);
        assert_eq!(back.routines[0].schedule[0].start, "16:00");
        assert_eq!(back.routines[0].schedule[0].end, "18:00");
        assert!(
            back.rules_at(at("2026-09-02T17:00:00+02:00"))
                .daily_budget_mins
                == 30,
            "a routine reloaded from disk still applies on its schedule"
        );
    }

    /// A `config.json` written before schedules existed still loads, with its routines manual.
    #[test]
    fn a_routine_without_a_schedule_field_still_loads() {
        let json = r#"{
            "port": 8443,
            "password_hash": "",
            "routines": [{ "name": "Weekend", "rules": { "daily_budget_mins": 240 } }]
        }"#;
        let cfg: Config = serde_json::from_str(json).expect("legacy config must still parse");
        assert_eq!(cfg.routines.len(), 1);
        assert!(
            cfg.routines[0].schedule.is_empty(),
            "a missing schedule field means manual-only, not a parse error"
        );
        assert_eq!(cfg.active_routine_at(at("2026-09-02T17:00:00+02:00")), None);
    }
}
