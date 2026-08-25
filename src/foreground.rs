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
    /// Seconds per **browser page title**, for the intervals where the focused window was a
    /// browser. Empty for everything else, which is most windows.
    ///
    /// Separate from `apps` rather than folded into it: the keys are a different kind of thing
    /// (`"Roblox"`, not `"chrome.exe"`), they are far higher-cardinality, and the enforcement tally
    /// is keyed on process names — mixing them would let a page title collide with an app rule.
    #[serde(default)]
    pub pages: BTreeMap<String, u64>,
}

/// Largest number of distinct page titles kept from one report.
///
/// Page titles are the highest-cardinality thing this system records and they come from a process
/// running as the child: every tab, every video, every renamed document is a new key. Without a cap
/// a script that retitles a window in a loop grows `usage_state.json` and the daily rollup without
/// bound. The heaviest entries are kept, which is also the only part anyone reads.
pub const MAX_PAGES: usize = 40;

/// Largest number of distinct **executables** carried in memory or stored for a day.
///
/// Far above any real machine — a busy PC sees tens of distinct foreground apps in a day, not
/// hundreds — because this is a backstop against a forged report, not a product decision. It has to
/// exist for the same reason [`MAX_PAGES`] does: the report arrives from a process running as the
/// child, so "how many programs are installed" bounds the honest case and nothing else.
///
/// Higher than `MAX_PAGES` because the two are different bets. Dropping a page title loses display
/// text; dropping an executable loses measured time that the enforcement tally is shown beside, so
/// the ceiling is set where it cannot plausibly be reached by a real user.
pub const MAX_APPS: usize = 200;

/// Largest JSON line accepted from the watcher pipe, in bytes.
///
/// Sized from the worst line an **honest** watcher can produce, which is the failure this limit
/// could cause rather than prevent: set it below that and real samples are discarded as if forged,
/// and the symptom is a child who appears to have used nothing.
///
/// That worst case is **170,170 bytes** — [`MAX_PAGES`] titles at the 512 UTF-16 units
/// `window_title` reads, plus the apps a 30-second emit can drain, every character worst-casing to
/// a six-byte `\uXXXX` escape. Measured, not estimated: `the_read_limit_clears_the_largest_honest_line`
/// builds that sample and asserts it fits, so raising `MAX_PAGES` or either watcher buffer without
/// raising this fails there rather than on a child's PC. One MiB leaves about six times over.
///
/// 64 KiB would be the natural round number and is under the real figure by a factor of three.
pub const MAX_LINE: u64 = 1024 * 1024;

