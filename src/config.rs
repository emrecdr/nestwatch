//! Persisted configuration and the on-disk locations the app uses.
//!
//! Config is a tiny JSON file holding the listen port and the Argon2 password *hash*
//! (never the plaintext). It lives alongside the TLS cert/key in a per-user data dir:
//! `%PROGRAMDATA%\HostHealth` on Windows (bland, low-profile), `~/.config/nestwatch` on dev.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::curfew::Curfew;

pub const DEFAULT_PORT: u16 = 8443;

/// The current local calendar day — the single key the grant writer (approve handler) and
/// reader (rules enforcer) both use, so a future clock/timezone policy lives in one place.
pub fn today() -> NaiveDate {
    chrono::Local::now().date_naive()
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
        self.minutes += minutes;
    }
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
}

/// Resolved on-disk locations, derived from [`data_dir`].
pub struct DataPaths {
    pub dir: PathBuf,
    pub config: PathBuf,
    pub cert: PathBuf,
    pub key: PathBuf,
}

pub fn data_paths() -> DataPaths {
    let dir = data_dir();
    DataPaths {
        config: dir.join("config.json"),
        cert: dir.join("cert.pem"),
        key: dir.join("key.pem"),
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

/// Write `contents` to `path` atomically: fill a sibling temp file, flush it to disk, then
/// `rename` over the destination. A same-directory rename is atomic on NTFS and POSIX, so a
/// crash or power cut mid-write can never leave a truncated file — which matters most for
/// `config.json`: an unreadable config makes the service fail to start (locking the parent out
/// until reinstall), and a torn `usage_state.json` silently resets the day's budget. The temp
/// file is created inside the ACL-hardened data dir, so it's no more readable than the target.
pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents)?;
        // Flush the bytes to disk BEFORE the rename, or the rename could be persisted while the
        // contents are still buffered — exposing an empty file after a power cut.
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let dir = std::env::temp_dir().join(format!("nw-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("data.json");

        write_atomic(&path, b"first").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
        // A second write replaces the contents in place…
        write_atomic(&path, b"second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        // …and never leaves the sibling temp file behind.
        assert!(!dir.join("data.tmp").exists());

        let _ = std::fs::remove_dir_all(&dir);
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
    }
}
