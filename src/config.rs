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

/// Largest number of installed integrations we keep (bounds the config).
///
/// The same job [`MAX_ROUTINES`] does, for the same reason: `providers` is written by an
/// authenticated request and lives in the persisted config, so without a ceiling a caller with
/// the parent's session can grow `config.json` without limit — and every save rewrites the whole
/// file. Twelve is far above any real household: an integration is one homework or chores signal,
/// and [`MAX_EARNED_SOURCES`] already refuses to let more than sixteen distinct sources grant
/// on one day, so a registry larger than that could not all be used anyway.
///
/// Paired with the delete route by necessity, not by taste. A cap with no way to remove an entry
/// is a trap: the twelfth install would be permanent, and turning an integration off would be the
/// only thing a parent could still do to it.
///
/// **Enforced in one place, unlike [`MAX_ROUTINES`], and that asymmetry is deliberate.** Routines
/// are checked again in [`Policy::validate`] because they ride the policy export/restore, so an
/// import is a second way to write them. Integrations do not: [`Policy`] carries `curfew`,
/// `rules`, `routines` and `language` and nothing else, so `api::set_provider` is the *whole*
/// write path and a second check would guard a door that is not there. Should `providers` ever
/// join `Policy`, this cap has to be restated there — that is the moment the asymmetry becomes a
/// hole.
pub const MAX_PROVIDERS: usize = 12;

/// Largest number of reward tiers one provider may carry (bounds the config).
///
/// The same job [`MAX_PROVIDERS`] does. Four rather than two because the shape a household
/// actually asks for is "most of it" and "all of it", and a third step between them is a
/// reasonable thing to want; far beyond that the tiers stop being a rule a child can hold in
/// their head, which is the real limit and not one a constant can enforce.
pub const MAX_TIERS: usize = 4;

/// Largest question count a reward tier may ask for.
///
/// Not a capacity limit — it bounds *dead configuration*. A tier asking for more questions than a
/// child could answer in a day can never be met, so it is the same defect as a tier asking for
/// nothing, arriving from the other end. Both are refused where they are written rather than left
/// to behave like a rule that silently never fires.
pub const MAX_TIER_QUESTIONS: u32 = 1000;

/// Largest practised-minutes threshold a reward tier may ask for: one day.
///
/// The same argument as [`MAX_TIER_QUESTIONS`], and here the ceiling is arithmetic rather than
/// judgement — a threshold above 1440 asks for more minutes than the day contains.
pub const MAX_TIER_MINUTES: u32 = 24 * 60;

/// Fewest minutes between two runs of one provider's probe.
///
/// A floor rather than a free choice, because the request lands on a third party's API under the
/// child's own account. StudyGo's risk register (in the Voortgang repository) rates a
/// terms-of-service objection as "account action" and relies on minimal request volume as the
/// mitigation. The gate's design polls every fifteen minutes and stops for the day once the bar is
/// met — about a dozen requests a day; five minutes is the least a parent can ask for here.
pub const MIN_PROBE_MINS: u32 = 5;
/// Most minutes between two runs. Bounded like every other minutes field so the dashboard's box
/// has a limit `web.rs` can hold it to. Four hours is already a probe that fires once an
/// afternoon; past that the field becomes a way of switching a probe off that reads as leaving it
/// on.
pub const MAX_PROBE_MINS: u32 = 240;
/// Longest probe file name accepted. Well past any real name, and the value is written into the
/// audit log and shown on the dashboard, so it must not be able to be a paragraph.
pub const MAX_PROBE_NAME: usize = 64;
/// Most distinct non-`parent` sources that may grant on one day. Bounds [`Config::earned`], which
/// lives in the persisted config: a compromised parent session must not be able to grow that file
/// without limit.
pub const MAX_EARNED_SOURCES: usize = 16;

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
/// signal earns. By default it carries no endpoint, no credential, and no
/// code — the gathering happens off this machine (on the parent's phone) and
/// arrives as an authenticated push, which is what keeps the monitored PC
/// from ever dialing out. See `docs/PLUGIN-SYSTEM.md` for why this shape and
/// not a loaded module.
///
/// The one exception is opt-in and named: a [`Probe`]. A parent who sets one
/// asks this machine to run a program *as the child* on a timer and judge
/// what it prints, which is the half of a gate a phone cannot do. It changes
/// nothing about how the answer is judged — a probe's two numbers go through
/// [`Config::earn`] exactly as a push's do — and it is absent from every
/// config that has not asked for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provider {
    /// Whether this provider may currently grant. A disabled provider's push
    /// is refused, so turning an integration off is one switch, not a
    /// re-pairing.
    pub enabled: bool,
    /// Minutes one met-threshold push is worth. The parent's policy, applied
    /// on this machine rather than trusted from the client.
    pub minutes: u32,
    /// Optional ceiling on the minutes this provider may grant across one
    /// local day, however many times it pushes.
    ///
    /// `None` is the default, is how every config written before this field
    /// loads, and keeps the original rule exactly: **one grant per source per
    /// day**, worth [`Provider::minutes`]. Set it and the same provider may
    /// push repeatedly until the ceiling is reached — which is what a signal
    /// arriving in pieces through the day needs, and what a single latch
    /// cannot express.
    ///
    /// **The ceiling replaces the latch as the bound on a compromised
    /// client, and is strictly the better one.** A latch bounds a bad push to
    /// "one reward"; a ceiling bounds it to a number the parent chose. Both
    /// refuse to trust the push itself, which is the property
    /// `docs/PLUGIN-SYSTEM.md` argues for.
    ///
    /// Skipped when absent rather than written as `null`, so an install that
    /// never opts in keeps a byte-identical `config.json` and a byte-identical
    /// `GET /api/providers` — the guarantee that this whole field is designed
    /// around.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_cap_mins: Option<u32>,
    /// Reward tiers, evaluated against what a push reports about the work
    /// behind it.
    ///
    /// Empty — the default, and how every existing config loads — means this
    /// provider has exactly one reward, [`Provider::minutes`], and a client's
    /// decision to push at all is the assertion that it was earned. That is
    /// the original contract and it is unchanged.
    ///
    /// Non-empty moves the judgement to this machine: the push says what the
    /// child *did*, and which reward that is worth is decided here, from the
    /// parent's configuration. It is the same move `83f0ce3` made for the
    /// reward amount, applied to the threshold that earns it — and it is what
    /// lets a parent change the bar without touching the client that reports
    /// against it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<Tier>,
    /// A program to run as the child, on a timer, to ask what he has done
    /// today. See [`Probe`]. Absent — the default, and every config written
    /// before it existed — means this provider is push-only, exactly as it
    /// always was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<Probe>,
}