/// Keep the `n` heaviest entries, dropping the rest. Ties break by name so the result is stable.
///
/// Private: outside this module the count bound is reached only through [`accrue_capped`], which
/// is what stops a caller from folding data in and forgetting to bound it.
fn retain_top(map: &mut BTreeMap<String, u64>, n: usize) {
    if map.len() <= n {
        return;
    }
    let mut by_weight: Vec<(String, u64)> = std::mem::take(map).into_iter().collect();
    by_weight.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    by_weight.truncate(n);
    *map = by_weight.into_iter().collect();
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
pub fn clamp(sample: Sample, elapsed_secs: u64) -> Sample {
    let mut pages = bound(sample.pages, elapsed_secs);
    // Cap after bounding, so what survives is the heaviest *real* time rather than the heaviest
    // claim. Pages only, because this bounds one report and apps cannot exceed `elapsed_secs`
    // entries in one — not because executables are a safe keyspace. They are not, and the count
    // bound that matters is [`accrue_capped`], applied wherever a map *accumulates*. See `MAX_APPS`.
    retain_top(&mut pages, MAX_PAGES);

    Sample {
        apps: bound(sample.apps, elapsed_secs),
        pages,
    }
}

/// Bound one map of seconds-per-key by the seconds that actually elapsed.
///
/// Both maps get the same treatment for the same reason: only one window holds focus at a time, so
/// neither the apps nor the pages recorded for an interval can sum to more than the interval.
fn bound(map: BTreeMap<String, u64>, elapsed_secs: u64) -> BTreeMap<String, u64> {
    if elapsed_secs == 0 {
        return BTreeMap::new();
    }

    // Saturating, because this total is computed from numbers the child could have chosen. A
    // release build does not check overflow, so a plain sum could wrap to something small and
    // turn the bound below into a no-op — the one outcome this function exists to prevent.
    let claimed = map
        .values()
        .fold(0u64, |acc, secs| acc.saturating_add(*secs));

    let mut bounded = if claimed > elapsed_secs {
        // Scale every entry by elapsed/claimed. `u128` because the numerator is a product of two
        // attacker-influenced `u64`s; the division floors, so the result understates.
        //
        // The per-key bound falls out of this rather than needing its own pass: a lone app
        // claiming 9,000s of a 30s tick is simply the case where it is the entire total.
        map.into_iter()
            .map(|(name, secs)| {
                let scaled =
                    u128::from(secs) * u128::from(elapsed_secs) / u128::from(claimed.max(1));
                (name, u64::try_from(scaled).unwrap_or(elapsed_secs))
            })
            .collect()
    } else {
        map
    };

    // In place: the honest path leaves this map untouched, where rebuilding it would move every
    // key and reallocate every node twice a tick for nothing.
    bounded.retain(|_, secs| *secs > 0);
    bounded
}

/// Decide whether the user counts as away, and **as of when**, from how long ago their last input
/// was. Returns the pair [`Tracker::set_idle`] takes.
///
/// This is what makes idle handling exact rather than approximate, and it is subtle enough to be
/// worth stating. The obvious implementation flips a flag once the threshold trips — which credits
/// the whole grace period twice over, once as it elapses and again on every poll until something
/// notices. But the OS reports how long ago the last input was, so the instant presence ended is
/// known: `last_input + grace`. Handing the tracker *that* timestamp credits the grace exactly
/// once and nothing after it.
///
/// It lives here rather than beside the `GetLastInputInfo` call that feeds it because that call
/// sits in a Windows-only module the dev machine never compiles — which would leave the one piece
/// of genuinely tricky arithmetic in this feature as the one piece with no test. Taking the idle
/// span as a plain number moves it back where it can be pinned.
pub fn idle_state(now_ms: u64, idle_ms: u64, idle_after_ms: u64) -> (bool, u64) {
    if idle_ms < idle_after_ms {
        return (false, now_ms);
    }
    // Saturating both ways: the tick counter this comes from wraps every ~49.7 days and is
    // documented as "not guaranteed to be incremental", so a nonsense span must clamp to "credited
    // nothing since before now" rather than wrap into a timestamp in the future — which the
    // tracker would then refuse to move backwards from, freezing accounting for the session.
    let credited_until = now_ms.saturating_sub(idle_ms).saturating_add(idle_after_ms);
    (true, credited_until.min(now_ms))
}

/// Read one newline-terminated line from the watcher pipe, never holding more than `max` bytes.
///
/// Returns `Ok(false)` once the pipe is done. On `Ok(true)`, `buf` holds the line without its
/// newline — or is **empty** when the line was over-long and got skipped, which needs no special
/// handling from the caller because an empty line does not parse as a sample.
///
/// `BufRead::lines` cannot do this job. It grows one `String` until it meets a newline, so a writer
/// that never sends one takes the reader's memory with it — and the reader here is the SYSTEM
/// service, reading a pipe from a process that runs as the child. By the time a line could be
/// inspected and rejected, the allocation that mattered has already happened; the limit has to be
/// on the read itself.
///
/// A trailing fragment with no newline is discarded rather than parsed. [`crate::watcher`] always
/// terminates what it writes, so an unterminated tail is a pipe cut mid-write — the torn read
/// [`parse_sample`] describes, and not a record that was ever complete.
pub fn read_bounded_line<R: std::io::BufRead>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max: u64,
) -> std::io::Result<bool> {
    use std::io::{BufRead, Read};

    buf.clear();
    if reader.by_ref().take(max).read_until(b'\n', buf)? == 0 {
        return Ok(false);
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        return Ok(true);
    }

    // Over-long, or torn at EOF. Skip to the next newline so one bad line costs one line rather
    // than desynchronising the stream — reusing `buf` as the scratch, so the ceiling on what is
    // held at once stays `max` however long the offending line turns out to be.
    loop {
        buf.clear();
        let read = reader.by_ref().take(max).read_until(b'\n', buf)?;
        // Ask the buffer, not the byte count. "Stopped short of the limit" looks like the same
        // question and is not: a chunk that fills the limit *and* ends on the newline is a line
        // that just ended, and treating it as unfinished swallows the following line whole.
        let ended = buf.last() == Some(&b'\n');
        buf.clear();
        if read == 0 {
            return Ok(false);
        }
        if ended {
            return Ok(true);
        }
    }
}

/// Fold one tick's bounded figures into a running map and bound how many keys it may hold.
///
/// **The only accrual this module exposes**, because both halves are needed everywhere and keeping
/// them together is what makes the pair unskippable. [`clamp`] bounds what the numbers may *say*;
/// `cap` bounds how many of them there may *be*, and the second is not implied by the first — a
/// report of ten thousand one-second entries passes every value check ever written. Every caller
/// previously did `accrue` then `retain_top` by hand, which is two things that must agree at four
/// call sites, and the map that is actually persisted was the one where they did not.
///
/// The heaviest entries survive, so a flood costs the flood: an app with real hours behind it
/// outweighs any number of forgeries. Free on the honest path — `retain_top` returns immediately
/// while the map fits.
pub fn accrue_capped(
    running: &mut BTreeMap<String, u64>,
    bounded: BTreeMap<String, u64>,
    cap: usize,
) {
    accrue(running, bounded);
    retain_top(running, cap);
}

/// Fold one tick's bounded figures into the running daily map.
///
/// Separate from [`clamp`] so the bound cannot be skipped by a caller that only wanted to
/// accumulate: the only way to obtain the map this takes is to have gone through `clamp`. Private
/// for the same reason one level up — outside this module the only way in is [`accrue_capped`], so
/// the count bound cannot be forgotten either.
fn accrue(running: &mut BTreeMap<String, u64>, bounded: BTreeMap<String, u64>) {
    for (name, secs) in bounded {
        let slot = running.entry(name).or_insert(0);
        *slot = slot.saturating_add(secs);
    }
}

/// Whether `exe` is a browser whose window title is worth reading, given an already-normalised
/// process name (see `rules::norm`).
///
/// This is the half of page attribution that cannot be forged. A window title is chosen by the
/// process that owns it, so `browser_page` alone would let anything claim page time by calling
/// itself `"Roblox - Google Chrome"` — injecting pages the child never visited, or filling the
/// capped list to evict the real ones. Requiring the executable to agree costs one comparison
/// against a fixed list and takes the decision out of the child's hands.
///
/// It also saves a `GetWindowTextW` and a `String` on every non-browser window, which is most of
/// them — but that is the smaller reason.
pub fn is_browser(exe: &str) -> bool {
    BROWSERS.iter().any(|(known, _)| *known == exe)
}

