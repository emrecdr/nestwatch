//! Words on an app group: what a parent typed, and what it matches.
//!
//! A group's words widen it from a list of exact executables to "anything called this". A program
//! whose name contains a word, or a browser page whose title or address does, draws from the
//! group's pool. The design, and why each rule below exists, is `docs/WORDS-IN-GROUPS.md`.
//!
//! Everything here is pure. The two inputs that are not the parent's own words come from very
//! different places, and the rules treat them differently for that reason:
//!
//! * **Process names and owners** come from Windows, read by the service. Trusted as far as they
//!   go, but a word matches far more than an exact name does, so a word only ever reaches the
//!   signed-in user's own processes — see [`match_groups`].
//! * **Page visits** come from the helper, which runs as the child. Every key is the child's to
//!   choose, so a visit can only add time to a pool and close the child's own processes of one of
//!   the four known browsers.

use std::collections::{BTreeMap, BTreeSet};

use crate::rules::{MAX_RULE_ENTRIES, MAX_RULE_NAME, norm};

/// Shortest word accepted, in characters.
///
/// Two would match far too much: `gg` is in `now.gg` and in every page that mentions eggs, and a
/// word here can close programs.
pub const MIN_WORD_CHARS: usize = 3;

/// Longest word stored as typed, in characters: [`MAX_RULE_NAME`] for the word itself, plus room
/// for a `%` at each end, which is not part of it. Bounds what is stored, where [`normalise`]
/// bounds what is matched — a word kept as typed would otherwise admit any amount of padding.
pub const MAX_WORD_TYPED: usize = MAX_RULE_NAME + 2;

/// Characters of a page title kept in a visit key **that carries an address**, which gets the rest
/// of [`VISIT_KEY_CHARS`], so a long title cannot crowd it out entirely. Without an address — every
/// visit until addresses are read — the title may use the whole key.
pub const VISIT_TITLE_CHARS: usize = 250;

/// Longest visit key the helper builds, in characters.
///
/// The longest key the service keeps whole ([`crate::foreground::MAX_KEY_CHARS`]), so an honest key
/// is never shortened by the service's own cut — the address it carries arrives intact.
pub const VISIT_KEY_CHARS: usize = crate::foreground::MAX_KEY_CHARS;

/// Nestwatch's own executables, which no word may match.
///
/// The helper runs under the child's account, so the owner rule does not protect it: `host` or
/// `health` would otherwise close it and stop page counting along with it. Both names, because the
/// installed binary is renamed and a development run is not.
const OWN_PROGRAMS: [&str; 2] = [crate::install::INSTALL_EXE_NAME, "nestwatch.exe"];

/// Windows' own programs, which no word may match.
///
/// The owner rule keeps a word away from everything Windows runs under its own accounts, which is
/// where the processes whose closing stops Windows live. It does not cover the few Windows programs
/// that run under the **signed-in user's** account: the shell, the Start menu, text input, per-user
/// services. A broad word such as `host` reaches those, and `svchost.exe` always runs — so the pool
/// would drain all day with nothing being played, and from then on the child's session would be
/// closed piece by piece every thirty seconds.
///
/// **Not complete, and not claimed to be.** It lists what a parent's word is plausibly going to
/// hit; the dashboard's preview shows anything else a word matches before it closes it. The
/// critical system processes are listed as well, although the owner rule already keeps words away
/// from them, so that a mistaken owner lookup still cannot reach them.
const WINDOWS_PROGRAMS: [&str; 30] = [
    "applicationframehost.exe",
    "backgroundtaskhost.exe",
    "conhost.exe",
    "csrss.exe",
    "ctfmon.exe",
    "dllhost.exe",
    "dwm.exe",
    "explorer.exe",
    "fontdrvhost.exe",
    "gamebar.exe",
    "gamebarftserver.exe",
    "lockapp.exe",
    "lsass.exe",
    "msedgewebview2.exe",
    "rundll32.exe",
    "runtimebroker.exe",
    "searchapp.exe",
    "searchhost.exe",
    "securityhealthsystray.exe",
    "services.exe",
    "shellexperiencehost.exe",
    "sihost.exe",
    "smartscreen.exe",
    "smss.exe",
    "startmenuexperiencehost.exe",
    "svchost.exe",
    "taskhostw.exe",
    "textinputhost.exe",
    "wininit.exe",
    "winlogon.exe",
];