/// One step of a provider's reward ladder: what the child has to have done, and what it earns.
///
/// A threshold of `0` states no condition rather than a trivially satisfied one — see
/// [`Tier::met`], where the difference is the whole of the type's behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tier {
    /// Questions answered, or `0` for "this tier does not ask about questions".
    #[serde(default)]
    pub questions: u32,
    /// Minutes *practised* — not minutes rewarded — or `0` for "does not ask".
    #[serde(default)]
    pub minutes_practised: u32,
    /// What meeting this tier is worth, in minutes of screen time.
    pub reward_mins: u32,
}

impl Tier {
    /// Whether reported work meets this tier.
    ///
    /// **Either condition suffices**, which is the rule as households state it: *fifteen
    /// questions or half an hour*. A child who works slowly and carefully reaches it on minutes;
    /// one who works quickly reaches it on questions. Requiring both would punish each of them
    /// for the way they work, which is the same objection that keeps accuracy out of this
    /// calculation entirely.
    ///
    /// **A zero threshold is not a condition**, so a tier that sets neither can never be met. The
    /// alternative reading — `questions >= 0`, trivially true — would make an unconfigured tier
    /// pay out on an empty day, which is the most expensive possible meaning for a field someone
    /// left blank.
    pub fn met(&self, questions: u32, minutes_practised: u32) -> bool {
        (self.questions > 0 && questions >= self.questions)
            || (self.minutes_practised > 0 && minutes_practised >= self.minutes_practised)
    }
}

/// A program this machine runs, as the child, to ask a provider what the child has done today.
///
/// The half of a gate the phone cannot do. Voortgang signs in to StudyGo and forwards the session
/// (`POST /api/providers/{name}/secret`), but a phone in a pocket cannot ask every fifteen
/// minutes; the PC can, and this names what it runs. The program reads the deposited secret on
/// stdin, prints one JSON object — `{"questions": 12, "minutes": 24}` — and exits. This machine
/// judges those numbers against the provider's [`Provider::tiers`] exactly as it judges a push,
/// so a probe is never trusted to decide anything. See `docs/PLUGIN-SYSTEM.md`, *A probe, and the
/// machine's first outbound request*.
///
/// **A file name, never a path.** It is resolved inside the program directory, which the child
/// can execute from and cannot write to (`install::harden_program_dir`). A path would let a
/// parent point at a file on the desktop, which the child could replace with one that prints
/// whatever he likes. The ceiling would still bound the damage, but the property that the probe
/// *cannot be swapped* is worth more than the flexibility, and a parent who wants a different
/// probe copies it into that directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    /// The file to run, inside the program directory.
    pub exe: String,
    /// Minutes between runs while the child is signed in, within
    /// [`MIN_PROBE_MINS`]..=[`MAX_PROBE_MINS`].
    pub every_mins: u32,
}

impl Probe {
    /// Refuse anything that is not a bare, bounded file name with a bounded interval.
    ///
    /// The charset is deliberately narrow: letters, digits, `.`, `_` and `-`, not starting with a
    /// dot. No separator of either platform, no drive letter, no whitespace — the name is joined
    /// to a directory and must not be able to leave it, and it is written verbatim into the
    /// audit log, where nothing may fake a line break or a quote.
    pub fn validate(&self) -> Result<(), String> {
        let name = &self.exe;
        let bare = !name.is_empty()
            && name.len() <= MAX_PROBE_NAME
            && !name.starts_with('.')
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if !bare {
            return Err(format!(
                "probe must be a bare file name of 1-{MAX_PROBE_NAME} letters, digits, '.', '_' \
                 or '-', not starting with '.'"
            ));
        }
        if !(MIN_PROBE_MINS..=MAX_PROBE_MINS).contains(&self.every_mins) {
            return Err(format!(
                "probe interval must be {MIN_PROBE_MINS}-{MAX_PROBE_MINS} minutes"
            ));
        }
        Ok(())
    }
}

impl Provider {
    /// What one push is worth, given whatever it reported about the work behind it.
    ///
    /// `None` means *nothing was earned* and is distinct from `Some(0)`, which cannot arise:
    /// `require_minutes` refuses a zero reward at the point a tier is configured. A caller must
    /// therefore treat `None` as a refusal to grant, not as a grant of nothing.
    ///
    /// **Both fallbacks land on the original behaviour, and that is deliberate.** A provider with
    /// no tiers is worth [`Provider::minutes`] however much detail a push carries, and a push that
    /// reports nothing is worth [`Provider::minutes`] however many tiers are configured. So this
    /// only ever changes the answer where a parent has configured a ladder *and* the client has
    /// said enough to place the child on it; every other combination is what shipped before.
    ///
    /// The best matching tier wins rather than the first, so the answer does not depend on the
    /// order a parent happened to enter them in.
    pub fn reward_for(&self, progress: Option<Progress>) -> Option<u32> {
        match (progress, self.tiers.as_slice()) {
            (_, []) | (None, _) => Some(self.minutes),
            (Some(done), tiers) => tiers
                .iter()
                .filter(|tier| tier.met(done.questions, done.minutes))
                .map(|tier| tier.reward_mins)
                .max(),
        }
    }

    /// The rung to aim at next: the cheapest tier this work has not yet met.
    ///
    /// What the child is told to reach, so it is the *nearest* thing rather than the most
    /// impressive — naming the top of a ladder to someone standing at the bottom is the
    /// discouraging choice, and the research this feature was designed against is explicit that a
    /// controlling frame produces more screen time rather than less. Chosen by reward rather than
    /// by position, so the sentence does not depend on the order a parent happened to type the
    /// tiers in, which is the same property [`Provider::reward_for`] holds on the paying side.
    ///
    /// `None` when every tier is met — there is nothing left to earn, so there is nothing to say —
    /// and when there are no tiers at all, which is a provider whose single reward has no bar in
    /// front of it.
    pub fn next_rung(&self, done: Progress) -> Option<&Tier> {
        self.tiers
            .iter()
            .filter(|tier| !tier.met(done.questions, done.minutes))
            .min_by_key(|tier| tier.reward_mins)
    }

