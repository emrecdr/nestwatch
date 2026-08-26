//! Offline time codes: the parent pre-generates short redeemable codes worth N minutes; the
//! child types a code into the LAN page and it's added to today's budget — no parent action,
//! and no internet, needed at redemption time. Useful when the parent is away (leave a code) or
//! the network is down.
//!
//! Storage mirrors [`crate::timereq`]: **event-sourced JSON-Lines** over [`crate::jsonl::JsonlLog`]
//! (`issued` / `redeemed` lines), folded by `code` to the latest status, so codes survive the
//! auto-restarting service and each code is single-use.
//!
//! Security: codes are 6 Crockford-base32 characters — 1,073,741,824 combinations — drawn from the
//! OS CSPRNG. Brute-force guessing through the redeem endpoint is infeasible because of the
//! **throttle**, not the length: at its 5-per-minute per-IP limit that is roughly 408 years of
//! guessing with one code outstanding, and about 8 years at [`MAX_ACTIVE_CODES`]. The plaintext
//! codes live only in the SYSTEM+Administrators-only data dir, unreadable to the child.
//!
//! Six rather than eight is a deliberate trade (2026-08-26): eight was 1,024x more combinations and
//! correspondingly more to read off a note and retype without error, and the length was never what
//! made this safe. The alphabet already omits `I`, `L`, `O` and `U` for the same reason. If the
//! throttle is ever loosened, this length has to be revisited with it — they are one decision.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::Serialize;
use serde_json::json;

use crate::jsonl::JsonlLog;

/// Largest single code we mint.
pub const MAX_CODE_MINUTES: u32 = 240;
/// Cap on outstanding (unredeemed) codes, so the store can't grow without bound.
pub(crate) const MAX_ACTIVE_CODES: usize = 50;
/// Code length in characters. The alphabet and generator live in [`crate::token`], shared with
/// pairing tokens so there's one audited source of randomness.
const CODE_LEN: usize = 6;

/// An active (issued, not-yet-redeemed) code as surfaced to the parent UI.
#[derive(Debug, Serialize)]
pub struct ActiveCode {
    pub code: String,
    pub ts: String,
    pub minutes: u32,
}

/// The persisted code store.
pub struct TimeCodes {
    log: JsonlLog,
    /// Serializes each check-and-append (`issue`'s cap check, `redeem`'s consume) so the
    /// read → decide → append sequence is atomic. Without this, concurrent redemptions of the
    /// same single-use code all observe it as active and each grant the minutes.
    gate: Mutex<()>,
}

impl TimeCodes {
    pub fn new(path: PathBuf) -> Self {
        Self {
            log: JsonlLog::new(path),
            gate: Mutex::new(()),
        }
    }

    /// A no-op store (tests): `issue` returns a code but nothing persists, so `active` is empty
    /// and `redeem` always fails.
    pub fn disabled() -> Self {
        Self {
            log: JsonlLog::disabled(),
            gate: Mutex::new(()),
        }
    }

    /// Mint a new code worth `minutes`, or `None` if the active-code cap is reached. Returns the
    /// plaintext code for the parent to write down / hand over.
    pub fn issue(&self, minutes: u32) -> Option<String> {
        let _gate = self.gate.lock().unwrap_or_else(|p| p.into_inner());
        if self.active().len() >= MAX_ACTIVE_CODES {
            return None;
        }
        let code = crate::token::random(CODE_LEN);
        self.log
            .record("issued", json!({ "code": code, "minutes": minutes }));
        Some(code)
    }

    /// The still-active codes, newest first. Folds the event log by `code`: events come back
    /// newest-first, so the first status seen for a code is its latest.
    pub fn active(&self) -> Vec<ActiveCode> {
        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        for e in self.log.recent(usize::MAX) {
            let Some(code) = e.get("code").and_then(|v| v.as_str()) else {
                continue;
            };
            if !seen.insert(code.to_string()) {
                continue; // already have this code's latest status
            }
            if e.get("event").and_then(|v| v.as_str()) == Some("issued") {
                out.push(ActiveCode {
                    code: code.to_string(),
                    ts: e
                        .get("ts")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    minutes: e.get("minutes").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                });
            }
        }
        out
    }