/// Why a word was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordProblem {
    /// Under [`MIN_WORD_CHARS`] once the `%` at the ends is removed.
    TooShort,
    /// Over [`MAX_RULE_NAME`], the bound every other name in a rule has.
    TooLong,
    /// A space, a control character or a `%` inside the word.
    Unsupported,
}

impl WordProblem {
    /// The message a parent sees for `word`. Says which rule it broke, so they can fix it.
    ///
    /// Shows at most [`MAX_RULE_NAME`] characters of the word: it names the word, and a word of any
    /// length must not make an error of any length.
    pub fn message(self, word: &str) -> String {
        let shown: String = word.chars().take(MAX_RULE_NAME).collect();
        let word = if shown.len() < word.len() {
            format!("{shown}…")
        } else {
            shown
        };
        match self {
            Self::TooShort => {
                format!("the word \"{word}\" is too short: use at least {MIN_WORD_CHARS} letters")
            }
            Self::TooLong => {
                format!("the word \"{word}\" is too long: use at most {MAX_RULE_NAME} characters")
            }
            Self::Unsupported => {
                format!("the word \"{word}\" can't contain spaces or % (a % at either end is fine)")
            }
        }
    }
}

/// Fold text for matching: lowercased, with Turkish dotted and dotless `i` both made plain `i`.
///
/// `to_lowercase` alone turns `İ` into `i` plus a combining dot and leaves `ı` alone, so in a
/// language the dashboard ships in, `izle` would never match `İZLE`. Words, page text and program
/// names all go through here, so the three cannot disagree about what "ignoring case" means.
fn fold(text: &str) -> String {
    text.to_lowercase().replace('\u{307}', "").replace('ı', "i")
}

/// Normalise one word as typed: `Ok(None)` for a blank entry, which is dropped rather than refused,
/// `Ok(Some(word))` folded, with any `%` at the ends and a trailing `.exe` removed, or the rule it
/// breaks.
///
/// `.exe` goes because programs are matched on their name without it: `roblox.exe`, copied from the
/// apps field, would otherwise match nothing at all.
pub fn normalise(raw: &str) -> Result<Option<String>, WordProblem> {
    // What is stored is the word as typed, so the bound on it lives here with the others: checked
    // anywhere else, a hand-edited config would reach the enforcer with padding `validate` refuses.
    if raw.chars().count() > MAX_WORD_TYPED {
        return Err(WordProblem::TooLong);
    }
    let word = raw.trim().trim_matches('%').trim();
    if word.is_empty() {
        return Ok(None);
    }
    if word
        .chars()
        .any(|c| c == '%' || c.is_whitespace() || c.is_control())
    {
        return Err(WordProblem::Unsupported);
    }
    // The upper bound on what the parent typed: folding can lengthen a word (`İ` is two characters
    // before the dot is dropped). The lower bound on what will be matched.
    if word.chars().count() > MAX_RULE_NAME {
        return Err(WordProblem::TooLong);
    }
    let folded = fold(word);
    let word = folded.strip_suffix(".exe").unwrap_or(&folded);
    if word.chars().count() < MIN_WORD_CHARS {
        return Err(WordProblem::TooShort);
    }
    Ok(Some(word.to_string()))
}