/// Every browser this can attribute, with the title suffixes it is known to use.
///
/// **One table on purpose.** These were two lists — executables here, suffixes there — and they
/// had already drifted: `opera.exe` and `vivaldi.exe` were admitted with no suffix to match, so an
/// Opera user paid a title read on every wake and silently got no page attribution at all. Pairing
/// them means a browser cannot be admitted without saying how to parse it, and adding one is a
/// single entry rather than two edits that must agree.
///
/// Firefox appears with two suffixes because it separates with an em dash; matching only `" - "`
/// would miss every Firefox window, which reads as "he never used Firefox" rather than as a bug.
///
/// Deliberately short. A browser whose title format has not been confirmed is left out rather than
/// guessed at — an entry that never matches is indistinguishable, to a parent, from a child who
/// never opened it.
const BROWSERS: &[(&str, &[&str])] = &[
    ("chrome.exe", &[" - Google Chrome"]),
    ("msedge.exe", &[" - Microsoft Edge"]),
    ("firefox.exe", &[" — Mozilla Firefox", " - Mozilla Firefox"]),
    ("brave.exe", &[" - Brave"]),
];

/// Recognise a browser window by its title suffix and pull the page title out of it.
///
/// This is the whole of the "what was he looking at on the web" feature, and its limits are the
/// point: a page *title*, never a URL and never a domain. `"Roblox - Google Chrome"` says the tab
/// said Roblox, which is enough to separate an evening of Roblox from an evening of homework, and
/// not enough to reconstruct browsing history. Getting domains would mean reconfiguring the
/// child's browsers; see `docs/FOREGROUND-TRACKING.md`.
///
/// Returns `None` for any window that is not a recognised browser, which is the common case.
/// The page is borrowed from `title` — which browser it was is deliberately not returned, because
/// nothing displays it and an unread field is a thing to keep correct for no one.
pub fn browser_page<'a>(exe: &str, title: &'a str) -> Option<&'a str> {
    // The window must carry *this* browser's suffix, not merely some browser's — Chrome credited
    // through Firefox's suffix would be one more thing a forged title could exploit.
    let (_, suffixes) = BROWSERS.iter().find(|(known, _)| *known == exe)?;
    let page = suffixes
        .iter()
        .find_map(|suffix| title.strip_suffix(suffix))?;

    Some(strip_tab_count(page))
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

/// Turns a stream of focus changes into per-app time.
///
/// The whole of the measurement, and deliberately free of Win32: it takes an app name and a
/// millisecond timestamp, both injected. The watcher supplies real ones; the tests supply chosen
/// ones. That split is what makes the accounting provable on a machine that isn't Windows, which
/// matters here because the Win32 half can only ever be checked by running it on the target PC.
///
/// Time is carried in **milliseconds** and only rounded down to whole seconds when a sample is
/// drained, with the remainder kept. Flooring on every drain instead would quietly lose up to a
/// second per app per interval — at a 30-second cadence that is up to two minutes a day, per app,
/// always in the direction of under-reporting.
pub struct Tracker {
    /// The app holding focus, and when it took it. `None` while no window is foreground at all —
    /// the lock screen, the UAC secure desktop, or the moment after a window closes.
    current: Option<(String, u64)>,
    /// Whether the user is currently idle. Time does not accrue while idle: an app left open in
    /// front of an empty chair is not screen time, and counting it is how a tracker ends up
    /// reporting an eight-hour Roblox session for a PC nobody touched.
    idle: bool,
    /// Whole milliseconds banked per app since the last drain, including sub-second carry.
    millis: BTreeMap<String, u64>,
    /// Largest number of keys to carry between drains, when the keyspace is unbounded.
    ///
    /// `None` for executables — a closed set, and dropping one would lose real measured time.
    /// `Some` for page titles, where every tab is a new key and nothing limits how many there can
    /// be. Without it the prune below only ever drops a key whose carry lands exactly on a whole
    /// second, so a briefly-focused title is kept for the life of the process.
    cap: Option<usize>,
}

impl Tracker {
    /// A tracker over a bounded keyspace — executables.
    pub fn new() -> Self {
        Self {
            current: None,
            idle: false,
            millis: BTreeMap::new(),
            cap: None,
        }
    }

    /// A tracker over an unbounded keyspace, keeping at most `cap` keys between drains.
    pub fn capped(cap: usize) -> Self {
        Self {
            cap: Some(cap),
            ..Self::new()
        }
    }

    /// How many keys are carrying time forward. Exists for the tests that pin the cap.
    #[cfg(test)]
    fn tracked_keys(&self) -> usize {
        self.millis.len()
    }

    /// Credit the focused app for the time since it was last accounted, and move the marker to
    /// `now_ms`. The single place time is ever added, so every entry point below can only
    /// under-count by forgetting to call it — never double-count by calling it twice.
    fn bank(&mut self, now_ms: u64) {
        let Some((app, since)) = &mut self.current else {
            return;
        };
        // Idle time is measured but not credited: the marker still advances, so the seconds an
        // absent user "spent" are dropped rather than banked when they return.
        let delta = if self.idle {
            0
        } else {
            now_ms.saturating_sub(*since)
        };
        // Never move the marker backwards. `clock.rs` already anchors the wall clock; this is the
        // same defence one level down, so a clock that jumps back cannot turn into credit later.
        if now_ms > *since {
            *since = now_ms;
        }
        // Clone inside the guard, not before it. The idle and zero-delta paths are the common ones
        // — the watcher banks up to four times a wake, per tracker, and most of those add nothing.
        if delta > 0 {
            match self.millis.get_mut(app.as_str()) {
                Some(slot) => *slot = slot.saturating_add(delta),
                None => {
                    let app = app.clone();
                    self.millis.insert(app, delta);
                }
            }
        }
    }