    /// Whether another grant today could pay anything — the question the probe scheduler asks
    /// before spending a request, so a source paid in full stops being polled for the day.
    ///
    /// Reads the same entry [`Config::earn`] writes, under the same rules: with no ceiling the
    /// first grant latches the day; with one, an untracked entry counts as the ceiling reached
    /// (see [`EarnedDay::minutes`]) and a tracked one is compared against it. An entry from
    /// another day says nothing about today.
    pub fn exhausted_for(&self, today: NaiveDate, earned: Option<&EarnedDay>) -> bool {
        let Some(entry) = earned.filter(|e| e.date == today) else {
            return false;
        };
        match (self.daily_cap_mins, entry.minutes) {
            (None, _) | (Some(_), None) => true,
            (Some(cap), Some(used)) => used >= cap,
        }
    }
}

/// What a child has actually done today, as reported by whatever is watching.
///
/// **One named type, because the two fields are the same shape and swapping them compiles.**
/// `reward_for`, `next_rung` and `earn` all took a bare `(u32, u32)`, and `api` and `probe` each
/// declared their own identical struct to feed them — so three call sites could transpose questions
/// and minutes silently, and a field added in one copy would be missing from the other. The push
/// from the phone and the probe's two numbers are the same fact arriving by different roads, so they
/// are the same type.
///
/// `Deserialize` lives here because both roads parse it: the HTTP body in `api::ExtraTimeBody` and
/// the probe's stdout in `probe::parse_output`.
///
/// **Both fields are required, and that is the probe's contract rather than a preference.** The
/// earlier HTTP-only copy of this struct defaulted each field so a client could send one of the
/// two; merging the two types carried that leniency onto the probe, where it meant a program
/// printing `{}` — or `[]`, which serde reads as a struct of defaults from a sequence — was read as
/// *nothing practised today* instead of being refused as not an answer. Silently crediting a broken
/// probe with a real zero is the failure this module is most careful about elsewhere. Nothing sent
/// a partial `progress`, so strictness costs nothing and
/// `the_answer_is_two_integers_and_nothing_else_is_believed` is what says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Progress {
    /// Questions answered today.
    pub questions: u32,
    /// Minutes *practised* today — deliberately not a reward. The two are one transposition apart,
    /// which is the whole argument for this being a struct rather than a pair.
    pub minutes: u32,
}

/// Why a provider grant was refused.
///
/// **A type rather than three string constants, so a fourth reason cannot be silently unspoken.**
/// `probe.rs` has to recognise one of these to decide whether the child hears anything, and while
/// the reason was a `&'static str` that match ended in a catch-all: a new refusal would have fallen
/// through it, the child would never have been told, and nothing at any layer would have asked the
/// author to decide whether they should be. Now adding a variant fails to compile at every site
/// that has to answer that question.
///
/// [`Refused::wire`] carries the values, which are **a cross-repository contract** — they reach
/// Voortgang as `{ok: false, reason}` and it branches on them, so the strings must not change. They
/// used to live as four scattered literals; one function and one test now hold them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Work was reported and met no tier. The only one worth telling the child about: it is the one
    /// with a rung still to aim at.
    BelowThreshold,
    /// This source already granted today and has no ceiling to top up against.
    AlreadyGrantedToday,
    /// A ceiling is configured and today's allowance is spent.
    DailyCapReached,
}

impl Refused {
    /// Every variant, so a test can walk them and a new one cannot be added past it.
    pub const ALL: [Refused; 3] = [
        Refused::BelowThreshold,
        Refused::AlreadyGrantedToday,
        Refused::DailyCapReached,
    ];

    /// The value that reaches the wire. Pinned by a test, because another repository reads it.
    pub fn wire(self) -> &'static str {
        match self {
            Refused::BelowThreshold => "below_threshold",
            Refused::AlreadyGrantedToday => "already_granted_today",
            Refused::DailyCapReached => "daily_cap_reached",
        }
    }
}

/// How a provider grant came out. See [`Config::earn`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Earn {
    /// Minutes were added to today's budget — the reward, or what was left of the ceiling.
    Granted(u32),
    /// An ordinary outcome that moved nothing. On the wire this is `200 {ok: false, reason}`, with
    /// the reason from [`Refused::wire`] — a cross-repo contract, since Voortgang branches on the
    /// flag and shows the reason.
    Refused(Refused),
}

/// What one grant source has already been given on one local day.
///
/// Replaces the bare `NaiveDate` [`Config::earned`] used to hold. The date
/// alone answered the only question the original rule asked — *has this
/// source granted today?* — and a ceiling has to ask a second one: *how
/// much?*
///
/// **Both spellings load, and the old one is still what gets written unless a
/// ceiling is configured.** See [`EarnedDay::minutes`]; the serde impls below
/// are hand-written for exactly that reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EarnedDay {
    /// The local day these minutes belong to. An entry for any other day is
    /// stale, and the grant handler prunes it before inserting.
    pub date: NaiveDate,
    /// Minutes granted to this source on [`EarnedDay::date`], or `None` when
    /// the amount is not being tracked.
    ///
    /// `None` arises two ways and means the same thing in both: an entry
    /// written by a build older than this field, or an entry written by this
    /// build for a provider with no ceiling configured. Either way the rule
    /// that applies is the original one — *this source has granted today, and
    /// that is the end of it*.
    ///
    /// **`None` is read as "the ceiling is already reached", never as zero.**
    /// A parent who adds a ceiling halfway through a day would otherwise hand
    /// that source a second full grant, because an untracked earlier grant
    /// would look like no grant at all — which is precisely the farming the
    /// latch exists to prevent.
    pub minutes: Option<u32>,
}