/// The usable words of a list, normalised, each once. Blank and refused entries are dropped here as
/// well as at validation, because a hand-edited `config.json` reaches the enforcer without passing
/// [`crate::rules::Rules::validate`].
pub fn usable(raw: &[String]) -> Vec<String> {
    raw.iter()
        .filter_map(|w| normalise(w).ok().flatten())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Whether a word as typed is one [`usable`] keeps — the one definition behind "does this group have
/// a word", without building the list.
pub fn is_usable(raw: &str) -> bool {
    matches!(normalise(raw), Ok(Some(_)))
}

/// One group's words as a save or a preview receives them: at most [`MAX_RULE_ENTRIES`], and none
/// that [`normalise`] refuses, with the message naming the rule it broke. Shared by
/// [`crate::rules::Rules::validate`] and `POST /api/rules/words-preview`, so the preview is never a
/// wider door than the rule it previews.
pub fn check(words: &[String]) -> Result<(), String> {
    if words.len() > MAX_RULE_ENTRIES {
        return Err(format!("at most {MAX_RULE_ENTRIES} words per group"));
    }
    for word in words {
        if let Err(problem) = normalise(word) {
            return Err(problem.message(word.trim()));
        }
    }
    Ok(())
}

/// Whether a program called `name` matches any of `words`, owner aside — the name half of the rule,
/// for the tests that pin it. Matched on the name **without `.exe`**, or the word `exe` would match
/// every program; never Nestwatch's own programs, nor Windows' own (`WINDOWS_PROGRAMS`). Nothing
/// ships through this: [`match_groups`] adds the owner rule, and is the only way in.
#[cfg(test)]
fn program_matches(words: &[String], name: &str) -> bool {
    program_stem(name, &BTreeSet::new()).is_some_and(|stem| stem_matches(words, &stem))
}

/// The part of a program's name a word is matched against — folded, without `.exe` — or `None` for
/// a program no word may match at all: Nestwatch's, Windows', or one of the `spared` check programs.
fn program_stem(name: &str, spared: &BTreeSet<String>) -> Option<String> {
    let key = norm(name);
    if OWN_PROGRAMS.contains(&key.as_str())
        || WINDOWS_PROGRAMS.contains(&key.as_str())
        || spared.contains(&key)
    {
        return None;
    }
    let folded = fold(name.trim());
    Some(folded.strip_suffix(".exe").unwrap_or(&folded).to_string())
}

fn stem_matches(words: &[String], stem: &str) -> bool {
    words.iter().any(|w| stem.contains(w.as_str()))
}

/// Build the helper's key for one foreground browser visit: `exe\ntitle\naddress`.
///
/// The whole key is cut to [`VISIT_KEY_CHARS`], and the title to [`VISIT_TITLE_CHARS`] when there is
/// an address to make room for. The browser is carried because the service has to know which
/// program to close.
pub fn visit_key(exe: &str, title: &str, address: &str) -> String {
    let cap = if address.is_empty() {
        // The browser and two separators are all the key needs besides the title.
        VISIT_KEY_CHARS.saturating_sub(exe.chars().count() + 2)
    } else {
        VISIT_TITLE_CHARS
    };
    let title: String = title.chars().take(cap).collect();
    format!("{exe}\n{title}\n{address}")
        .chars()
        .take(VISIT_KEY_CHARS)
        .collect()
}

/// What one group's words matched during one check.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupMatch {
    /// Seconds a matching page was in front, summed over the visits.
    pub page_secs: u64,
    /// The known browsers, normalised, that showed a matching page.
    pub browsers: BTreeSet<String>,
    /// The child's own processes whose names match.
    pub programs: BTreeSet<u32>,
}