    /// Note that `app` now holds focus (`None` when nothing does). Banks whatever the previous app
    /// earned up to `now_ms`.
    pub fn focus(&mut self, app: Option<&str>, now_ms: u64) {
        self.bank(now_ms);
        self.current = app.map(|a| (a.to_string(), now_ms));
    }

    /// Note that the user went idle or came back. Banks time up to `now_ms` before the transition
    /// takes effect, so the boundary second is charged to the side it belongs to.
    pub fn set_idle(&mut self, idle: bool, now_ms: u64) {
        self.bank(now_ms);
        self.idle = idle;
    }

    /// Take everything banked so far as whole seconds, keeping sub-second remainders for next time.
    /// The currently-focused key keeps its focus — a drain is a reporting boundary, not a reset.
    ///
    /// Returns a bare map rather than a [`Sample`] because the watcher runs **two** of these: one
    /// keyed by executable and one by browser page title. The state machine is the same for both.
    pub fn drain(&mut self, now_ms: u64) -> BTreeMap<String, u64> {
        self.bank(now_ms);

        let mut whole = BTreeMap::new();
        // One pass: report the whole seconds, keep the remainder, drop anything carrying nothing.
        self.millis.retain(|name, remaining| {
            if *remaining >= 1000 {
                whole.insert(name.clone(), *remaining / 1000);
                *remaining %= 1000;
            }
            *remaining > 0
        });

        // That prune alone is weaker than it looks: it only drops a key whose carry happens to land
        // on an exact second, so a key focused for a fraction of a second is kept indefinitely. For
        // executables that is harmless — the keyspace is closed. For page titles it is a slow leak
        // in the one process whose memory footprint this feature is judged on, so a capped tracker
        // keeps only the heaviest keys. The cost is under a second of carry per dropped key, once.
        if let Some(cap) = self.cap {
            retain_top(&mut self.millis, cap);
        }

        whole
    }
}

impl Default for Tracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Where the watcher's samples wait between the reader thread that receives them and the enforcer
/// tick that folds them into the day's tally.
///
/// Cheap to clone (one `Arc`), because the reader thread and the enforcer each hold one.
///
/// The important thing this type gets right is telling **"the watcher reported nothing"** apart
/// from **"there is no watcher"**. A child at an idle desktop and a child who killed the helper
/// both produce zero minutes, and only the first of those is a fact. [`Feed::drain`] returns
/// `None` for the second, so the day is recorded as *not measured* rather than as measured-zero —
/// the same distinction `screentime::DayRow` already draws, for the same reason.
/// `None` means no watcher has reported since the last drain. That is the whole point of the type,
/// so it is the shape of the type: there is no separate flag to keep in step with the maps, and no
/// way to accumulate a report without also recording that one arrived.
#[derive(Clone, Default)]
pub struct Feed(std::sync::Arc<std::sync::Mutex<Option<Sample>>>);

impl Feed {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one report from the watcher. Called on the reader thread.
    ///
    /// An **empty** sample still counts as having been heard: that is the watcher saying "I am here
    /// and the machine was idle", which is exactly the message that must not be confused with
    /// silence.
    pub fn submit(&self, sample: Sample) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let pending = state.get_or_insert_with(Sample::default);

