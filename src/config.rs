//! Persisted configuration and the on-disk locations the app uses.
//!
//! Config is a tiny JSON file holding the listen port and the Argon2 password *hash*
//! (never the plaintext). It lives alongside the TLS cert/key in a per-user data dir:
//! `%PROGRAMDATA%\HostHealth` on Windows (bland, low-profile), `~/.config/nestwatch` on dev.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use chrono::NaiveDate;
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
/// Longest routine name we accept.
pub const MAX_ROUTINE_NAME: usize = 40;

/// A saved, named preset of usage [`Rules`](crate::rules::Rules) — e.g. "Homework", "Weekend" —
/// that the parent can apply to the live rules with one click.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Routine {
    pub name: String,
    pub rules: crate::rules::Rules,
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

impl Config {
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
    #[test]
    fn all_lists_every_language_variant() {
        let src = include_str!("config.rs");
        let body = src
            .split_once("pub enum Language {")
            .expect("the enum must still be named `Language`")
            .1
            .split_once("\n}")
            .expect("unterminated enum body")
            .0;
        let variants: Vec<&str> = body
            .lines()
            .map(str::trim)
            .filter(|l| {
                !l.is_empty()
                    && !l.starts_with("//")
                    && !l.starts_with("#[")
                    && l.ends_with(',')
                    && !l.contains(' ')
            })
            .collect();
        // A broken extractor must not make this vacuous — the same guard `spawn_paths` carries.
        assert!(
            variants.len() >= 2,
            "extracted {variants:?} from the enum body; the parser is broken, not the code"
        );
        assert_eq!(
            variants.len(),
            Language::ALL.len(),
            "the enum declares {variants:?} but `Language::ALL` has {} entries",
            Language::ALL.len()
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

    #[test]
    fn routines_round_trip_through_json() {
        let cfg = Config {
            routines: vec![Routine {
                name: "Homework".into(),
                rules: crate::rules::Rules {
                    daily_budget_mins: 30,
                    ..Default::default()
                },
            }],
            ..Default::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back.routines.len(), 1);
        assert_eq!(back.routines[0].name, "Homework");
        assert_eq!(back.routines[0].rules.daily_budget_mins, 30);
    }
}