/// Match every group's words against one check's visits and the child's processes.
///
/// `groups` yields each group's [`usable`] words, and the result has one entry per group **in the
/// same order** — by position rather than by name, because nothing stops two groups sharing a name,
/// and a map keyed by it let one group's matches overwrite another's.
///
/// `child` is every process running under the signed-in user's account, ID to name, from a single
/// snapshot, or `None` when that is unknown — and unknown matches no program at all, because the
/// alternative is a word reaching every process on the machine. Owner and name come from the same
/// snapshot so that "this ID is the child's" and "this ID is called Roblox" describe one process.
///
/// `spared` holds the normalised file names of the check programs configured on the Integrations
/// card. They run under the child's account too, and closing one mid-check would leave shut a gate
/// that should have opened, so no word matches them — the same reasoning as `OWN_PROGRAMS`.
pub fn match_groups<'a>(
    groups: impl IntoIterator<Item = &'a [String]>,
    web: &BTreeMap<String, u64>,
    child: Option<&BTreeMap<u32, String>>,
    spared: &BTreeSet<String>,
) -> Vec<GroupMatch> {
    let groups: Vec<&[String]> = groups.into_iter().collect();
    // Nothing to match on most installs, which have no words: skip folding every visit and name.
    if groups.iter().all(|words| words.is_empty()) {
        return vec![GroupMatch::default(); groups.len()];
    }
    // The child's programs, once rather than once per group: their stems, with every program no
    // word may match — Nestwatch's, Windows', a configured check — already gone.
    let programs: Vec<(u32, String)> = child
        .into_iter()
        .flatten()
        .filter_map(|(&pid, name)| Some((pid, program_stem(name, spared)?)))
        .collect();
    // Each visit likewise once: the browser, normalised and checked against the known four, and
    // the folded text after it. A key that names anything else is dropped before any group sees
    // it — it is the child's to forge, and a forged `explorer.exe` must neither count nor become
    // something to close.
    let visits: Vec<(String, String, u64)> = web
        .iter()
        .filter_map(|(key, &secs)| {
            let (exe, text) = key.split_once('\n')?;
            let exe = norm(exe);
            crate::foreground::is_browser(&exe).then(|| (exe, fold(text), secs))
        })
        .collect();

    groups
        .into_iter()
        .map(|words| {
            let mut m = GroupMatch::default();
            for (exe, text, secs) in &visits {
                if words.iter().any(|w| text.contains(w.as_str())) {
                    m.page_secs = m.page_secs.saturating_add(*secs);
                    m.browsers.insert(exe.clone());
                }
            }
            m.programs = programs
                .iter()
                .filter(|(_, stem)| stem_matches(words, stem))
                .map(|(pid, _)| *pid)
                .collect();
            m
        })
        .collect()
}