        // Capped, because this is what accumulates *between* drains. `clamp` runs when the enforcer
        // drains, thirty seconds apart; everything arriving in between lands here first, at whatever
        // rate the watcher writes and with whatever names it chooses. Bounding only the stored day
        // would leave the buffer in front of it, inside the service, with no ceiling at all.
        accrue_capped(&mut pending.apps, sample.apps, MAX_APPS);
        accrue_capped(&mut pending.pages, sample.pages, MAX_PAGES);
    }

    /// Take everything reported since the last drain, or `None` if no watcher has reported at all.
    pub fn drain(&self) -> Option<Sample> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(pairs: &[(&str, u64)]) -> Sample {
        Sample {
            apps: pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect(),
            pages: BTreeMap::new(),
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
        let got = clamp(sample(&[("roblox.exe", 20), ("chrome.exe", 10)]), 30).apps;
        assert_eq!(got.get("roblox.exe"), Some(&20));
        assert_eq!(got.get("chrome.exe"), Some(&10));
    }

    #[test]
    fn one_app_claiming_more_than_the_tick_is_clamped() {
        let got = clamp(sample(&[("homework.exe", 9_000)]), 30).apps;
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
            pages: BTreeMap::new(),
        };

        let got = clamp(s, 30).apps;
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
        let got = clamp(
            sample(&[("a.exe", 100), ("b.exe", 100), ("c.exe", 100)]),
            10,
        )
        .apps;
        let total: u64 = got.values().sum();
        assert!(total <= 10, "must not exceed the tick, got {total}");
        for (name, secs) in &got {
            assert!(*secs <= 4, "{name} should floor to 3, got {secs}");
        }
    }

    #[test]
    fn a_zero_second_entry_is_dropped() {
        let got = clamp(sample(&[("idle.exe", 0), ("roblox.exe", 5)]), 30).apps;
        assert!(
            !got.contains_key("idle.exe"),
            "a zero carries no information and would let a forged report pad the map"
        );
        assert_eq!(got.get("roblox.exe"), Some(&5));
    }

    #[test]
    fn an_active_user_is_credited_right_up_to_now() {
        assert_eq!(idle_state(100_000, 0, 180_000), (false, 100_000));
        assert_eq!(
            idle_state(100_000, 179_999, 180_000),
            (false, 100_000),
            "one millisecond short of the threshold is still present"
        );
    }

    /// The moment the threshold is crossed, the whole grace period has just elapsed — so everything
    /// up to now was within it and everything up to now is credited.
    #[test]
    fn crossing_the_threshold_credits_the_entire_grace_period() {
        assert_eq!(idle_state(200_000, 180_000, 180_000), (true, 200_000));
    }

    /// The back-dating that makes this exact rather than approximate. Two minutes past the
    /// threshold means presence ended two minutes ago, not now — a flag flipped at detection time
    /// would credit those two minutes to whatever was on screen.
    #[test]
    fn a_long_absence_is_credited_only_to_where_presence_ended() {
        // Away for 300s with a 180s grace: credited until 120s ago.
        assert_eq!(idle_state(500_000, 300_000, 180_000), (true, 380_000));
    }

    /// Defence in depth: `GetLastInputInfo` is documented as "not guaranteed to be incremental",
    /// and its tick counter wraps. A reported idle span longer than the process has been running
    /// must clamp rather than wrap into an enormous future timestamp.
    #[test]
    fn an_absurd_idle_reading_clamps_instead_of_wrapping() {
        let (idle, at) = idle_state(1_000, u64::MAX, 180_000);
        assert!(idle);
        assert!(
            at <= 1_000 + 180_000,
            "a wrapped reading must not credit into the future, got {at}"
        );
    }

    /// A tick that took no time can charge no time — and must not divide by zero doing it.
    #[test]
    fn a_zero_length_tick_charges_nothing() {
        let got = clamp(sample(&[("roblox.exe", 30)]), 0).apps;
        assert!(got.is_empty(), "no elapsed time means nothing to charge");
    }

    #[test]
    fn accrual_adds_to_what_is_already_there() {
        let mut running: BTreeMap<String, u64> = BTreeMap::new();
        accrue(&mut running, clamp(sample(&[("roblox.exe", 20)]), 30).apps);
        accrue(
            &mut running,
            clamp(sample(&[("roblox.exe", 10), ("chrome.exe", 5)]), 30).apps,
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
        accrue(&mut running, clamp(sample(&[("roblox.exe", 30)]), 30).apps);
        assert_eq!(running.get("roblox.exe"), Some(&u64::MAX));
    }

    #[test]
    fn a_chrome_window_yields_its_page_title() {
        let got = browser_page("chrome.exe", "Roblox - Google Chrome")
            .expect("Chrome must be recognised");
        assert_eq!(got, "Roblox");
    }

    /// Firefox separates with an em dash, not a hyphen. Matching only `" - "` silently misses
    /// every Firefox window, which would look like "he never used Firefox" rather than a bug.
    #[test]
    fn firefox_uses_an_em_dash() {
        let got = browser_page("firefox.exe", "Wikipedia — Mozilla Firefox")
            .expect("Firefox must be recognised");
        assert_eq!(got, "Wikipedia");
    }

    /// Edge appends a tab count when several are open; it is chrome, not page title.
    #[test]
    fn edge_drops_its_and_n_more_pages_suffix() {
        let got = browser_page("msedge.exe", "Roblox and 3 more pages - Microsoft Edge")
            .expect("Edge must be recognised");
        assert_eq!(got, "Roblox");
    }

    #[test]
    fn a_non_browser_window_is_not_a_page() {
        assert_eq!(browser_page("notepad.exe", "Untitled - Notepad"), None);
        assert_eq!(
            browser_page("chrome.exe", "Roblox"),
            None,
            "the game itself is not a page"
        );
        assert_eq!(browser_page("chrome.exe", ""), None);
    }

    #[test]
    fn one_app_held_for_the_whole_interval_earns_all_of_it() {
        let mut t = Tracker::new();
        t.focus(Some("roblox.exe"), 0);
        assert_eq!(t.drain(30_000).get("roblox.exe"), Some(&30));
    }

    #[test]
    fn time_is_split_at_the_moment_focus_changes() {
        let mut t = Tracker::new();
        t.focus(Some("roblox.exe"), 0);
        t.focus(Some("chrome.exe"), 10_000);
        let s = t.drain(30_000);

        assert_eq!(s.get("roblox.exe"), Some(&10));
        assert_eq!(s.get("chrome.exe"), Some(&20));
    }

    /// An app in front of an empty chair is not screen time. Charging it is how a tracker reports
    /// an eight-hour session for a PC nobody touched.
    #[test]
    fn idle_time_is_not_charged_to_the_focused_app() {
        let mut t = Tracker::new();
        t.focus(Some("roblox.exe"), 0);
        t.set_idle(true, 5_000);
        let s = t.drain(30_000);

        assert_eq!(
            s.get("roblox.exe"),
            Some(&5),
            "only the 5s before going idle counts"
        );
    }

    #[test]
    fn coming_back_from_idle_resumes_the_focused_app() {
        let mut t = Tracker::new();
        t.focus(Some("roblox.exe"), 0);
        t.set_idle(true, 5_000);
        t.set_idle(false, 20_000);
        let s = t.drain(30_000);

        assert_eq!(
            s.get("roblox.exe"),
            Some(&15),
            "5s before idle plus 10s after returning"
        );
    }

    /// `GetForegroundWindow` returns nothing during UAC, at the lock screen, and briefly after a
    /// window closes. Those seconds belong to no app.
    #[test]
    fn no_foreground_window_charges_nothing() {
        let mut t = Tracker::new();
        t.focus(Some("roblox.exe"), 0);
        t.focus(None, 10_000);
        let s = t.drain(30_000);

        assert_eq!(s.get("roblox.exe"), Some(&10));
        assert_eq!(s.len(), 1, "the other 20s belong to nobody");
    }

    /// The accuracy property. Flooring on every drain would lose up to a second per app per
    /// interval; at 30s that is minutes a day, always under-reporting.
    #[test]
    fn sub_second_remainders_survive_across_drains() {
        let mut t = Tracker::new();
        t.focus(Some("roblox.exe"), 0);

        // Three drains at 1.5s each. Flooring each time gives 1+1+1=3; carrying gives 1+2+1=4.
        let mut total = 0;
        for i in 1..=3 {
            total += t.drain(1_500 * i).get("roblox.exe").copied().unwrap_or(0);
        }

        assert_eq!(total, 4, "4.5s of focus must report as 4s, not 3s");
    }

    /// The hole `MAX_PAGES` did not previously close.
    ///
    /// `clamp` bounds each report and the enforcer re-caps the stored day, but the watcher's own
    /// tracker had no cap — and its prune only drops keys whose carry lands exactly on a whole
    /// second, so a key that never accrues a full second is kept forever. Every distinct page
    /// title focused all session stayed resident in the process whose memory footprint is the
    /// entire point of the constraint.
    #[test]
    fn a_capped_tracker_does_not_grow_without_bound() {
        let mut t = Tracker::capped(3);

        // 500 distinct titles, each focused briefly — a retitling loop, or just a long evening.
        for i in 0..500u64 {
            t.focus(Some(&format!("page {i}")), i * 100);
        }
        t.drain(50_000);

        assert!(
            t.tracked_keys() <= 3,
            "capped tracker kept {} keys",
            t.tracked_keys()
        );
    }

    /// The app tracker is deliberately uncapped: its keys are installed programs, a closed set, and
    /// silently dropping one would lose real measured time.
    #[test]
    fn an_uncapped_tracker_keeps_every_key() {
        let mut t = Tracker::new();
        // 1,500ms each, so every key carries a 500ms remainder and stays. (An exact multiple of a
        // second would be dropped by the prune, correctly — it carried nothing forward.)
        for i in 0..100u64 {
            t.focus(Some(&format!("app{i}.exe")), i * 1_500);
        }
        t.drain(150_000);
        assert!(
            t.tracked_keys() > 3,
            "uncapped must keep every key that carries a remainder, got {}",
            t.tracked_keys()
        );
    }

    #[test]
    fn a_drain_does_not_stop_the_clock_on_the_focused_app() {
        let mut t = Tracker::new();
        t.focus(Some("roblox.exe"), 0);
        t.drain(10_000);
        let s = t.drain(20_000);

        assert_eq!(
            s.get("roblox.exe"),
            Some(&10),
            "focus continues across a reporting boundary"
        );
    }

    /// Defence in depth behind `clock.rs`: a timestamp that goes backwards must bank nothing
    /// rather than underflow into an enormous duration.
    #[test]
    fn a_backwards_clock_banks_nothing() {
        let mut t = Tracker::new();
        t.focus(Some("roblox.exe"), 10_000);
        let s = t.drain(5_000);
        assert!(s.is_empty(), "time cannot run backwards into credit");
    }

    /// Silence is not zero. A day with no watcher must record as *not measured*, or a child who
    /// kills the helper renders exactly like a child who did not touch the PC.
    #[test]
    fn a_feed_nobody_reported_to_drains_to_nothing_at_all() {
        let feed = Feed::new();
        assert!(
            feed.drain().is_none(),
            "no watcher must be unknown, never a measured zero"
        );
    }

    /// The other half of that distinction: the watcher saying "I am here and nothing happened" is
    /// a fact, and must not be mistaken for silence.
    #[test]
    fn an_empty_report_still_counts_as_having_been_heard() {
        let feed = Feed::new();
        feed.submit(Sample::default());

        let drained = feed.drain().expect("an empty report is still a report");
        assert!(drained.apps.is_empty());
    }

    #[test]
    fn reports_accumulate_between_drains_and_reset_after() {
        let feed = Feed::new();
        feed.submit(sample(&[("roblox.exe", 10)]));
        feed.submit(sample(&[("roblox.exe", 5), ("chrome.exe", 3)]));

        let first = feed.drain().expect("two reports were submitted");
        assert_eq!(first.apps.get("roblox.exe"), Some(&15));
        assert_eq!(first.apps.get("chrome.exe"), Some(&3));

        assert!(
            feed.drain().is_none(),
            "a drained feed has heard nothing since; the next tick must not re-count"
        );
    }

    /// Page titles are the one unbounded thing here: every tab, every video, every renamed
    /// document is a new key, and they arrive from a process running as the child. A script that
    /// retitles a window in a loop must not be able to grow the stored tally without limit.
    #[test]
    fn page_titles_are_capped_so_a_retitling_loop_cannot_grow_the_tally() {
        let flood: BTreeMap<String, u64> = (0..500).map(|i| (format!("page {i}"), 1)).collect();
        let got = clamp(
            Sample {
                apps: BTreeMap::new(),
                pages: flood,
            },
            600,
        );

        assert!(
            got.pages.len() <= MAX_PAGES,
            "kept {} titles, cap is {MAX_PAGES}",
            got.pages.len()
        );
    }

    #[test]
    fn the_cap_keeps_the_heaviest_entries_not_an_arbitrary_slice() {
        let mut map: BTreeMap<String, u64> =
            [("zzz small".to_string(), 1), ("aaa big".to_string(), 99)].into();
        retain_top(&mut map, 1);

        assert_eq!(map.len(), 1);
        assert!(
            map.contains_key("aaa big"),
            "the heaviest entry must survive, not whichever sorts first"
        );
    }

    /// Pages get the same sum bound as apps, and for the same reason: one window at a time.
    #[test]
    fn page_time_is_bounded_by_the_tick_like_app_time() {
        let got = clamp(
            Sample {
                apps: BTreeMap::new(),
                pages: [("Roblox".to_string(), 9_000)].into(),
            },
            30,
        );
        assert_eq!(got.pages.get("Roblox"), Some(&30));
    }

    /// A browser must carry **its own** suffix, not merely some browser's.
    ///
    /// The two facts were previously checked against separate lists, which could disagree in both
    /// directions: an executable admitted with no suffix to match (a guaranteed-empty title read),
    /// or a title suffix accepted from the wrong browser entirely. One table settles both.
    #[test]
    fn a_browser_is_matched_against_its_own_title_suffix() {
        assert_eq!(
            browser_page("chrome.exe", "Roblox - Google Chrome"),
            Some("Roblox")
        );
        assert_eq!(
            browser_page("chrome.exe", "Roblox — Mozilla Firefox"),
            None,
            "Chrome must not be credited through Firefox's suffix"
        );
    }

    /// Every executable the process gate admits must have a suffix to match, or reading its title
    /// is guaranteed waste — and its user silently gets no page attribution at all.
    #[test]
    fn every_admitted_browser_can_actually_be_parsed() {
        // Walks the table itself rather than a copy of it, so an entry added with a suffix that
        // does not round-trip fails here instead of on a child's PC.
        for (exe, suffixes) in BROWSERS {
            assert!(is_browser(exe), "{exe} must pass the process gate");
            assert!(!suffixes.is_empty(), "{exe} admitted with nothing to match");

            for suffix in *suffixes {
                let title = format!("Some Page{suffix}");
                assert_eq!(
                    browser_page(exe, &title),
                    Some("Some Page"),
                    "{exe} must parse its own suffix {suffix:?}"
                );
            }
        }

        // The inverse: nothing is admitted that cannot be parsed. An executable with no known
        // title format would cost a title read on every wake and yield nothing, and its user would
        // see no page attribution without being told why.
        assert!(!is_browser("opera.exe"));
        assert!(!is_browser("notepad.exe"));
    }

    /// Title suffixes are attacker-chosen, so they cannot be the only evidence a window is a
    /// browser. Any process can set its window title to `"Roblox - Google Chrome"`; a child
    /// scripting that could inject pages they never visited, or flood the capped list to push real
    /// entries out of it. The executable has to agree.
    #[test]
    fn only_a_real_browser_process_can_contribute_a_page() {
        assert!(is_browser("chrome.exe"));
        assert!(is_browser("msedge.exe"));
        assert!(is_browser("firefox.exe"));

        assert!(!is_browser("notepad.exe"), "a title alone must not qualify");
        assert!(
            !is_browser("roblox - google chrome.exe"),
            "naming the executable after a browser must not qualify either"
        );
        assert!(!is_browser(""));
    }

    /// Matching is on the already-normalised name, so casing and stray spacing cannot slip past.
    #[test]
    fn browser_matching_uses_the_normalised_name() {
        assert!(is_browser(&crate::rules::norm("  CHROME.EXE  ")));
    }

    /// A page whose own title ends in a browser name must not be mistaken for chrome.
    #[test]
    fn only_the_trailing_suffix_counts() {
        let got = browser_page(
            "chrome.exe",
            "How to uninstall Google Chrome - Google Chrome",
        )
        .expect("still a Chrome window");
        assert_eq!(
            got, "How to uninstall Google Chrome",
            "only the final suffix is the browser's"
        );
    }

    /// The bound was being applied at the wrong end of the pipe.
    ///
    /// `clamp` runs when the enforcer *drains*, once every 30 seconds. Until then every report is
    /// added straight into this map, inside the SYSTEM service, at whatever rate the watcher cares
    /// to write. A forged watcher naming a new executable per line therefore grew the service's
    /// memory for a full tick with nothing in the way — the bound that made the stored day safe did
    /// not protect the buffer in front of it.
    #[test]
    fn a_flooded_feed_cannot_grow_the_service_without_bound() {
        let feed = Feed::new();
        for i in 0..10_000u64 {
            feed.submit(sample(&[(&format!("app{i}.exe"), 1)]));
        }

        let drained = feed.drain().expect("reports were submitted");
        assert!(
            drained.apps.len() <= MAX_APPS,
            "feed accumulated {} app keys, cap is {MAX_APPS}",
            drained.apps.len()
        );
    }

    /// Same hole on the higher-cardinality map. `MAX_PAGES` bounded the stored day and each report,
    /// but not what accumulates between drains.
    #[test]
    fn a_flooded_feed_caps_page_titles_too() {
        let feed = Feed::new();
        for i in 0..10_000u64 {
            feed.submit(Sample {
                apps: BTreeMap::new(),
                pages: [(format!("page {i}"), 1)].into(),
            });
        }

        let drained = feed.drain().expect("reports were submitted");
        assert!(
            drained.pages.len() <= MAX_PAGES,
            "feed accumulated {} page keys, cap is {MAX_PAGES}",
            drained.pages.len()
        );
    }

    /// Flooding must cost the *flood*, not the real measurements it arrives beside.
    #[test]
    fn a_flood_does_not_evict_the_app_that_actually_earned_time() {
        let feed = Feed::new();
        feed.submit(sample(&[("roblox.exe", 1_800)]));
        for i in 0..10_000u64 {
            feed.submit(sample(&[(&format!("app{i}.exe"), 1)]));
        }

        let drained = feed.drain().expect("reports were submitted");
        assert_eq!(
            drained.apps.get("roblox.exe"),
            Some(&1_800),
            "the heaviest entry must survive a flood, or the flood becomes the attack"
        );
    }

    /// A line with no newline in it is a write that never ends, and `BufRead::lines` will buffer it
    /// until the service runs out of memory. Reading is where that has to be stopped: by the time
    /// anything can inspect the line, the allocation has already happened.
    #[test]
    fn an_endless_line_is_discarded_instead_of_buffered() {
        let flood = "x".repeat(4096);
        let mut input = std::io::Cursor::new(format!("{flood}\n{{\"apps\":{{}}}}\n"));

        let mut buf = Vec::new();
        assert!(
            read_bounded_line(&mut input, &mut buf, 64).expect("reading must not fail"),
            "an over-long line must not end the stream"
        );
        assert!(buf.is_empty(), "the over-long line must be discarded whole");

        assert!(read_bounded_line(&mut input, &mut buf, 64).expect("reading must not fail"));
        assert_eq!(
            std::str::from_utf8(&buf).expect("valid utf-8"),
            r#"{"apps":{}}"#,
            "the line after a discarded one must still arrive"
        );
    }

    /// `MAX_LINE` has to clear the largest line an **honest** watcher can produce.
    ///
    /// This is the failure the read limit could cause rather than prevent: set it too low and real
    /// samples are discarded as if forged, which shows up as a child who used nothing. The three
    /// numbers that decide it live in three places and are edited independently — `MAX_PAGES` here,
    /// the 512-unit title buffer and 260-unit process-name buffer in `watcher.rs`, and the 30-second
    /// emit cadence — so raising any of them without raising `MAX_LINE` breaks this quietly. That is
    /// the same two-things-that-must-agree shape as the browser table above, which had already
    /// drifted once.
    ///
    /// Built at the true worst case for JSON size: every character a control character, which
    /// serde escapes to six bytes. Real titles are far cheaper — even CJK is three.
    #[test]
    fn the_read_limit_clears_the_largest_honest_line() {
        // At most one window holds focus, so a 30-second emit can drain at most 30 whole-second
        // app entries. Titles and process names are bounded by watcher.rs's fixed buffers.
        let worst_title = "\u{1}".repeat(512);
        let worst_exe = "\u{1}".repeat(260);

        let sample = Sample {
            apps: (0..30).map(|i| (format!("{worst_exe}{i}"), 1u64)).collect(),
            pages: (0..MAX_PAGES)
                .map(|i| (format!("{worst_title}{i}"), 1u64))
                .collect(),
        };

        let line = serde_json::to_string(&sample).expect("a sample must serialise");
        assert!(
            (line.len() as u64) < MAX_LINE,
            "the worst honest line is {} bytes but MAX_LINE is {MAX_LINE}; real samples would be \
             discarded as forged, and the symptom is a child who appears to have used nothing",
            line.len()
        );
    }

    /// Resynchronising must not overshoot into the next line.
    ///
    /// A skipped line whose newline lands exactly on the read limit reads as "the chunk filled the
    /// limit", which is indistinguishable from "the line is still going" if you ask the byte count
    /// instead of the buffer. Getting that wrong swallows the line *after* the over-long one — so
    /// one forged line would cost two, and the second would be a real sample.
    ///
    /// 15 bytes plus a newline, read 8 at a time: the second chunk is exactly full and exactly
    /// finished.
    #[test]
    fn skipping_an_over_long_line_stops_at_its_newline_not_past_it() {
        let mut input = std::io::Cursor::new(format!("{}\nkeep me\n", "x".repeat(15)));

        let mut buf = Vec::new();
        assert!(read_bounded_line(&mut input, &mut buf, 8).expect("reading must not fail"));
        assert!(buf.is_empty(), "the over-long line is discarded");

        assert!(read_bounded_line(&mut input, &mut buf, 8).expect("reading must not fail"));
        assert_eq!(
            std::str::from_utf8(&buf).expect("valid utf-8"),
            "keep me",
            "the line after a discarded one must survive, not be eaten by the skip"
        );
    }

    #[test]
    fn a_normal_line_round_trips_without_its_newline() {
        let mut input = std::io::Cursor::new("{\"apps\":{}}\nsecond\n");

        let mut buf = Vec::new();
        assert!(read_bounded_line(&mut input, &mut buf, MAX_LINE).expect("reading must not fail"));
        assert_eq!(
            std::str::from_utf8(&buf).expect("valid utf-8"),
            r#"{"apps":{}}"#
        );

        assert!(read_bounded_line(&mut input, &mut buf, MAX_LINE).expect("reading must not fail"));
        assert_eq!(std::str::from_utf8(&buf).expect("valid utf-8"), "second");

        assert!(
            !read_bounded_line(&mut input, &mut buf, MAX_LINE).expect("reading must not fail"),
            "a drained pipe reports EOF"
        );
    }

    /// The pipe can be cut mid-write by the session ending. A tail with no newline is a torn write,
    /// not a sample — `emit` always terminates its lines — so it is dropped rather than parsed.
    #[test]
    fn a_torn_final_line_is_dropped_rather_than_parsed() {
        let mut input = std::io::Cursor::new("{\"apps\":{}}\n{\"apps\":{\"roblo");

        let mut buf = Vec::new();
        assert!(read_bounded_line(&mut input, &mut buf, MAX_LINE).expect("reading must not fail"));
        assert_eq!(
            std::str::from_utf8(&buf).expect("valid utf-8"),
            r#"{"apps":{}}"#
        );

        assert!(
            !read_bounded_line(&mut input, &mut buf, MAX_LINE).expect("reading must not fail"),
            "a half-written trailing line is EOF, not a record"
        );
    }
}
