//! Per-app **foreground time**: how long each app was actually in front of the child.
//!
//! The counterpart to `rules::Usage::per_app_secs`, which counts an app while its process *runs*.
//! That number is conservative for enforcement and misleading as a report — a minimised game and a
//! game being played look identical in it. See `docs/FOREGROUND-TRACKING.md`.
//!
//! This module is the **pure** half: parsing and bounding the reports that arrive from the watcher
//! helper. It has no clock, no filesystem, and no Win32 — so all of it is unit-tested on the dev
//! machine, unlike the watcher itself, which can only be verified on the target PC.
//!
//! # The input is untrusted
//!
//! The watcher must run in the child's session to see the child's windows, which means it runs *as
//! the child* — and this project's threat model already says the child is the adversary. Everything
//! arriving from it is therefore attacker-controlled, and [`clamp`] is what makes it safe to add to
//! a report a parent will read.
//!
//! Two bounds, because the obvious one alone is not enough:
//!
//! * **Per app** — no single app can have been focused for longer than the tick lasted.
//! * **Across all apps** — only one window holds focus at a time, so the *sum* cannot exceed the
//!   tick either. A forged report claiming the full tick for each of twenty apps passes a per-app
//!   check and fails this one. This is the bound worth keeping.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One report from the watcher: seconds of focus per app since its previous report.
///
/// Keys are normalized process names (`"roblox.exe"`), matching how `rules::norm` keys the
/// enforcement tally, so the two can be shown side by side without a second naming scheme.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Sample {
    #[serde(default)]
    pub apps: BTreeMap<String, u64>,
}

/// Parse one JSONL line from the watcher. `None` for anything malformed.
///
/// A corrupt or truncated line must never be fatal: the watcher writes to a pipe that can be cut
/// mid-line by a session ending or the child killing the process, and a partial write is expected
/// rather than exceptional.
pub fn parse_sample(line: &str) -> Option<Sample> {
    serde_json::from_str(line).ok()
}

/// Bound an untrusted [`Sample`] by the seconds that actually elapsed during the tick.
///
/// When the reported total exceeds `elapsed_secs`, every entry is scaled down proportionally so the
/// total fits. Integer division floors, so the result **understates** rather than overstates — the
/// same direction `countdown` already chooses deliberately, and the safe one for a figure a parent
/// will read as fact.
///
/// Zero-valued entries are dropped: they carry no information and would otherwise let a forged
/// report pad the map with thousands of app names.
pub fn clamp(sample: Sample, elapsed_secs: u64) -> BTreeMap<String, u64> {
    if elapsed_secs == 0 {
        return BTreeMap::new();
    }

    // Saturating, because this total is computed from numbers the child could have chosen. A
    // release build does not check overflow, so a plain sum could wrap to something small and
    // turn the bound below into a no-op — the one outcome this function exists to prevent.
    let claimed = sample
        .apps
        .values()
        .fold(0u64, |acc, secs| acc.saturating_add(*secs));

    let bounded = if claimed > elapsed_secs {
        // Scale every entry by elapsed/claimed. `u128` because the numerator is a product of two
        // attacker-influenced `u64`s; the division floors, so the result understates.
        //
        // The per-app bound falls out of this rather than needing its own pass: a lone app
        // claiming 9,000s of a 30s tick is simply the case where it is the entire total.
        sample
            .apps
            .into_iter()
            .map(|(name, secs)| {
                let scaled =
                    u128::from(secs) * u128::from(elapsed_secs) / u128::from(claimed.max(1));
                (name, u64::try_from(scaled).unwrap_or(elapsed_secs))
            })
            .collect()
    } else {
        sample.apps
    };

    bounded.into_iter().filter(|(_, secs)| *secs > 0).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(pairs: &[(&str, u64)]) -> Sample {
        Sample {
            apps: pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect(),
        }
    }

    #[test]
    fn a_well_formed_line_parses() {
        let got = parse_sample(r#"{"apps":{"roblox.exe":30}}"#).expect("a valid line must parse");
        assert_eq!(got, sample(&[("roblox.exe", 30)]));
    }

    #[test]
    fn a_malformed_line_is_skipped_not_fatal() {
        // A pipe cut mid-write is expected, not exceptional.
        assert_eq!(parse_sample(r#"{"apps":{"roblox.exe":3"#), None);
        assert_eq!(parse_sample(""), None);
        assert_eq!(parse_sample("not json at all"), None);
    }

    #[test]
    fn an_honest_report_passes_through_unchanged() {
        let got = clamp(sample(&[("roblox.exe", 20), ("chrome.exe", 10)]), 30);
        assert_eq!(got.get("roblox.exe"), Some(&20));
        assert_eq!(got.get("chrome.exe"), Some(&10));
    }

    #[test]
    fn one_app_claiming_more_than_the_tick_is_clamped() {
        let got = clamp(sample(&[("homework.exe", 9_000)]), 30);
        assert_eq!(
            got.get("homework.exe"),
            Some(&30),
            "no app can be focused for longer than the tick lasted"
        );
    }

    /// The bound a per-app check alone would miss.
    ///
    /// Twenty apps each claiming the full tick is twenty individually-plausible numbers that sum to
    /// twenty times reality. Only one window holds focus at a time, so the total is the real bound.
    #[test]
    fn a_forged_total_across_many_apps_is_clamped_to_the_tick() {
        let forged: Vec<(String, u64)> = (0..20).map(|i| (format!("app{i}.exe"), 30)).collect();
        let s = Sample {
            apps: forged.into_iter().collect(),
        };

        let got = clamp(s, 30);
        let total: u64 = got.values().sum();

        assert!(
            total <= 30,
            "the sum of focus time cannot exceed the tick, got {total}"
        );
    }

    /// Scaling floors, so a bounded report is never inflated by rounding.
    #[test]
    fn scaling_understates_rather_than_overstates() {
        // Three apps claiming 100s each inside a 10s tick: 10/300 of each is 3.33s.
        let got = clamp(sample(&[("a.exe", 100), ("b.exe", 100), ("c.exe", 100)]), 10);
        let total: u64 = got.values().sum();
        assert!(total <= 10, "must not exceed the tick, got {total}");
        for (name, secs) in &got {
            assert!(*secs <= 4, "{name} should floor to 3, got {secs}");
        }
    }

    #[test]
    fn a_zero_second_entry_is_dropped() {
        let got = clamp(sample(&[("idle.exe", 0), ("roblox.exe", 5)]), 30);
        assert!(
            !got.contains_key("idle.exe"),
            "a zero carries no information and would let a forged report pad the map"
        );
        assert_eq!(got.get("roblox.exe"), Some(&5));
    }

    /// A tick that took no time can charge no time — and must not divide by zero doing it.
    #[test]
    fn a_zero_length_tick_charges_nothing() {
        let got = clamp(sample(&[("roblox.exe", 30)]), 0);
        assert!(got.is_empty(), "no elapsed time means nothing to charge");
    }
}