impl Serialize for EarnedDay {
    /// Writes the bare date when no amount is tracked, and only then the
    /// richer object.
    ///
    /// This is what keeps the promise on [`Provider::daily_cap_mins`]: a
    /// household that never sets a ceiling never sees its `config.json`
    /// change shape, and its file stays readable by an older build. The
    /// object form appears the first time a ceiling actually governs a grant.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.minutes {
            None => self.date.serialize(serializer),
            Some(minutes) => {
                use serde::ser::SerializeStruct;
                let mut entry = serializer.serialize_struct("EarnedDay", 2)?;
                entry.serialize_field("date", &self.date)?;
                entry.serialize_field("minutes", &minutes)?;
                entry.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for EarnedDay {
    /// Accepts both spellings.
    ///
    /// `untagged` is the sanctioned tool here rather than a shortcut: its
    /// documented hazard is two variants sharing a shape, where serde silently
    /// takes the first that parses. These two cannot collide — one is a JSON
    /// string and the other a JSON object — and `serde_reads_both_spellings`
    /// pins that rather than leaving it to this comment.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            /// How every build before the ceiling wrote it.
            Legacy(NaiveDate),
            Tracked {
                date: NaiveDate,
                #[serde(default)]
                minutes: Option<u32>,
            },
        }
        Ok(match Repr::deserialize(deserializer)? {
            Repr::Legacy(date) => Self {
                date,
                minutes: None,
            },
            Repr::Tracked { date, minutes } => Self { date, minutes },
        })
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
    /// What each non-`parent` grant source has been given today — judged
    /// against *this* machine's trusted clock, never a day the pushing device
    /// computed, for the same reason [`crate::clock`] exists.
    ///
    /// Without a [`Provider::daily_cap_mins`] this is the original rule
    /// unchanged: an earned bonus lands **once per source per day**. With one,
    /// the entry accumulates and the source may push again until the ceiling
    /// is reached. [`EarnedDay`] carries both cases.
    ///
    /// Self-pruning: the grant handler drops entries for other days before
    /// inserting, so the map never outgrows one day's sources. `parent` is
    /// deliberately absent — a human pressing the button twice means it twice.
    #[serde(default)]
    pub earned: std::collections::BTreeMap<String, EarnedDay>,
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
    /// Settings written by a build **newer** than this one, kept verbatim so an older binary
    /// cannot delete them.
    ///
    /// The tests above cover old-file/new-code, and every field here carries `#[serde(default)]`
    /// so that direction is safe. The other direction is the one that loses data: `load` ignores
    /// fields it has no name for and `save` writes only the fields it knows, so an older binary
    /// run once — after a rollback, off a USB stick, from an old install directory — rewrites the
    /// file without every setting added since it was built. Silently, and not recovered by
    /// upgrading again.
    ///
    /// [`crate::api::set_policy`] reasoned this exact hazard through for the *export* document and
    /// answered it with a warning naming both versions. It could do that because the export
    /// carries a version; `config.json` never has, so on the file that is actually rewritten on
    /// every settings change there was nothing to compare and nothing to say.
    ///
    /// **Nothing in this crate reads this.** It is not a place to put anything; it exists so that
    /// load → save is lossless, and it disappears on its own once a build has a real field for
    /// whatever it is holding.
    #[serde(flatten)]
    pub unknown: std::collections::BTreeMap<String, serde_json::Value>,
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
    Tr,
}

impl Language {
    /// Every variant, for tests that must cover all of them.
    ///
    /// Exists because the alternative is each test hand-writing `[Language::En, Language::Nl]`,
    /// which keeps passing when a third language is added and quietly stops testing it — the
    /// tautological-fixture trap `tests/spawn_paths.rs` was written to close. Adding a variant
    /// without extending this list fails `all_lists_every_language_variant` below, which counts
    /// the variants in this file's own source rather than trusting the list.
    pub const ALL: [Language; 3] = [Language::En, Language::Nl, Language::Tr];

    /// The BCP-47 tag, for `<html lang>` and for the client's string table.
    pub fn tag(self) -> &'static str {
        match self {
            Language::En => "en",
            Language::Nl => "nl",
            Language::Tr => "tr",
        }
    }

