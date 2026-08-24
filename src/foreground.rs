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

/// Fold one tick's bounded figures into the running daily map.
///
/// Separate from [`clamp`] so the bound cannot be skipped by a caller that only wanted to
/// accumulate: the only way to obtain the map this takes is to have gone through `clamp`.
pub fn accrue(running: &mut BTreeMap<String, u64>, bounded: BTreeMap<String, u64>) {
    for (name, secs) in bounded {
        let slot = running.entry(name).or_insert(0);
        *slot = slot.saturating_add(secs);
    }
}

/// What a browser window's title reveals, once the browser's own suffix is stripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserPage {
    /// The page title as the browser rendered it.
    pub page: String,
    /// Which browser, by its title suffix.
    pub browser: &'static str,
}

/// Recognise a browser window by its title suffix and pull the page title out of it.
///
/// This is the whole of the "what was he looking at on the web" feature, and its limits are the
/// point: a page *title*, never a URL and never a domain. `"Roblox - Google Chrome"` says the tab
/// said Roblox, which is enough to separate an evening of Roblox from an evening of homework, and
/// not enough to reconstruct browsing history. Getting domains would mean reconfiguring the
/// child's browsers; see `docs/FOREGROUND-TRACKING.md`.
///
/// Returns `None` for any window that is not a recognised browser, which is the common case.
pub fn browser_page(title: &str) -> Option<BrowserPage> {
    /// Title suffixes, longest-first where one could shadow another. Firefox is listed twice
    /// because it separates with an em dash, and matching only `" - "` would miss every Firefox
    /// window — which reads as "he never used Firefox" rather than as a bug.
    const SUFFIXES: &[(&str, &str)] = &[
        (" - Google Chrome", "Google Chrome"),
        (" — Mozilla Firefox", "Mozilla Firefox"),
        (" - Mozilla Firefox", "Mozilla Firefox"),
        (" - Microsoft Edge", "Microsoft Edge"),
        (" - Brave", "Brave"),
    ];

    let (page, browser) = SUFFIXES
        .iter()
        .find_map(|(suffix, name)| Some((title.strip_suffix(suffix)?, *name)))?;

    Some(BrowserPage {
        page: strip_tab_count(page).to_string(),
        browser,
    })
}

/// Drop Edge's `" and 3 more pages"` tail. That is window chrome describing how many tabs are
/// open, not part of what the child was reading, and leaving it in would make the same page look
/// like a different one every time another tab opened.
fn strip_tab_count(page: &str) -> &str {
    for tail in [" more pages", " more page"] {
        if let Some(head) = page.strip_suffix(tail)
            && let Some((rest, count)) = head.rsplit_once(' ')
            && !count.is_empty()
            && count.chars().all(|c| c.is_ascii_digit())
            && let Some(stripped) = rest.strip_suffix(" and")
        {
            return stripped;
        }
    }
    page
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

    #[test]
    fn accrual_adds_to_what_is_already_there() {
        let mut running: BTreeMap<String, u64> = BTreeMap::new();
        accrue(&mut running, clamp(sample(&[("roblox.exe", 20)]), 30));
        accrue(
            &mut running,
            clamp(sample(&[("roblox.exe", 10), ("chrome.exe", 5)]), 30),
        );

        assert_eq!(running.get("roblox.exe"), Some(&30), "20 + 10");
        assert_eq!(running.get("chrome.exe"), Some(&5));
    }

    /// Saturating, for the same reason every other accumulator in this codebase is: a release
    /// build does not check overflow, and a wrapped total reads as a small, believable number.
    #[test]
    fn accrual_saturates_instead_of_wrapping() {
        let mut running: BTreeMap<String, u64> = BTreeMap::new();
        running.insert("roblox.exe".into(), u64::MAX);
        accrue(&mut running, clamp(sample(&[("roblox.exe", 30)]), 30));
        assert_eq!(running.get("roblox.exe"), Some(&u64::MAX));
    }

    #[test]
    fn a_chrome_window_yields_its_page_title() {
        let got = browser_page("Roblox - Google Chrome").expect("Chrome must be recognised");
        assert_eq!(got.page, "Roblox");
        assert_eq!(got.browser, "Google Chrome");
    }

    /// Firefox separates with an em dash, not a hyphen. Matching only `" - "` silently misses
    /// every Firefox window, which would look like "he never used Firefox" rather than a bug.
    #[test]
    fn firefox_uses_an_em_dash() {
        let got = browser_page("Wikipedia — Mozilla Firefox").expect("Firefox must be recognised");
        assert_eq!(got.page, "Wikipedia");
        assert_eq!(got.browser, "Mozilla Firefox");
    }

    /// Edge appends a tab count when several are open; it is chrome, not page title.
    #[test]
    fn edge_drops_its_and_n_more_pages_suffix() {
        let got = browser_page("Roblox and 3 more pages - Microsoft Edge")
            .expect("Edge must be recognised");
        assert_eq!(got.page, "Roblox");
    }

    #[test]
    fn a_non_browser_window_is_not_a_page() {
        assert_eq!(browser_page("Untitled - Notepad"), None);
        assert_eq!(browser_page("Roblox"), None, "the game itself is not a page");
        assert_eq!(browser_page(""), None);
    }

    /// A page whose own title ends in a browser name must not be mistaken for chrome.
    #[test]
    fn only_the_trailing_suffix_counts() {
        let got = browser_page("How to uninstall Google Chrome - Google Chrome")
            .expect("still a Chrome window");
        assert_eq!(
            got.page, "How to uninstall Google Chrome",
            "only the final suffix is the browser's"
        );
    }
}