/// The names of the child's programs that `words`, as typed, would match: what the dashboard shows
/// under a group's words before anything is closed. Each name once, as Windows reports it, sorted.
///
/// The enforcer's own rule, run on the same owner snapshot, so the preview cannot list a program
/// the rule would leave alone or miss one it would close.
pub fn preview(
    words: &[String],
    child: &BTreeMap<u32, String>,
    spared: &BTreeSet<String>,
) -> Vec<String> {
    let usable = usable(words);
    let matched = match_groups([usable.as_slice()], &BTreeMap::new(), Some(child), spared);
    let names: BTreeSet<&String> = matched
        .iter()
        .flat_map(|m| &m.programs)
        .filter_map(|pid| child.get(pid))
        .collect();
    names.into_iter().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|w| (*w).to_string()).collect()
    }

    #[test]
    fn a_percent_at_either_end_is_removed_and_case_is_ignored() {
        for raw in [
            "roblox",
            "%roblox%",
            "%roblox",
            "roblox%",
            "Roblox",
            " ROBLOX ",
            "%%roblox%%",
        ] {
            assert_eq!(normalise(raw), Ok(Some("roblox".into())), "{raw:?}");
        }
    }

    #[test]
    fn a_blank_entry_is_dropped_not_refused() {
        for raw in ["", "   ", "%", "%%", " % "] {
            assert_eq!(normalise(raw), Ok(None), "{raw:?}");
        }
    }

    /// Both sides of each bound in one test, so a `<` that drifts to `<=` fails here.
    #[test]
    fn a_word_is_three_to_sixty_four_characters() {
        assert_eq!(normalise("ab"), Err(WordProblem::TooShort));
        assert_eq!(normalise("%ab%"), Err(WordProblem::TooShort));
        assert_eq!(normalise("abc"), Ok(Some("abc".into())));
        let at = "x".repeat(MAX_RULE_NAME);
        assert_eq!(normalise(&at), Ok(Some(at.clone())));
        assert_eq!(
            normalise(&"x".repeat(MAX_RULE_NAME + 1)),
            Err(WordProblem::TooLong)
        );
        // Characters, not bytes: three two-byte letters are three characters.
        assert_eq!(normalise("ÉÉÉ"), Ok(Some("ééé".into())));
    }

    #[test]
    fn a_space_control_character_or_inner_percent_is_refused() {
        for raw in ["rob lox", "rob\tlox", "rob%lox", "\u{7}roblox", "rob\nlox"] {
            assert_eq!(normalise(raw), Err(WordProblem::Unsupported), "{raw:?}");
        }
    }

    #[test]
    fn every_problem_names_the_word_in_its_message() {
        for p in [
            WordProblem::TooShort,
            WordProblem::TooLong,
            WordProblem::Unsupported,
        ] {
            assert!(p.message("zz").contains("\"zz\""), "{p:?}");
        }
    }

    /// Turkish ships in the dashboard, and `to_lowercase` alone leaves `İ` as `i` plus a combining
    /// dot and `ı` as itself — so `izle` would never match `İZLE`, nor `oyunları` match `OYUNLARI`.
    #[test]
    fn turkish_dotted_and_dotless_i_match_either_way() {
        assert_eq!(normalise("İzle"), Ok(Some("izle".into())));
        assert_eq!(normalise("OYUNLARI"), Ok(Some("oyunlari".into())));
        assert_eq!(normalise("oyunları"), Ok(Some("oyunlari".into())));
        let visits = web(&[
            ("msedge.exe\nFİLM İZLE\n", 10),
            ("chrome.exe\noyunları\n", 5),
        ]);
        let folded = usable(&words(&["izle", "OYUNLARI"]));
        assert_eq!(
            only(match_groups(one(&folded), &visits, None, &BTreeSet::new())).page_secs,
            15
        );
        assert!(program_matches(&usable(&words(&["izle"])), "İZLE.exe"));
    }

    /// A parent copying a name from the apps field types `roblox.exe`. Matching is on the program's
    /// name without `.exe`, so a word keeping it would match nothing — the word drops it instead.
    #[test]
    fn a_trailing_exe_is_dropped_from_a_word() {
        assert_eq!(normalise("roblox.exe"), Ok(Some("roblox".into())));
        assert_eq!(
            normalise("RobloxPlayerBeta.EXE"),
            Ok(Some("robloxplayerbeta".into()))
        );
        assert_eq!(normalise("ab.exe"), Err(WordProblem::TooShort));
        assert_eq!(normalise(".exe"), Err(WordProblem::TooShort));
        assert!(program_matches(
            &usable(&words(&["roblox.exe"])),
            "RobloxPlayerBeta.exe"
        ));
    }

    /// The message names the word, but a word of any length must not make an error of any length.
    #[test]
    fn a_message_shows_at_most_a_bounded_part_of_the_word() {
        let message = WordProblem::TooLong.message(&"x".repeat(10_000));
        assert!(
            message.chars().count() < 200,
            "{} characters",
            message.chars().count()
        );
        assert!(
            message.contains(&format!("\"{}…\"", "x".repeat(MAX_RULE_NAME))),
            "a cut word must say it was cut: {message}"
        );
        let whole = WordProblem::TooShort.message("ab");
        assert!(whole.contains("\"ab\"") && !whole.contains('…'), "{whole}");
    }

    #[test]
    fn usable_keeps_valid_words_once_and_drops_the_rest() {
        let got = usable(&words(&[
            "%Roblox%", "", "ab", "roblox", "rob lox", "now.gg",
        ]));
        assert_eq!(got, words(&["now.gg", "roblox"]));
        // Padding counts against the typed bound here too, not only at `validate`: a hand-edited
        // config reaches the enforcer through this list without passing it.
        assert!(usable(&[format!("{}roblox", " ".repeat(100))]).is_empty());
    }

    #[test]
    fn a_program_matches_on_its_name_without_exe() {
        let w = words(&["roblox"]);
        assert!(program_matches(&w, "RobloxPlayerBeta.exe"));
        assert!(program_matches(&w, "robloxstudiobeta.exe"));
        assert!(program_matches(&w, "roblox"));
        assert!(!program_matches(&w, "Minecraft.exe"));
        assert!(
            !program_matches(&words(&["exe"]), "notepad.exe"),
            "`exe` must not match every program through its extension"
        );
        assert!(program_matches(&words(&["host"]), "GhostHunter.exe"));
    }

    /// Windows runs a few of its own programs under the child's account, so the owner rule does not
    /// keep a word away from them — and `svchost.exe` always runs, so `host` would drain the pool
    /// all day and then close the Start menu and notifications every check. The list is not
    /// complete and does not claim to be; the preview covers what it misses.
    #[test]
    fn windows_own_programs_are_never_matched() {
        for (word, name) in [
            ("host", "svchost.exe"),
            ("host", "sihost.exe"),
            ("host", "TaskHostW.exe"),
            ("explorer", "explorer.exe"),
            ("frame", "ApplicationFrameHost.exe"),
            ("edge", "msedgewebview2.exe"),
            ("win", "wininit.exe"),
            ("win", "winlogon.exe"),
        ] {
            assert!(
                !program_matches(&words(&[word]), name),
                "{word} reached {name}"
            );
        }
        assert!(
            program_matches(&words(&["edge"]), "msedge.exe"),
            "the browser itself is not Windows' own and stays matchable"
        );
    }

    /// A check program on the Integrations card runs under the child's account too, and closing
    /// it mid-check would leave a gate shut that should have opened.
    #[test]
    fn a_configured_check_program_is_never_matched() {
        let w = words(&["study"]);
        let snapshot = child(&[(10, "studygo-probe.exe"), (11, "StudyGoGame.exe")]);
        let spared = BTreeSet::from(["studygo-probe.exe".to_string()]);
        let m = only(match_groups(
            one(&w),
            &BTreeMap::new(),
            Some(&snapshot),
            &spared,
        ));
        assert_eq!(m.programs, BTreeSet::from([11]));
    }

    #[test]
    fn no_word_matches_nestwatchs_own_programs() {
        for (word, name) in [
            ("host", "host-health.exe"),
            ("health", "HOST-HEALTH.EXE"),
            ("nest", "nestwatch.exe"),
        ] {
            assert!(!program_matches(&words(&[word]), name), "{word} vs {name}");
        }
    }

    #[test]
    fn a_visit_key_carries_browser_title_and_address() {
        assert_eq!(
            visit_key("msedge.exe", "Roblox", "roblox.com/games"),
            "msedge.exe\nRoblox\nroblox.com/games"
        );
        assert_eq!(
            visit_key("chrome.exe", "Roblox", ""),
            "chrome.exe\nRoblox\n"
        );
    }

    #[test]
    fn a_long_title_is_cut_and_the_address_keeps_the_rest() {
        let key = visit_key("chrome.exe", &"T".repeat(600), &"a".repeat(600));
        let title = key.split('\n').nth(1).unwrap();
        let address = key.split('\n').nth(2).unwrap();
        assert_eq!(title.chars().count(), 250);
        // 512 in all: "chrome.exe\n" is 11, the title 250, its "\n" 1, so the address keeps 250.
        assert_eq!(address.chars().count(), 250);
        // A short key is left alone.
        assert_eq!(visit_key("chrome.exe", "a", "b").chars().count(), 14);
    }

    /// With no address — every visit until addresses are read — the title may use the whole key,
    /// so a word late in a long title still matches.
    #[test]
    fn without_an_address_the_title_keeps_the_whole_key() {
        let title = format!("{}Roblox", "t".repeat(300));
        let key = visit_key("chrome.exe", &title, "");
        assert!(key.ends_with("Roblox\n"), "{key}");
        let long = visit_key("chrome.exe", &"T".repeat(600), "");
        assert_eq!(long.chars().count(), crate::foreground::MAX_KEY_CHARS);
        assert_eq!(long.split('\n').nth(1).unwrap().chars().count(), 500);
    }

    #[test]
    fn the_preview_lists_what_the_rule_would_match_each_name_once() {
        let snapshot = child(&[
            (10, "RobloxPlayerBeta.exe"),
            (11, "RobloxPlayerBeta.exe"),
            (12, "RobloxCrashHandler.exe"),
            (13, "svchost.exe"),
            (14, "host-health.exe"),
            (15, "studygo-probe.exe"),
            (16, "notepad.exe"),
        ]);
        let spared = BTreeSet::from(["studygo-probe.exe".to_string()]);
        assert_eq!(
            preview(&words(&["%Roblox%", "host", "study"]), &snapshot, &spared),
            ["RobloxCrashHandler.exe", "RobloxPlayerBeta.exe"],
            "Windows', Nestwatch's and the check program's names must not be listed"
        );
        assert!(preview(&words(&["", "ab"]), &snapshot, &spared).is_empty());
        assert!(preview(&words(&["zzz"]), &snapshot, &spared).is_empty());
    }

    /// The property the key length exists for, stated against the service rather than against the
    /// constant: the longest key the helper can build passes the service's own cut unchanged.
    #[test]
    fn the_longest_visit_key_survives_the_services_cut_whole() {
        let key = visit_key("chrome.exe", &"T".repeat(600), &"a".repeat(600));
        let sample = crate::foreground::Sample {
            web: [(key.clone(), 10)].into(),
            ..Default::default()
        };
        let kept = crate::foreground::clamp(sample, 30).web;
        assert!(
            kept.contains_key(&key),
            "the service shortened an honest key: {kept:?}"
        );
    }

    fn one(words: &[String]) -> [&[String]; 1] {
        [words]
    }

    fn web(entries: &[(&str, u64)]) -> BTreeMap<String, u64> {
        entries
            .iter()
            .map(|(k, s)| ((*k).to_string(), *s))
            .collect()
    }

    /// The child's processes, ID to name, as one owner snapshot reports them.
    fn child(entries: &[(u32, &str)]) -> BTreeMap<u32, String> {
        entries
            .iter()
            .map(|(p, n)| (*p, (*n).to_string()))
            .collect()
    }

    fn only(got: Vec<GroupMatch>) -> GroupMatch {
        assert_eq!(got.len(), 1, "one entry per group");
        got.into_iter().next().unwrap()
    }

    #[test]
    fn page_seconds_sum_over_matching_visits_by_title_or_address() {
        let w = words(&["roblox"]);
        let visits = web(&[
            ("msedge.exe\nRoblox\nroblox.com/games", 10),
            (
                "chrome.exe\nPlay games | now.gg\nnow.gg/apps/roblox-corporation/5349",
                7,
            ),
            ("chrome.exe\nHomework\ndocs.example.com", 13),
        ]);
        let m = only(match_groups(one(&w), &visits, None, &BTreeSet::new()));
        assert_eq!(m.page_secs, 17);
        assert_eq!(
            m.browsers,
            BTreeSet::from(["chrome.exe".to_string(), "msedge.exe".to_string()])
        );
    }

    #[test]
    fn the_browser_part_of_a_visit_is_never_matched() {
        let w = words(&["chrome"]);
        let visits = web(&[("chrome.exe\nHomework\ndocs.example.com", 30)]);
        assert_eq!(
            only(match_groups(one(&w), &visits, None, &BTreeSet::new())),
            GroupMatch::default(),
            "the word `chrome` matched a Chrome page through `chrome.exe`"
        );
    }

    #[test]
    fn a_visit_naming_anything_but_a_known_browser_is_dropped() {
        let w = words(&["roblox"]);
        for key in [
            "explorer.exe\nRoblox\nroblox.com",
            "opera.exe\nRoblox\nroblox.com",
            "no-separator-roblox",
        ] {
            let visits = web(&[(key, 30)]);
            assert_eq!(
                only(match_groups(one(&w), &visits, None, &BTreeSet::new())),
                GroupMatch::default(),
                "{key:?} counted or named a browser to close"
            );
        }
    }

    #[test]
    fn the_browser_named_in_a_visit_is_normalised() {
        let w = words(&["roblox"]);
        let visits = web(&[("  MSEdge.EXE\nRoblox\n", 5)]);
        let m = only(match_groups(one(&w), &visits, None, &BTreeSet::new()));
        assert_eq!(m.browsers, BTreeSet::from(["msedge.exe".into()]));
    }

    #[test]
    fn page_seconds_saturate_rather_than_wrap() {
        let w = words(&["roblox"]);
        let visits = web(&[
            ("msedge.exe\nRoblox\n", u64::MAX),
            ("chrome.exe\nRoblox\n", 5),
        ]);
        assert_eq!(
            only(match_groups(one(&w), &visits, None, &BTreeSet::new())).page_secs,
            u64::MAX
        );
    }

    /// Only processes in the child's snapshot can match, and never Nestwatch's own. A process under
    /// another account is simply absent from that snapshot, which is the whole of the owner rule.
    #[test]
    fn only_the_childs_own_matching_programs_are_matched() {
        let w = words(&["roblox", "host"]);
        let snapshot = child(&[
            (10, "RobloxPlayerBeta.exe"),
            (12, "GhostHunter.exe"),
            (13, "host-health.exe"),
            (14, "notepad.exe"),
        ]);
        let m = only(match_groups(
            one(&w),
            &BTreeMap::new(),
            Some(&snapshot),
            &BTreeSet::new(),
        ));
        assert_eq!(m.programs, BTreeSet::from([10, 12]));
    }

    #[test]
    fn an_unknown_owner_matches_no_program() {
        let w = words(&["roblox"]);
        assert_eq!(
            only(match_groups(
                one(&w),
                &BTreeMap::new(),
                None,
                &BTreeSet::new()
            ))
            .programs,
            BTreeSet::new()
        );
    }

    #[test]
    fn a_group_without_words_matches_nothing() {
        let none: Vec<String> = Vec::new();
        let visits = web(&[("msedge.exe\nRoblox\n", 30)]);
        let snapshot = child(&[(10, "RobloxPlayerBeta.exe")]);
        assert_eq!(
            only(match_groups(
                one(&none),
                &visits,
                Some(&snapshot),
                &BTreeSet::new()
            )),
            GroupMatch::default()
        );
    }

    /// One entry per group, by position: two groups with the same words — or the same name, which
    /// this function never sees — each get their own entry.
    #[test]
    fn each_group_is_matched_against_its_own_words_in_order() {
        let games = words(&["roblox"]);
        let video = words(&["youtube"]);
        let visits = web(&[
            ("msedge.exe\nRoblox\n", 10),
            ("msedge.exe\nCats - YouTube\nyoutube.com/watch", 20),
        ]);
        let got = match_groups(
            [games.as_slice(), video.as_slice(), games.as_slice()],
            &visits,
            None,
            &BTreeSet::new(),
        );
        let secs: Vec<u64> = got.iter().map(|m| m.page_secs).collect();
        assert_eq!(secs, [10, 20, 10]);
    }
}