    /// Parse a tag from the API. `None` for anything this build has no strings for — the caller
    /// rejects it rather than quietly serving English.
    pub fn from_tag(tag: &str) -> Option<Self> {
        match tag {
            "en" => Some(Language::En),
            "nl" => Some(Language::Nl),
            "tr" => Some(Language::Tr),
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
    /// The provider `source` names, if it may currently grant — otherwise why it may not.
    ///
    /// **One place decides, because two callers ask and they must not drift.** `require_auth`
    /// asks before letting an integration-scoped session reach either of its routes;
    /// `api::extra_time` asks again inside the write guard, which is the race-free one and also
    /// the *only* check for a `Scope::Dashboard` caller naming a `source` in its body. Both are
    /// load-bearing and neither can go — so what is shared here is the decision and its wording,
    /// not the call.
    ///
    /// **The two messages are a cross-repo contract**, which is the sharper reason to keep them
    /// in one place. Voortgang routes on the status and shows the parent a remedy; "turned off"
    /// and "not installed" both mean *fix it on the PC*, and a reworded copy that drifted from
    /// its twin would send half the callers to the wrong sentence. `tests/provider_lifecycle.rs`
    /// asserts both routes answer identically, and this is what makes that cheap to keep true.
    pub fn provider_authority(&self, source: &str) -> Result<&Provider, String> {
        match self.providers.get(source) {
            Some(provider) if provider.enabled => Ok(provider),
            Some(_) => Err(format!("the '{source}' integration is turned off")),
            None => Err(format!("no '{source}' integration is installed")),
        }
    }

    /// Grant earned time from `source` for what it reported about today, or say why not.
    ///
    /// **The one place a provider grant is decided.** `api::extra_time` calls it for a push from
    /// the phone and `probe::run_once` for a probe this machine ran itself. The two arrive by
    /// different roads — one authenticated over the LAN, one launched as the child and read back
    /// over a pipe — and neither is believed about anything but the two numbers it reports: the
    /// provider must exist and be on, the reward is the registry's, the day latch or the ceiling
    /// is applied, and only then does the budget move.
    ///
    /// `Err` rejects the request itself — no such provider, turned off, too many sources today.
    /// `Ok(Refused)` is an ordinary answer that changed nothing. `Ok(Granted)` has already moved
    /// [`Config::extra`] and written [`Config::earned`]. A caller holding the config write guard
    /// through a persist sees the three as one atomic step, which is what stops two grants from
    /// the same source racing past the latch — the reason the callers run this inside
    /// `try_update_config` rather than before it.
    pub fn earn(
        &mut self,
        source: &str,
        today: NaiveDate,
        reported: Option<Progress>,
    ) -> Result<Earn, String> {
        // A provider grant is governed by the registry: it must name an enabled provider, and
        // the reward is that provider's — read here, never taken from the push.
        let (mut minutes, ceiling) = {
            let provider = self.provider_authority(source)?;
            match provider.reward_for(reported) {
                Some(reward) => (reward, provider.daily_cap_mins),
                // Work was reported and it meets no tier. Not an error: a client that pushes
                // whatever it sees and lets this machine judge is exactly what tiers are for, so
                // "not yet" has to be an ordinary answer rather than a rejection. Unreachable for
                // a provider with no tiers configured.
                None => return Ok(Earn::Refused(Refused::BelowThreshold)),
            }
        };
        // Read out as an owned value, not held as a borrow: the map is mutated a few lines
        // below, and `Option<Option<u32>>` says the two things that matter separately — whether
        // this source has an entry for today at all, and whether that entry's amount was ever
        // measured.
        let spent = self
            .earned
            .get(source)
            .filter(|entry| entry.date == today)
            .map(|entry| entry.minutes);
        match (ceiling, spent) {
            // The original rule, and the one every config that has not opted in takes: one
            // grant per source per day, whatever it was worth.
            (None, Some(_)) => return Ok(Earn::Refused(Refused::AlreadyGrantedToday)),
            // A ceiling is configured, but this source's earlier grant today was never measured —
            // an older build wrote it, or the ceiling was added after it landed. Refusing is the
            // conservative reading; `EarnedDay::minutes` gives the argument for why the
            // alternative hands out a second full reward.
            (Some(_), Some(None)) => return Ok(Earn::Refused(Refused::DailyCapReached)),
            (Some(cap), Some(Some(used))) => {
                let room = cap.saturating_sub(used);
                if room == 0 {
                    return Ok(Earn::Refused(Refused::DailyCapReached));
                }
                // The last grant of a day is worth the remainder, not the full reward.
                //
                // **Deliberately not the rejection an over-quota API call would get**, and the
                // difference is that a provider never asks for an amount: there is no request to
                // half-fulfil. The client asserts a threshold was met and this machine answers
                // what that is worth today, which near the ceiling is the remainder. Rejecting
                // instead would take the work and pay nothing for it — the failure this whole
                // feature exists to avoid.
                //
                // The ceiling still binds exactly: `used + minutes <= cap` by construction here
                // and in the arm below, so `tracked` can never pass `cap` however many times a
                // client pushes.
                minutes = minutes.min(room);
            }
            // A ceiling below the single-grant reward still binds on the first push.
            (Some(cap), None) => minutes = minutes.min(cap),
            (None, None) => {}
        }
        // Only a **new** source can hit the ceiling on distinct sources. The original rule made a
        // second push from a counted source unreachable, so the check never had to exclude one;
        // a ceiling makes it ordinary, and without this clause a household at the limit would
        // start refusing exactly the sources it had already admitted.
        if spent.is_none()
            && self.earned.values().filter(|e| e.date == today).count() >= MAX_EARNED_SOURCES
        {
            return Err("too many earned-time sources today".into());
        }
        self.earned.retain(|_, entry| entry.date == today);
        // Measured only where a ceiling governs it. Writing `Some` unconditionally would change
        // the shape of a config that never opted in, which is the single thing `EarnedDay`'s
        // hand-written serde impls exist to prevent.
        let tracked = ceiling.map(|_| spent.flatten().unwrap_or(0) + minutes);
        self.earned.insert(
            source.to_string(),
            EarnedDay {
                date: today,
                minutes: tracked,
            },
        );
        self.extra.add(today, minutes);
        Ok(Earn::Granted(minutes))
    }

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

    /// Helper: what a child did, for the call sites that judge it. Named fields, so a test cannot
    /// transpose questions and minutes the way a bare pair let every caller do.
    fn done(questions: u32, minutes: u32) -> Progress {
        Progress { questions, minutes }
    }

    /// Helper: a provider with the two-step ladder a household actually asks for.
    fn laddered() -> Provider {
        Provider {
            enabled: true,
            minutes: 30,
            daily_cap_mins: Some(30),
            tiers: vec![
                Tier {
                    questions: 10,
                    minutes_practised: 20,
                    reward_mins: 16,
                },
                Tier {
                    questions: 15,
                    minutes_practised: 30,
                    reward_mins: 30,
                },
            ],
            probe: None,
        }
    }

    /// Either condition carries a tier, and neither is required.
    #[test]
    fn a_tier_takes_either_condition() {
        let tier = Tier {
            questions: 15,
            minutes_practised: 30,
            reward_mins: 30,
        };
        assert!(tier.met(15, 0), "questions alone are enough");
        assert!(tier.met(0, 30), "minutes alone are enough");
        assert!(tier.met(20, 45), "both is obviously enough");
        assert!(!tier.met(14, 29), "just short on both is short");
    }

    /// A blank threshold states no condition, rather than one that is trivially true.
    ///
    /// The expensive misreading: `questions >= 0` holds on an empty day, so a tier a parent left
    /// half-filled would pay out for doing nothing. Pinned because the correct behaviour here is
    /// one comparison away from the worst possible one.
    #[test]
    fn a_zero_threshold_is_not_a_condition() {
        let questions_only = Tier {
            questions: 10,
            minutes_practised: 0,
            reward_mins: 16,
        };
        assert!(!questions_only.met(0, 999), "minutes must not carry it");
        assert!(questions_only.met(10, 0));

        let blank = Tier {
            questions: 0,
            minutes_practised: 0,
            reward_mins: 16,
        };
        assert!(
            !blank.met(999, 999),
            "a tier asking for nothing can never be met"
        );
    }

    /// The best matching tier decides, so the answer does not depend on entry order.
    #[test]
    fn the_best_matching_tier_wins_not_the_first() {
        let mut provider = laddered();
        assert_eq!(
            provider.reward_for(Some(done(20, 40))),
            Some(30),
            "clearing both tiers is worth the higher one"
        );
        provider.tiers.reverse();
        assert_eq!(
            provider.reward_for(Some(done(20, 40))),
            Some(30),
            "and still the higher one when they are listed the other way round"
        );
        assert_eq!(
            provider.reward_for(Some(done(12, 0))),
            Some(16),
            "clearing only the lower tier is worth the lower reward"
        );
    }

    /// Nothing earned is `None`, and a caller must not read it as a grant of zero.
    #[test]
    fn work_below_every_tier_earns_nothing() {
        assert_eq!(laddered().reward_for(Some(done(9, 19))), None);
    }

    /// Both roads back to the original behaviour.
    ///
    /// This is the compatibility guarantee for tiers, in the same shape as the ceiling's: a
    /// provider that configures no ladder, and a push that reports nothing, both land on the
    /// single reward that shipped before any of this existed.
    #[test]
    fn a_provider_without_tiers_is_worth_what_it_always_was() {
        let plain = Provider {
            enabled: true,
            minutes: 25,
            daily_cap_mins: None,
            tiers: Vec::new(),
            probe: None,
        };
        assert_eq!(
            plain.reward_for(Some(done(0, 0))),
            Some(25),
            "no tiers means the single reward, however detailed the push"
        );
        assert_eq!(plain.reward_for(None), Some(25));
        assert_eq!(
            laddered().reward_for(None),
            Some(30),
            "and a push reporting nothing is the single reward, however many tiers exist"
        );
    }

    /// Both spellings of an [`EarnedDay`] load, and mean what they should.
    ///
    /// The one property `#[serde(untagged)]` cannot be trusted on by inspection: its documented
    /// failure is two variants sharing a shape, where the first that parses silently wins. These
    /// two are a JSON string and a JSON object, so they cannot collide — but "cannot" is the kind
    /// of claim this project has been bitten by, so it is measured here instead of asserted in a
    /// comment.
    #[test]
    fn serde_reads_both_spellings() {
        let legacy: std::collections::BTreeMap<String, EarnedDay> =
            serde_json::from_str(r#"{"studygo":"2026-09-08"}"#).expect("a bare date must load");
        assert_eq!(
            legacy["studygo"],
            EarnedDay {
                date: NaiveDate::from_ymd_opt(2026, 9, 8).unwrap(),
                minutes: None,
            },
            "a config written before the ceiling existed must load as an untracked grant"
        );

        let tracked: std::collections::BTreeMap<String, EarnedDay> =
            serde_json::from_str(r#"{"studygo":{"date":"2026-09-08","minutes":16}}"#)
                .expect("the tracked form must load");
        assert_eq!(tracked["studygo"].minutes, Some(16));

        // A tracked entry that predates `minutes` within the object form. Not a shape this build
        // writes, but `#[serde(default)]` promises it loads, and the promise is free to keep.
        let partial: EarnedDay =
            serde_json::from_str(r#"{"date":"2026-09-08"}"#).expect("minutes must be optional");
        assert_eq!(partial.minutes, None);
    }

    /// An install that never sets a ceiling never changes the shape of its own config.
    ///
    /// This is the whole backward-compatibility guarantee in one assertion: the new field is
    /// skipped rather than written as `null`, and an untracked grant is still written as the bare
    /// date every earlier build wrote. So adding the ceiling to the codebase does not, by itself,
    /// rewrite anybody's `config.json` — or change what `GET /api/providers` answers.
    #[test]
    fn nothing_written_changes_shape_until_a_ceiling_is_set() {
        let provider = Provider {
            enabled: true,
            minutes: 30,
            daily_cap_mins: None,
            tiers: Vec::new(),
            probe: None,
        };
        assert_eq!(
            serde_json::to_string(&provider).unwrap(),
            r#"{"enabled":true,"minutes":30}"#,
            "a provider with no ceiling must serialise exactly as it did before the field existed"
        );

        let untracked = EarnedDay {
            date: NaiveDate::from_ymd_opt(2026, 9, 8).unwrap(),
            minutes: None,
        };
        assert_eq!(
            serde_json::to_string(&untracked).unwrap(),
            r#""2026-09-08""#,
            "an untracked grant must still be written as the bare date an older build can read"
        );
    }

    /// Once a ceiling governs a grant, the richer spelling appears — and survives a round trip.
    #[test]
    fn a_tracked_entry_round_trips() {
        let provider = Provider {
            enabled: true,
            minutes: 30,
            daily_cap_mins: Some(45),
            tiers: Vec::new(),
            probe: None,
        };
        let json = serde_json::to_string(&provider).unwrap();
        assert!(json.contains(r#""daily_cap_mins":45"#), "got {json}");

        let tracked = EarnedDay {
            date: NaiveDate::from_ymd_opt(2026, 9, 8).unwrap(),
            minutes: Some(16),
        };
        let back: EarnedDay = serde_json::from_str(&serde_json::to_string(&tracked).unwrap())
            .expect("the tracked form must round trip");
        assert_eq!(back, tracked);
    }

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

    /// Every language the build can *emit* is one it can also *parse* back.
    ///
    /// [`Language::tag`] and [`Language::from_tag`] are two matches over the same enum, and only
    /// one of them is exercised by anything else: the message tests walk `ALL` and call `tag`,
    /// while `from_tag` is reached only through `api::set_language`, whose tests name specific
    /// tags. So a variant added to `tag` and forgotten in `from_tag` compiles, ships, and fails
    /// exactly once — when a parent presses that button and the API answers 400 for a language
    /// the dashboard is offering them.
    ///
    /// **Measured before this was written, which is why it exists:** deleting the `"tr"` arm from
    /// `from_tag` left the whole suite green at 628 tests. `ALL` cannot close this on its own —
    /// it proves the list is complete, not that the two directions agree.
    #[test]
    fn every_language_parses_back_from_the_tag_it_emits() {
        for lang in Language::ALL {
            let tag = lang.tag();
            assert_eq!(
                Language::from_tag(tag),
                Some(lang),
                "{lang:?} emits the tag {tag:?}, which `from_tag` does not accept — the dashboard \
                 would offer this language and the API would refuse it"
            );
        }
    }

    /// A tag this build has no strings for is refused rather than quietly served as English.
    #[test]
    fn an_unknown_tag_is_refused() {
        assert_eq!(Language::from_tag("de"), None);
        assert_eq!(Language::from_tag(""), None);
        assert_eq!(
            Language::from_tag("TR"),
            None,
            "tags are lowercase on the wire"
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

    /// A config written by a **newer** build keeps its unknown settings through load → save.
    ///
    /// The direction the tests above cover is old-file/new-code. This is the other one, and it is
    /// the one that loses data: `load` ignores fields it has no name for and `save` writes only
    /// the fields it knows, so running an older binary once rewrites the file without them. There
    /// are eleven released versions, so a parent rolling back after a bad upgrade reaches this.
    #[test]
    fn a_newer_configs_unknown_settings_survive_a_load_and_save() {
        let from_the_future =
            r#"{"port":8443,"password_hash":"$argon2id$abc","bedtime_stories":{"enabled":true}}"#;
        let cfg: Config = serde_json::from_str(from_the_future).unwrap();
        let written = serde_json::to_string(&cfg).unwrap();
        assert!(
            written.contains("bedtime_stories"),
            "a setting this build has no field for was dropped on save: {written}"
        );
    }

    /// The capture map holds **only** what this build has no field for.
    ///
    /// `#[serde(flatten)]` onto a map has a documented failure mode (serde-rs/serde#2764) where it
    /// also re-captures fields the struct declares, which here would mean every known setting
    /// written twice into `config.json` — the second copy winning on the next load, forever. That
    /// is worse than the loss this field exists to prevent, so it is pinned rather than assumed.
    #[test]
    fn the_capture_map_takes_nothing_the_struct_already_names() {
        let ordinary =
            r#"{"port":8443,"password_hash":"$argon2id$abc","language":"nl","cert_sans":["a"]}"#;
        let cfg: Config = serde_json::from_str(ordinary).unwrap();
        assert!(
            cfg.unknown.is_empty(),
            "a field this build declares was swept into the capture map: {:?}",
            cfg.unknown.keys().collect::<Vec<_>>()
        );

        // …and the round-trip emits each key once, so nothing is duplicated on disk.
        let written = serde_json::to_string(&cfg).unwrap();
        for key in ["port", "password_hash", "language", "cert_sans"] {
            assert_eq!(
                written.matches(&format!("\"{key}\"")).count(),
                1,
                "{key} was written more than once: {written}"
            );
        }
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

    // ----- the probe: a bare file name, a bounded interval, and nothing written until set -----

    fn probe(exe: &str, every_mins: u32) -> Probe {
        Probe {
            exe: exe.into(),
            every_mins,
        }
    }

    /// The probe is named, never pathed: it is resolved inside a directory the child cannot
    /// write, and a path would let a parent point at one he can.
    #[test]
    fn a_probe_is_a_bare_file_name_not_a_path() {
        assert!(probe("studygo-probe.exe", 15).validate().is_ok());
        assert!(probe("probe", 15).validate().is_ok());
        for bad in [
            "",
            ".",
            "..",
            ".hidden",
            "..\\probe.exe",
            "../probe.exe",
            "bin/probe.exe",
            "bin\\probe.exe",
            "C:\\probe.exe",
            "probe .exe",
            "probe\n.exe",
        ] {
            assert!(
                probe(bad, 15).validate().is_err(),
                "{bad:?} must be refused as a probe name"
            );
        }
        let long = "p".repeat(MAX_PROBE_NAME + 1);
        assert!(probe(&long, 15).validate().is_err());
        assert!(probe(&"p".repeat(MAX_PROBE_NAME), 15).validate().is_ok());
    }

    /// The interval is bounded on both sides: a floor because the request lands on a third
    /// party's API, a ceiling because a probe that runs once a week is a rule that never fires.
    #[test]
    fn a_probe_interval_is_bounded_on_both_sides() {
        assert!(probe("p", 0).validate().is_err());
        assert!(probe("p", MIN_PROBE_MINS - 1).validate().is_err());
        assert!(probe("p", MIN_PROBE_MINS).validate().is_ok());
        assert!(probe("p", MAX_PROBE_MINS).validate().is_ok());
        assert!(probe("p", MAX_PROBE_MINS + 1).validate().is_err());
    }

    /// The same guarantee `daily_cap_mins` and `tiers` carry: a provider that never opts in
    /// serialises exactly as it did before the field existed.
    #[test]
    fn nothing_written_changes_shape_until_a_probe_is_set() {
        let mut provider = Provider {
            enabled: true,
            minutes: 30,
            daily_cap_mins: None,
            tiers: Vec::new(),
            probe: None,
        };
        assert_eq!(
            serde_json::to_string(&provider).unwrap(),
            r#"{"enabled":true,"minutes":30}"#
        );
        provider.probe = Some(probe("studygo-probe.exe", 15));
        let json = serde_json::to_string(&provider).unwrap();
        assert!(
            json.contains(r#""probe":{"exe":"studygo-probe.exe","every_mins":15}"#),
            "got {json}"
        );
        let back: Provider = serde_json::from_str(&json).unwrap();
        assert_eq!(back, provider);
    }

    /// Whether running the probe again today could grant anything — the check that stops the
    /// scheduler spending a request on a source that has already been paid in full.
    #[test]
    fn a_provider_is_exhausted_once_today_has_paid_it_in_full() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 8).unwrap();
        let yesterday = NaiveDate::from_ymd_opt(2026, 9, 7).unwrap();
        let entry = |date, minutes| EarnedDay { date, minutes };

        let latched = Provider {
            enabled: true,
            minutes: 30,
            daily_cap_mins: None,
            tiers: Vec::new(),
            probe: None,
        };
        assert!(!latched.exhausted_for(today, None));
        assert!(latched.exhausted_for(today, Some(&entry(today, None))));
        assert!(latched.exhausted_for(today, Some(&entry(today, Some(5)))));
        assert!(!latched.exhausted_for(today, Some(&entry(yesterday, None))));

        let capped = Provider {
            daily_cap_mins: Some(30),
            ..latched
        };
        assert!(!capped.exhausted_for(today, None));
        assert!(!capped.exhausted_for(today, Some(&entry(today, Some(16)))));
        assert!(capped.exhausted_for(today, Some(&entry(today, Some(30)))));
        assert!(capped.exhausted_for(today, Some(&entry(today, Some(31)))));
        assert!(
            capped.exhausted_for(today, Some(&entry(today, None))),
            "an untracked grant under a ceiling reads as the ceiling reached, as EarnedDay says"
        );
        assert!(!capped.exhausted_for(today, Some(&entry(yesterday, Some(30)))));
    }

    // ----- the grant itself, out of the handler and into the registry -----

    fn with_provider(name: &str, provider: Provider) -> Config {
        let mut cfg = Config::default();
        cfg.providers.insert(name.into(), provider);
        cfg
    }

    /// The registry decides, and every outcome is one of three shapes: a grant that moved the
    /// budget, an ordinary refusal that moved nothing, or a rejection of the request itself.
    #[test]
    fn earn_grants_refuses_and_rejects_from_one_place() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 8).unwrap();

        let mut cfg = Config::default();
        assert!(cfg.earn("studygo", today, None).is_err(), "not installed");
        let mut cfg = with_provider(
            "studygo",
            Provider {
                enabled: false,
                ..laddered()
            },
        );
        assert!(cfg.earn("studygo", today, None).is_err(), "turned off");

        // The original rule: one grant, then the latch.
        let mut cfg = with_provider(
            "studygo",
            Provider {
                daily_cap_mins: None,
                tiers: Vec::new(),
                ..laddered()
            },
        );
        assert_eq!(cfg.earn("studygo", today, None), Ok(Earn::Granted(30)));
        assert_eq!(cfg.extra.for_day(today), 30);
        assert_eq!(
            cfg.earned.get("studygo"),
            Some(&EarnedDay {
                date: today,
                minutes: None
            })
        );
        assert_eq!(
            cfg.earn("studygo", today, None),
            Ok(Earn::Refused(Refused::AlreadyGrantedToday))
        );
        assert_eq!(cfg.extra.for_day(today), 30, "a refusal moves nothing");

        // The ladder under a ceiling: the lower rung, then the difference, then the ceiling.
        let mut cfg = with_provider("studygo", laddered());
        assert_eq!(
            cfg.earn("studygo", today, Some(done(3, 5))),
            Ok(Earn::Refused(Refused::BelowThreshold))
        );
        assert!(cfg.earned.is_empty(), "below the bar writes no entry");
        assert_eq!(
            cfg.earn("studygo", today, Some(done(12, 5))),
            Ok(Earn::Granted(16))
        );
        assert_eq!(
            cfg.earn("studygo", today, Some(done(15, 5))),
            Ok(Earn::Granted(14))
        );
        assert_eq!(
            cfg.earn("studygo", today, Some(done(40, 60))),
            Ok(Earn::Refused(Refused::DailyCapReached))
        );
        assert_eq!(cfg.extra.for_day(today), 30);
        assert_eq!(cfg.earned.get("studygo").and_then(|e| e.minutes), Some(30));
    }

    /// Every refusal's wire value, spelled out.
    ///
    /// **The cross-repository contract, made explicit.** These three strings reach Voortgang as
    /// `{ok: false, reason}` and it branches on them, so they are not ours to rename — and until
    /// they were a type they existed as four scattered literals with nothing asserting any of them.
    /// Walking `Refused::ALL` is what stops a fourth variant arriving without a value anyone chose.
    #[test]
    fn every_refusal_keeps_the_wire_value_another_repository_reads() {
        assert_eq!(Refused::BelowThreshold.wire(), "below_threshold");
        assert_eq!(Refused::AlreadyGrantedToday.wire(), "already_granted_today");
        assert_eq!(Refused::DailyCapReached.wire(), "daily_cap_reached");
        let all: Vec<&str> = Refused::ALL.iter().map(|r| r.wire()).collect();
        assert_eq!(
            all.len(),
            3,
            "a variant was added without a wire value: {all:?}"
        );
        let unique: std::collections::BTreeSet<&&str> = all.iter().collect();
        assert_eq!(
            unique.len(),
            all.len(),
            "two refusals share a wire value: {all:?}"
        );
        crate::testutil::assert_all_lists_every_variant(
            include_str!("config.rs"),
            "pub enum Refused {",
            Refused::ALL.len(),
        );
    }

    /// The rung to aim at is the cheapest one still unmet, not the highest or the first.
    ///
    /// This is what the child is told to reach, so it has to be the nearest achievable thing
    /// rather than the most impressive: naming the top of the ladder to someone at the bottom of
    /// it is the discouraging choice, and naming whichever the parent happened to type first makes
    /// the message depend on entry order the way `reward_for` refuses to.
    #[test]
    fn the_rung_to_aim_at_is_the_cheapest_one_not_yet_met() {
        let ladder = laddered();
        // Nothing done: the 16-minute rung is nearer than the 30-minute one.
        assert_eq!(
            ladder.next_rung(done(0, 0)).map(|t| t.reward_mins),
            Some(16)
        );
        // The lower rung is met, so the next thing to aim at is the upper one.
        assert_eq!(
            ladder.next_rung(done(12, 0)).map(|t| t.reward_mins),
            Some(30)
        );
        // Everything met: nothing left to aim at, and nothing to say.
        assert_eq!(ladder.next_rung(done(99, 99)), None);
        // Entry order must not decide it.
        let mut reversed = laddered();
        reversed.tiers.reverse();
        assert_eq!(
            reversed.next_rung(done(0, 0)).map(|t| t.reward_mins),
            Some(16)
        );
        // A provider with no ladder has no rung to name.
        let plain = Provider {
            tiers: Vec::new(),
            ..laddered()
        };
        assert_eq!(plain.next_rung(done(0, 0)), None);
    }

    /// A rejected grant leaves the config exactly as it found it.
    ///
    /// **Required by the caller, not merely tidy.** Both callers run this inside
    /// `api::try_update_config`, whose contract is explicit: a mutation that returns `Err` must
    /// leave the config unchanged, because on that path the write guard is dropped *without
    /// saving*. A partial mutation would therefore live in memory and never reach disk — the
    /// parent sees a budget that the next restart silently revokes, which is the divergence that
    /// lock exists to prevent. `earn` satisfies it by rejecting before it touches anything, and
    /// this is what says so.
    #[test]
    fn a_rejected_grant_changes_nothing() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 8).unwrap();

        // Not installed, and switched off: both refused before any field is read.
        for provider in [
            None,
            Some(Provider {
                enabled: false,
                ..laddered()
            }),
        ] {
            let mut cfg = Config::default();
            if let Some(provider) = provider {
                cfg.providers.insert("studygo".into(), provider);
            }
            let before = serde_json::to_string(&cfg).unwrap();
            assert!(cfg.earn("studygo", today, None).is_err());
            assert_eq!(
                serde_json::to_string(&cfg).unwrap(),
                before,
                "a rejected grant must not have moved anything"
            );
        }

        // The sources cap, which is the one rejection that happens *after* a reward has been
        // resolved and is therefore the one with something to leave behind.
        let mut cfg = Config::default();
        for i in 0..MAX_EARNED_SOURCES {
            let name = format!("s{i}");
            cfg.providers.insert(
                name.clone(),
                Provider {
                    daily_cap_mins: None,
                    tiers: Vec::new(),
                    ..laddered()
                },
            );
            assert!(matches!(cfg.earn(&name, today, None), Ok(Earn::Granted(_))));
        }
        cfg.providers.insert("one-more".into(), laddered());
        let before = serde_json::to_string(&cfg).unwrap();
        assert!(cfg.earn("one-more", today, None).is_err());
        assert_eq!(
            serde_json::to_string(&cfg).unwrap(),
            before,
            "the source cap must reject before it writes, not after"
        );
    }

    /// The sources cap counts distinct sources, and only a *new* one can trip it.
    #[test]
    fn a_new_source_past_the_cap_is_rejected_and_a_counted_one_is_not() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 8).unwrap();
        let mut cfg = Config::default();
        for i in 0..MAX_EARNED_SOURCES {
            let name = format!("s{i}");
            cfg.providers.insert(
                name.clone(),
                Provider {
                    daily_cap_mins: Some(60),
                    tiers: Vec::new(),
                    ..laddered()
                },
            );
            assert_eq!(cfg.earn(&name, today, None), Ok(Earn::Granted(30)));
        }
        cfg.providers.insert("one-more".into(), laddered());
        assert!(cfg.earn("one-more", today, None).is_err());
        assert_eq!(
            cfg.earn("s0", today, None),
            Ok(Earn::Granted(30)),
            "a source already counted today is not a new one"
        );
    }
}