    /// Redeem `input` (normalized: case-insensitive, punctuation/space ignored). Returns the
    /// minutes granted and marks the code used, or `None` if it isn't an active code.
    pub fn redeem(&self, input: &str) -> Option<u32> {
        let code = crate::token::normalize(input);
        if code.is_empty() {
            return None;
        }
        // Hold the gate across find → append so a single-use code can't be consumed twice by
        // concurrent redemptions (each would otherwise see it as still active and grant minutes).
        let _gate = self.gate.lock().unwrap_or_else(|p| p.into_inner());
        let found = self.active().into_iter().find(|c| c.code == code)?;
        self.log.record("redeemed", json!({ "code": code }));
        Some(found.minutes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (TimeCodes, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "nw-timecode-{}-{}",
            std::process::id(),
            // vary by a monotonic-ish suffix so parallel tests don't collide
            std::time::SystemTime::UNIX_EPOCH
                .elapsed()
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        (TimeCodes::new(dir.join("time_codes.jsonl")), dir)
    }

    /// The code a child has to read off a note and type is **six** characters.
    ///
    /// A literal rather than `CODE_LEN`, because asserting a constant against itself pins nothing:
    /// this is a product decision about what a person can retype without error, and it is also a
    /// security parameter, so a change to it should have to be made here as well as at the
    /// constant.
    ///
    /// Six characters of this 32-symbol alphabet is 1,073,741,824 combinations. Against the redeem
    /// endpoint's 5-per-minute per-IP limit that is ~408 years of guessing with one code
    /// outstanding, and ~8 years at `MAX_ACTIVE_CODES`. Eight characters was 1,024x more and
    /// correspondingly harder to type; the throttle, not the length, is what makes this infeasible.
    #[test]
    fn a_code_is_six_characters_long() {
        let (codes, _dir) = store();
        let code = codes.issue(30).unwrap();
        assert_eq!(
            code.len(),
            6,
            "a six-character code is short enough to read off a note and retype: got {code}"
        );
    }

    #[test]
    fn issue_active_redeem_roundtrip() {
        let (codes, dir) = store();
        let code = codes.issue(30).unwrap();
        assert_eq!(code.len(), CODE_LEN);

        let active = codes.active();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].minutes, 30);
        assert_eq!(active[0].code, code);

        // Redeeming with messy formatting still works and grants the minutes.
        let messy = format!("{}-{}", &code[..4], &code[4..]).to_lowercase();
        assert_eq!(codes.redeem(&messy), Some(30));
        // Single-use: gone from active, and a second redeem fails.
        assert!(codes.active().is_empty());
        assert_eq!(codes.redeem(&code), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn redeem_unknown_code_is_none() {
        let (codes, dir) = store();
        codes.issue(15).unwrap();
        assert_eq!(codes.redeem("NOTACODE"), None);
        assert_eq!(codes.redeem(""), None);
        assert_eq!(
            codes.active().len(),
            1,
            "a bad guess doesn't consume a code"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn codes_are_unique_and_from_the_alphabet() {
        let (codes, dir) = store();
        let a = codes.issue(10).unwrap();
        let b = codes.issue(10).unwrap();
        assert_ne!(a, b, "two mints differ");
        assert!(
            a.bytes().all(|c| crate::token::ALPHABET.contains(&c)),
            "only alphabet chars"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_redeem_grants_a_code_only_once() {
        // Regression: redeem must be atomic. Fire many threads at one single-use code and assert
        // exactly one wins — without the gate, several would each observe it as active and grant.
        use std::sync::Arc;
        let dir = std::env::temp_dir().join(format!(
            "nw-timecode-race-{}-{}",
            std::process::id(),
            std::time::SystemTime::UNIX_EPOCH
                .elapsed()
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let codes = Arc::new(TimeCodes::new(dir.join("time_codes.jsonl")));
        let code = codes.issue(30).unwrap();

        let mut handles = Vec::new();
        for _ in 0..16 {
            let codes = codes.clone();
            let code = code.clone();
            handles.push(std::thread::spawn(move || codes.redeem(&code)));
        }
        let wins = handles
            .into_iter()
            .filter_map(|h| h.join().unwrap())
            .count();
        assert_eq!(wins, 1, "a single-use code must redeem exactly once");
        assert!(codes.active().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn disabled_store_is_noop() {
        let codes = TimeCodes::disabled();
        assert!(codes.issue(10).is_some(), "returns a code");
        assert!(codes.active().is_empty(), "but nothing persisted");
        assert_eq!(codes.redeem("ABCDEFGH"), None);
    }
}
