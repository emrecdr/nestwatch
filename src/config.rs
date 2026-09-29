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
/// Longest settling period a parent may put before a provider's first check of the day.
///
/// The same ceiling as [`MAX_PROBE_MINS`] and for a weaker reason: there is no risk here to bound,
/// only a number that has to stop somewhere and a dashboard box that needs a `max`. Worth knowing
/// where the real limit is instead — a settling period at or past the day's budget means the first
/// check never happens, because the machine locks before it is due. Nothing refuses that pair:
/// the budget and the provider are edited independently and either can move under the other, so
/// refusing it here would reject a config that was valid when it was written. See
/// `docs/PLUGIN-SYSTEM.md`, *A settling period, measured in the only clock he cannot reset*.
pub const MAX_SETTLE_MINS: u32 = 240;
/// Longest allowance a practice gate may put on a day.
///
/// A day, minus nothing. The allowance is a *ceiling*, so an absurdly large one is harmless — it
/// simply never binds — which is the opposite of the settling period above, where a large value
/// silently switches the feature off. Bounded anyway because every minutes field here is, and
/// because the dashboard's box needs a `max` that `web.rs` can hold it to.
pub const MAX_GATE_ALLOWANCE_MINS: u32 = 1440;
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
    /// Say the shortfall notice at **every** check that finds him short,
    /// rather than once a local day.
    ///
    /// `false` — the default, and how every config written before this field
    /// loads — is the rationed rule `docs/PLUGIN-SYSTEM.md` argues for: a
    /// grant is announced every time, a shortfall is mentioned once a day,
    /// because a fifteen-minute timer that says *not yet* every fifteen
    /// minutes is a nag, and a nagging tool and a purely negative one fail in
    /// the same direction.
    ///
    /// **That argument is about a reward, and it does not survive being
    /// pointed at a gate.** Rationing is right when the notice means *there is
    /// more to earn if you want it*, and wrong when it is the only warning
    /// that the machine is going to lock: a child told once at 16:00 and
    /// locked out at 16:35 was, for practical purposes, not told. Which of
    /// the two a household has is a property of its configuration, not of
    /// this code, so it is the parent's switch.
    ///
    /// It moves **only** the shortfall. A grant is still announced every
    /// time, a failed check still says nothing to him, and the day latch and
    /// the ceiling still say nothing — those two mean the day is already
    /// paid, so there is no rung to aim at whatever this is set to.
    #[serde(default, skip_serializing_if = "is_false")]
    pub remind_every_check: bool,
    /// A ceiling this provider puts on the day until its bar is met. See [`Gate`].
    ///
    /// Absent — the default, and every config written before it existed — means this provider
    /// only ever *adds* time, which is what an integration did for its whole life before today.
    /// Present, it also takes the day down to [`Gate::allowance_mins`] until the child has done
    /// the work, and removing or switching off the provider removes the ceiling with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<Gate>,
}

/// `skip_serializing_if` for a `bool` that defaults to `false`.
///
/// Named rather than `std::ops::Not::not`, which type-checks here and reads
/// like a mistake. The point of every `skip_serializing_if` in this file is
/// that a household which never opted in keeps a byte-identical
/// `config.json`, and that intent should be legible at the attribute.
fn is_false(b: &bool) -> bool {
    !b
}

/// `skip_serializing_if` for a `u32` whose zero means *not set*. Same intent as [`is_false`]: a
/// household that never named one keeps the bytes it had.
fn is_zero(n: &u32) -> bool {
    *n == 0
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

/// A ceiling one provider puts on the day until its bar is met — a practice gate.
///
/// **The household rule this exists for:** *he has thirty-five minutes; once he has done his
/// practice he has his normal day.* The obvious spelling of that — and the one this codebase
/// shipped first — is to write 35 into `daily_budget_mins` and make the top reward worth the
/// difference. It produces the right numbers and it is wrong, for a reason that only shows up at
/// the off switch: it spends the **parent's own daily limit** as the gate's allowance, so the
/// number meaning *his normal day* is then written down nowhere, and switching the integration off
/// leaves the short day standing with no way back. A plugin may not redefine the household's
/// settings. See `docs/PLUGIN-SYSTEM.md`, *A gate is a ceiling, not a budget*.
///
/// So the allowance belongs here, to the provider, and it **caps** rather than replaces: the
/// parent's daily limit, per-weekday limits, routines and bedtime all stay exactly what they were,
/// and this puts a lid on the day while the bar is unmet. Remove the provider and the lid goes
/// with it — which is the property the whole registry is built to keep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gate {
    /// Minutes the day is capped at while the bar below is unmet.
    ///
    /// Raised by whatever this provider has already granted today, so a ladder's lower rungs
    /// extend the gate rather than adding to the day — see [`Config::gate_cap_mins`]. That is what
    /// makes *do two-thirds and get another quarter of an hour* a longer leash rather than a
    /// bonus, and it is why meeting the bar afterwards lands on exactly the normal day rather than
    /// the normal day plus change.
    pub allowance_mins: u32,
    /// Questions that lift the gate, or `0` for "the bar does not ask about questions".
    #[serde(default)]
    pub questions: u32,
    /// Minutes *practised* that lift the gate, or `0` for "does not ask".
    #[serde(default)]
    pub minutes_practised: u32,
}

impl Gate {
    /// Whether reported work lifts this gate.
    ///
    /// Deliberately the same rule as [`Tier::met`] — either condition suffices, and a zero
    /// threshold is not a condition — because a household states the bar and the rungs in one
    /// breath and would not expect them to be read differently. A gate with neither threshold set
    /// can never be lifted by work, which is the expensive-but-honest reading of a blank field;
    /// [`Gate::validate`] refuses that pair outright rather than letting it ship.
    pub fn met(&self, done: Progress) -> bool {
        Tier {
            questions: self.questions,
            minutes_practised: self.minutes_practised,
            reward_mins: 0,
        }
        .met(done.questions, done.minutes)
    }

    /// Refuse a gate that cannot be lifted, or one whose allowance is out of range.
    ///
    /// **A gate with no bar is the dangerous one** and is why this is a hard refusal rather than a
    /// dashboard hint: it caps the child's day at the allowance with no work that can ever raise
    /// it, for every day until a parent notices. Every other bad number here is a number; this one
    /// is a child locked to thirty-five minutes a day by a blank field.
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_GATE_ALLOWANCE_MINS).contains(&self.allowance_mins) {
            return Err(format!(
                "the gate's allowance must be 1-{MAX_GATE_ALLOWANCE_MINS} minutes"
            ));
        }
        if self.questions == 0 && self.minutes_practised == 0 {
            return Err(
                "a gate needs a bar that can lift it: set questions, minutes practised, or both"
                    .into(),
            );
        }
        if self.questions > MAX_TIER_QUESTIONS || self.minutes_practised > MAX_TIER_MINUTES {
            return Err(format!(
                "the gate's bar must be at most {MAX_TIER_QUESTIONS} questions or \
                 {MAX_TIER_MINUTES} minutes practised"
            ));
        }
        Ok(())
    }
}

/// What an integration reads back about its own gate today — see [`Config::gate_read_back`].
///
/// Two facts, both caused by the integration itself: whether the work it reported has met the bar
/// today, and how many minutes its rungs have raised the gate by. Neither says anything about the
/// child's day that the integration did not put there, which is the line
/// `auth::INTEGRATION_USAGE_FIELDS` draws for everything else on that route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct GateReadBack {
    /// The bar was met today, so the gate no longer binds — [`EarnedDay::bar_met`].
    pub lifted: bool,
    /// Minutes this source's rungs have raised its gate by today. Not `extra_mins`: a gated grant
    /// never reaches the parent's pool (`Config::earn`), so that field cannot confirm it.
    pub earned_mins: u32,
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
    /// Minutes of **screen time used today** before the day's first check runs. `0` is no
    /// settling period, which is what every config written before this field existed says.
    ///
    /// Screen time rather than wall clock, and that is the whole of the design: a period measured
    /// from when the session became active is one the child can reset at the Start menu, so
    /// signing out and back in inside it would mean the probe never ran at all. The tally this
    /// reads only goes up, and it is the same number the budget is spent against — so "three
    /// minutes in" means the same thing to the gate and to the enforcer, and the two cannot drift.
    ///
    /// It delays the first check **of each day**, and that is a consequence rather than a second
    /// rule: the tally resets at midnight, so the floor applies again; and within a day the tally
    /// only rises, so once it is past the mark no later check is ever delayed by it. `probe.rs`
    /// says what a mutant had to prove before this was written down.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub first_check_after_mins: u32,
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
        if self.first_check_after_mins > MAX_SETTLE_MINS {
            return Err(format!(
                "the settling period before the first check must be 0-{MAX_SETTLE_MINS} minutes"
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
    /// **A push that reports nothing lands on the original behaviour, and that is deliberate:** it
    /// is worth [`Provider::minutes`] however many tiers are configured, which is what a client
    /// that predates counts sends, and what a catch-up push for yesterday sends.
    ///
    /// **A push that reports work is paid for work.** With tiers, the best one it meets. With
    /// none, the single reward — for any work at all, or under a gate for work that meets the
    /// gate's bar, since then the bar is the only rung there is. A report of no work earns
    /// nothing: once a client pushes counts rather than a verdict, it pushes them when nothing has
    /// happened yet too, and paying that would pay for a sync.
    ///
    /// The best matching tier wins rather than the first, so the answer does not depend on the
    /// order a parent happened to enter them in.
    pub fn reward_for(&self, progress: Option<Progress>) -> Option<u32> {
        match (progress, self.tiers.as_slice()) {
            (None, _) => Some(self.minutes),
            (Some(done), []) => match &self.gate {
                Some(gate) => gate.met(done),
                None => done.questions > 0 || done.minutes > 0,
            }
            .then_some(self.minutes),
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
    /// and when there are no tiers at all, which is a provider with no ladder to name a step on.
    pub fn next_rung(&self, done: Progress) -> Option<&Tier> {
        self.tiers
            .iter()
            .filter(|tier| !tier.met(done.questions, done.minutes))
            .min_by_key(|tier| tier.reward_mins)
    }

    /// The most this provider may grant across one local day, or `None` for the original
    /// one-grant-per-day latch.
    ///
    /// **One spelling, and that is the point of it being a function.** [`Config::earn`] and
    /// [`Provider::exhausted_for`] both need this number, and while they each derived it the two
    /// disagreed: `earn` had learned that a gate's ladder is bounded by its own top rung and
    /// `exhausted_for` had not. A gated provider with no typed `daily_cap_mins` was therefore paid
    /// its first rung and then dropped out of `probe::run_once`'s due list for the rest of the
    /// day — the child did the work, took the lowest reward, and the gate stayed shut with nothing
    /// said, which is the exact failure the ladder exists to prevent. Every probe test missed it
    /// because the fixture they share sets `daily_cap_mins`, which is the one arm where the two
    /// spellings agreed. That fix was necessary and not sufficient: a ladder topped out *below*
    /// the bar was still paid in full with the gate shut, so `exhausted_for` now asks a gated
    /// provider about its bar instead of about this number.
    ///
    /// An explicit ceiling always wins. A gate without one is bounded by its top rung, because
    /// that is the most the ladder can pay and a gate paying past it would be a ceiling nobody
    /// chose. A provider with neither keeps the latch, which is how every config that has not
    /// opted in loads — and a gate with no tiers keeps it too, since there is no ladder to climb.
    pub fn ceiling_mins(&self) -> Option<u32> {
        self.daily_cap_mins.or_else(|| {
            self.gate.as_ref()?;
            self.tiers.iter().map(|tier| tier.reward_mins).max()
        })
    }

    /// Whether another check today could change anything — the question the probe scheduler asks
    /// before spending a request, so a source with nothing left to decide stops being polled for
    /// the day.
    ///
    /// **Under a gate that is whether the bar has been met, not whether the rungs are paid.** A
    /// check can open a gate as well as pay, and a ladder whose top rung sits below the bar is paid
    /// in full with the gate still shut: stopping there took the provider off the due list before
    /// the one check that could have opened it. Once the bar is met the reverse holds — a rung
    /// still unclimbed could only raise a ceiling that no longer applies.
    ///
    /// Otherwise it reads the same entry [`Config::earn`] writes, under the same rules: with no
    /// ceiling the first grant latches the day; with one, an untracked entry counts as the ceiling
    /// reached (see [`EarnedDay::minutes`]) and a tracked one is compared against it. An entry from
    /// another day says nothing about today.
    pub fn exhausted_for(&self, today: NaiveDate, earned: Option<&EarnedDay>) -> bool {
        let Some(entry) = earned.filter(|e| e.date == today) else {
            return false;
        };
        if self.gate.is_some() {
            return entry.bar_met;
        }
        match (self.ceiling_mins(), entry.minutes) {
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
    /// Work was reported and earned nothing: it met no tier, or — for a provider with none — it was
    /// no work at all, or short of the gate's bar. The only one worth telling the child about: it is
    /// the one with something still to aim at.
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
    /// Whether this source's [`Gate`] bar was met on [`EarnedDay::date`].
    ///
    /// The one piece of *verdict* in a struct that otherwise records amounts, and it is here
    /// rather than recomputed because the evidence does not survive: the progress a check reported
    /// lives in memory (`probe::ProbeStatus`) and dies with the process, while the day's ceiling
    /// has to keep meaning the same thing across a restart at four in the afternoon.
    ///
    /// `false` for every entry written before this field, which is the right reading of silence:
    /// a config that predates gates has no gate to have lifted.
    pub bar_met: bool,
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
        match (self.minutes, self.bar_met) {
            // Still the bare date for a household with neither a ceiling nor a gate, which is
            // what this impl exists for.
            (None, false) => self.date.serialize(serializer),
            (minutes, bar_met) => {
                use serde::ser::SerializeStruct;
                // Counted rather than fixed at 2: `serialize_struct`'s length has to match the
                // fields actually written, and a gate without a ceiling writes only one of them.
                let fields = 1 + usize::from(minutes.is_some()) + usize::from(bar_met);
                let mut entry = serializer.serialize_struct("EarnedDay", fields)?;
                entry.serialize_field("date", &self.date)?;
                if let Some(minutes) = minutes {
                    entry.serialize_field("minutes", &minutes)?;
                }
                // Skipped when false, so adding gates to this build changed no existing file.
                if bar_met {
                    entry.serialize_field("bar_met", &true)?;
                }
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
                #[serde(default)]
                bar_met: bool,
            },
        }
        Ok(match Repr::deserialize(deserializer)? {
            Repr::Legacy(date) => Self {
                date,
                minutes: None,
                bar_met: false,
            },
            Repr::Tracked {
                date,
                minutes,
                bar_met,
            } => Self {
                date,
                minutes,
                bar_met,
            },
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
        let (reward, ceiling, bar_met_now) = {
            let provider = self.provider_authority(source)?;
            (
                provider.reward_for(reported),
                // What this provider may pay across today. Under a gate that is its own top
                // rung whether or not one was typed, which bounds a compromised client exactly as
                // the latch did while leaving the ladder climbable. [`Provider::ceiling_mins`]
                // holds the rule and the reason it is not written twice.
                provider.ceiling_mins(),
                provider
                    .gate
                    .as_ref()
                    .zip(reported)
                    .is_some_and(|(gate, done)| gate.met(done)),
            )
        };
        // Read out as an owned value, not held as a borrow: the map is mutated a few lines
        // below, and `Option<Option<u32>>` says the two things that matter separately — whether
        // this source has an entry for today at all, and whether that entry's amount was ever
        // measured. Read **before** the bar is recorded, because recording it can create today's
        // entry, and a first push that meets the bar must not then find itself already paid.
        let spent = self
            .earned
            .get(source)
            .filter(|entry| entry.date == today)
            .map(|entry| entry.minutes);
        // **The gate opens on work done, not on minutes paid, and that has to happen before every
        // refusal below — `below_threshold` included.** A child who does two-thirds of his
        // practice takes the lower rung, which spends the day's single grant; finishing afterwards
        // then arrives here and is refused as `already_granted_today`. And nothing ties a ladder
        // to the bar, so work can meet the bar and no rung at all. If the bar were only recorded on
        // the granting path, the push that proves he finished would be the one push that could not
        // open his gate, and he would sit at the allowance having done everything asked of him.
        // The minutes rules below are unchanged — this records a fact about the child, not a
        // payment.
        if bar_met_now {
            self.note_bar_met(source, today);
        }
        // Work was reported and it earns nothing yet — see `Provider::reward_for`. Not an error: a
        // client that pushes whatever it sees and lets this machine judge is exactly what tiers
        // and the bar are for, so "not yet" has to be an ordinary answer rather than a rejection.
        let Some(mut minutes) = reward else {
            return Ok(Earn::Refused(Refused::BelowThreshold));
        };
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
        // Measured where a ceiling governs it — and now also where a gate does, because a gate's
        // ceiling is `allowance + what this source has granted today` and that sum needs the
        // second term. Still not written unconditionally: a household with neither keeps the bare
        // date, which is the single thing `EarnedDay`'s hand-written serde impls exist to
        // prevent changing.
        let gated = self.providers.get(source).and_then(|p| p.gate.as_ref());
        let tracked =
            (ceiling.is_some() || gated.is_some()).then(|| spent.flatten().unwrap_or(0) + minutes);
        // Once met, met for the day: a later check reporting less — a provider that resets a
        // counter, a probe that reads a stale page — must not shut a gate the child has already
        // opened and send him from his normal day back to the allowance mid-afternoon.
        let bar_met = self
            .earned
            .get(source)
            .is_some_and(|entry| entry.date == today && entry.bar_met);
        self.earned.insert(
            source.to_string(),
            EarnedDay {
                date: today,
                minutes: tracked,
                bar_met,
            },
        );
        // **A gated provider's minutes raise its gate, not the day.** `Config::extra` is the
        // parent's own pool — grants, redeemed codes, a bedtime extension — and adding to it here
        // would mean that meeting the bar later handed the child his normal day *plus* whatever
        // the lower rungs paid on the way. What a rung buys is a longer leash while he is still
        // short, which is exactly a higher ceiling. See `Config::gate_cap_mins`.
        if gated.is_none() {
            self.extra.add(today, minutes);
        }
        Ok(Earn::Granted(minutes))
    }

    /// Record that a gated source's bar was met today, whatever today's minutes have done.
    ///
    /// Creates the day's entry if there is none, and **with `Some(0)` rather than `None`**, which
    /// is the whole subtlety: `None` is read everywhere else as *granted an untracked amount*, so
    /// an entry conjured here with `None` would tell the ceiling that an unmeasured grant had
    /// already happened and refuse every real one for the rest of the day. Zero is the true
    /// statement — nothing has been granted yet — and it is only ever written for a provider that
    /// is tracked anyway, so it changes no file that was not already keeping a number.
    fn note_bar_met(&mut self, source: &str, today: NaiveDate) {
        // The same pruning the granting path does, for the same reason: an entry from an earlier
        // day must not be mistaken for today's, and this can run on a day where nothing grants.
        self.earned.retain(|_, entry| entry.date == today);
        // Read before the mutable borrow: the cap is about how many sources the map may hold, and
        // asking that question inside a `match` arm on `get_mut` is what the borrow checker is
        // for.
        let room = self.earned.len() < MAX_EARNED_SOURCES;
        match self.earned.get_mut(source) {
            Some(entry) => entry.bar_met = true,
            // Bounded by the same cap as a grant. A source that cannot be admitted cannot open a
            // gate either — otherwise the cap on this map would be one line of defence with a
            // second door beside it.
            None if room => {
                self.earned.insert(
                    source.to_string(),
                    EarnedDay {
                        date: today,
                        minutes: Some(0),
                        bar_met: true,
                    },
                );
            }
            None => {}
        }
    }

    /// The ceiling every installed practice gate puts on `today`, or `None` when none applies.
    ///
    /// **A ceiling, never a budget.** The parent's own rules — the daily limit, per-weekday
    /// limits, routines, bedtime — are computed without ever consulting this, and this puts a lid
    /// on the result. That is what makes the feature removable: take the provider out and the lid
    /// goes with it, leaving numbers nothing ever rewrote. `Rules` is not told what a provider
    /// is; it takes this as a number, exactly as it already takes granted extra.
    ///
    /// A gate stops applying for four reasons, and only the first is about the child:
    ///
    /// - **the bar was met today**, recorded on [`EarnedDay::bar_met`];
    /// - **the provider is switched off**, which is the answer to *what does the off switch do* —
    ///   it must hand the day back, or the switch would be a punishment;
    /// - **the provider is gone**, the same, one step further;
    /// - **this machine cannot currently check**, which `not_checking` names. A probe that failed
    ///   or a scheduler that has stopped is not evidence that the child has not practised, and
    ///   treating it as though it were would let a StudyGo outage cost him his day. Note what
    ///   that buys and what it costs: it is the correct reading of ignorance, and it also means a
    ///   child who can stop the check — pulling the network is enough — can lift the gate. That
    ///   trade is the household's to make and is written up in `docs/PLUGIN-SYSTEM.md`.
    ///
    /// With several gates installed the **tightest** wins, because a child under two rules is
    /// under both, and being under both is being under the smaller.
    pub fn gate_cap_mins(
        &self,
        today: NaiveDate,
        not_checking: &std::collections::BTreeSet<String>,
    ) -> Option<u32> {
        self.providers
            .iter()
            .filter(|(name, _)| !not_checking.contains(name.as_str()))
            .filter_map(|(name, provider)| {
                let gate = provider.gate.as_ref().filter(|_| provider.enabled)?;
                let today_entry = self.earned.get(name).filter(|entry| entry.date == today);
                if today_entry.is_some_and(|entry| entry.bar_met) {
                    return None;
                }
                // Saturating for the reason every accumulator in `rules` is: a hand-edited
                // allowance plus a granted minute must not wrap to a near-zero ceiling, which
                // would lock the child out rather than let him through.
                Some(
                    gate.allowance_mins
                        .saturating_add(today_entry.and_then(|entry| entry.minutes).unwrap_or(0)),
                )
            })
            .min()
    }

    /// What `source` may read back about its own gate on `today`, or `None` when it has no gate.
    ///
    /// **Why this exists: `extra_mins` cannot see a gated grant.** A gated provider's minutes raise
    /// its gate and never `Config::extra`, so a client that confirms a push by reading
    /// `extra_mins` back — Voortgang does, and treats a shortfall as failure — sees `0` after being
    /// paid and tells the parent nothing happened. Replaying its exact request against a gated
    /// provider showed exactly that. This is the gate's own ledger, [`Config::earned`], read for
    /// the one source asking.
    ///
    /// Present whenever the provider has a gate, lifted or not, so its absence means *no gate*
    /// rather than *nothing yet*. The provider being switched off or removed is not a case here:
    /// `auth::require_auth` refuses such an integration before any read.
    pub fn gate_read_back(&self, source: &str, today: NaiveDate) -> Option<GateReadBack> {
        self.providers.get(source)?.gate.as_ref()?;
        let entry = self.earned.get(source).filter(|entry| entry.date == today);
        Some(GateReadBack {
            lifted: entry.is_some_and(|entry| entry.bar_met),
            earned_mins: entry.and_then(|entry| entry.minutes).unwrap_or(0),
        })
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
            remind_every_check: false,
            gate: None,
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

    /// A provider with no tiers pays for work, and a report of none is not work.
    ///
    /// Its single reward used to be paid for any push that carried counts, whatever they said —
    /// including zero questions and zero minutes, which is what a client reports when it syncs
    /// before the child has practised. Once a client pushes the work rather than a verdict, that
    /// paid for nothing. Under a gate the gate's own bar is the only rung there is: a tierless
    /// gated provider paid its reward on the first partial report, raising the gate by a step
    /// nobody configured.
    ///
    /// **A push that reports nothing is still the single reward**, tiers or not, which is the
    /// compatibility guarantee for clients that predate counts and the shape of a catch-up push.
    #[test]
    fn a_provider_without_tiers_pays_for_work_and_only_work() {
        let plain = Provider {
            enabled: true,
            minutes: 25,
            daily_cap_mins: None,
            tiers: Vec::new(),
            probe: None,
            remind_every_check: false,
            gate: None,
        };
        assert_eq!(
            plain.reward_for(Some(done(0, 0))),
            None,
            "a report of no work earns nothing"
        );
        assert_eq!(plain.reward_for(Some(done(1, 0))), Some(25));
        assert_eq!(
            plain.reward_for(Some(done(0, 1))),
            Some(25),
            "and any work at all is what the single reward was always for"
        );
        assert_eq!(plain.reward_for(None), Some(25));
        assert_eq!(
            laddered().reward_for(None),
            Some(30),
            "and a push reporting nothing is the single reward, however many tiers exist"
        );

        let gated = Provider {
            gate: Some(Gate {
                allowance_mins: 35,
                questions: 15,
                minutes_practised: 30,
            }),
            ..plain
        };
        assert_eq!(
            gated.reward_for(Some(done(14, 29))),
            None,
            "under a gate with no rungs, work short of the bar earns nothing"
        );
        assert_eq!(gated.reward_for(Some(done(15, 0))), Some(25));
        assert_eq!(
            gated.reward_for(Some(done(0, 30))),
            Some(25),
            "and the bar — either half of it — is what pays"
        );
        assert_eq!(gated.reward_for(None), Some(25));
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
                bar_met: false,
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
            remind_every_check: false,
            gate: None,
        };
        assert_eq!(
            serde_json::to_string(&provider).unwrap(),
            r#"{"enabled":true,"minutes":30}"#,
            "a provider with no ceiling must serialise exactly as it did before the field existed"
        );

        let untracked = EarnedDay {
            date: NaiveDate::from_ymd_opt(2026, 9, 8).unwrap(),
            minutes: None,
            bar_met: false,
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
            remind_every_check: false,
            gate: None,
        };
        let json = serde_json::to_string(&provider).unwrap();
        assert!(json.contains(r#""daily_cap_mins":45"#), "got {json}");

        let tracked = EarnedDay {
            date: NaiveDate::from_ymd_opt(2026, 9, 8).unwrap(),
            minutes: Some(16),
            bar_met: false,
        };
        let back: EarnedDay = serde_json::from_str(&serde_json::to_string(&tracked).unwrap())
            .expect("the tracked form must round trip");
        assert_eq!(back, tracked);

        // And the two shapes a gate adds, one of which carries no amount at all: a bar met on a
        // day whose earlier grant was never measured — a gate added after an untracked grant.
        // The object's length is counted from the fields present, and serde_json writes `{}` at
        // once for a length of zero, so a count that came to zero for exactly that shape would
        // write a `config.json` no build could read back.
        for (minutes, bar_met) in [(Some(16), true), (None, true)] {
            let gated = EarnedDay {
                date: NaiveDate::from_ymd_opt(2026, 9, 8).unwrap(),
                minutes,
                bar_met,
            };
            let json = serde_json::to_string(&gated).unwrap();
            let back: EarnedDay = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("{json} must round trip: {e}"));
            assert_eq!(back, gated);
        }
    }

    /// Every opt-in a provider carries survives a save and a load.
    ///
    /// Each is skipped when unset, so that a household which never opted in keeps a
    /// byte-identical `config.json` — which leaves each `skip_serializing_if` one wrong answer
    /// away from dropping the setting of a household that did. Nothing caught that: with
    /// `is_false` or `is_zero` answering *skip* for everything, every test passed, and the tick box
    /// and the settling period would have come back unset after each restart.
    #[test]
    fn a_providers_opt_ins_survive_a_round_trip() {
        let provider = Provider {
            enabled: true,
            minutes: 30,
            daily_cap_mins: Some(45),
            tiers: vec![Tier {
                questions: 10,
                minutes_practised: 20,
                reward_mins: 16,
            }],
            probe: Some(Probe {
                exe: "studygo-probe.exe".into(),
                every_mins: 15,
                first_check_after_mins: 5,
            }),
            remind_every_check: true,
            gate: Some(Gate {
                allowance_mins: 35,
                questions: 15,
                minutes_practised: 30,
            }),
        };
        let json = serde_json::to_string(&provider).unwrap();
        let back: Provider = serde_json::from_str(&json).unwrap();
        assert_eq!(back, provider, "lost in {json}");
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
            first_check_after_mins: 0,
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

    use std::collections::BTreeSet;

    /// A gate's rungs accumulate, because a gate's ceiling is its own top rung.
    ///
    /// The trap this closes, found by driving it rather than by reading it: the legacy rule is
    /// **one grant per source per day** unless a ceiling is set, and a gate makes rungs the thing
    /// that extends the leash. So a parent who wrote a two-rung ladder under a gate and set no
    /// daily maximum got the first rung and silence — the second was refused as
    /// `already_granted_today`, and the card said nothing, because from the registry's point of
    /// view the day was paid.
    ///
    /// The latch exists to bound a compromised client to one reward. Under a gate it bounds
    /// nothing that was not already bounded: the ceiling is applied as `min(base, cap)`, so the
    /// worst a client can do by pushing forever is reach the day the parent already set. What it
    /// does instead is break the ladder. So a gated provider's ceiling **defaults to its highest
    /// rung** — the parent's own number, requiring no second field — and an explicit
    /// `daily_cap_mins` still wins.
    #[test]
    fn a_gates_rungs_accumulate_up_to_the_top_one() {
        let day = NaiveDate::from_ymd_opt(2026, 9, 16).unwrap();
        let none = BTreeSet::new();
        let mut cfg = Config::default();
        cfg.providers.insert(
            "studygo".into(),
            Provider {
                enabled: true,
                minutes: 1,
                daily_cap_mins: None,
                tiers: vec![
                    Tier {
                        questions: 5,
                        minutes_practised: 0,
                        reward_mins: 8,
                    },
                    Tier {
                        questions: 10,
                        minutes_practised: 0,
                        reward_mins: 16,
                    },
                ],
                probe: None,
                remind_every_check: false,
                gate: Some(Gate {
                    allowance_mins: 35,
                    questions: 15,
                    minutes_practised: 0,
                }),
            },
        );
        let p = |questions| {
            Some(Progress {
                questions,
                minutes: 0,
            })
        };

        assert_eq!(cfg.gate_cap_mins(day, &none), Some(35));
        assert_eq!(cfg.earn("studygo", day, p(5)), Ok(Earn::Granted(8)));
        assert_eq!(
            cfg.gate_cap_mins(day, &none),
            Some(43),
            "35 + the first rung"
        );

        // The line the latch used to refuse. The second rung is worth 16 IN TOTAL, so what lands
        // is the difference — the same arithmetic `A ceiling instead of a latch` already applies,
        // and the reason the ladder reads as a ladder rather than as a set of alternatives.
        assert_eq!(cfg.earn("studygo", day, p(10)), Ok(Earn::Granted(8)));
        assert_eq!(
            cfg.gate_cap_mins(day, &none),
            Some(51),
            "35 + the second rung, not + both"
        );

        // And the top rung is the end of it: nothing further can be farmed out of the ladder.
        assert_eq!(
            cfg.earn("studygo", day, p(10)),
            Ok(Earn::Refused(Refused::DailyCapReached))
        );
        assert_eq!(cfg.gate_cap_mins(day, &none), Some(51));

        // The bar still opens the gate, on work done rather than on minutes paid — which is the
        // only thing that could open it once the ladder is spent.
        assert_eq!(
            cfg.earn("studygo", day, p(15)),
            Ok(Earn::Refused(Refused::DailyCapReached))
        );
        assert_eq!(
            cfg.gate_cap_mins(day, &none),
            None,
            "the bar is met, so the gate is gone"
        );

        // **And none of that reaches a provider without a gate**, which is the guarantee every
        // field in this registry has kept: a laddered, ungated, uncapped provider still takes the
        // original rule — one grant per source per day, whatever it was worth. Pinned here rather
        // than assumed, because the implicit ceiling above is a *default*, and a default that
        // leaked would quietly turn every existing ladder into a repeatable one.
        let mut plain = Config::default();
        plain.providers.insert(
            "chores".into(),
            Provider {
                enabled: true,
                minutes: 1,
                daily_cap_mins: None,
                tiers: vec![
                    Tier {
                        questions: 5,
                        minutes_practised: 0,
                        reward_mins: 8,
                    },
                    Tier {
                        questions: 10,
                        minutes_practised: 0,
                        reward_mins: 16,
                    },
                ],
                probe: None,
                remind_every_check: false,
                gate: None,
            },
        );
        assert_eq!(plain.earn("chores", day, p(5)), Ok(Earn::Granted(8)));
        assert_eq!(
            plain.earn("chores", day, p(10)),
            Ok(Earn::Refused(Refused::AlreadyGrantedToday)),
            "an ungated ladder still latches: the implicit ceiling is the gate's, not every \
             ladder's"
        );
        assert_eq!(
            plain.extra.for_day(day),
            8,
            "and its minutes still reach the day, unlike a gate's"
        );
    }

    /// Work that meets the bar opens the gate even when it meets no rung.
    ///
    /// Nothing ties a ladder to the bar: a parent may put every rung above it, and with *either
    /// condition suffices* on both sides there is no single order to require anyway. `earn`
    /// recorded the bar only after deciding the push was worth something, so a push that met the
    /// bar and no rung returned `below_threshold` first — and the child who had done exactly the
    /// practice asked of him stayed at the allowance. Measured before the fix: 35.
    #[test]
    fn meeting_the_bar_opens_the_gate_even_below_every_rung() {
        let day = NaiveDate::from_ymd_opt(2026, 9, 16).unwrap();
        let none = BTreeSet::new();
        let mut cfg = Config::default();
        cfg.providers.insert(
            "studygo".into(),
            Provider {
                enabled: true,
                minutes: 30,
                daily_cap_mins: None,
                // The only rung is past the bar.
                tiers: vec![Tier {
                    questions: 20,
                    minutes_practised: 0,
                    reward_mins: 16,
                }],
                probe: None,
                remind_every_check: false,
                gate: Some(Gate {
                    allowance_mins: 35,
                    questions: 15,
                    minutes_practised: 0,
                }),
            },
        );

        assert_eq!(
            cfg.earn("studygo", day, Some(done(15, 0))),
            Ok(Earn::Refused(Refused::BelowThreshold)),
            "no rung is met, so nothing is paid"
        );
        assert_eq!(
            cfg.gate_cap_mins(day, &none),
            None,
            "but the bar is, so the gate is gone"
        );
        assert_eq!(
            cfg.gate_read_back("studygo", day),
            Some(GateReadBack {
                lifted: true,
                earned_mins: 0,
            }),
            "and the integration reads back a lifted gate that paid nothing, which is the truth"
        );
    }

    /// The first push of the day that meets the bar is paid as well as opening the gate.
    ///
    /// Recording the bar can create today's entry, so `earn` reads what was already spent before
    /// it records anything. Read afterwards, a first push found the entry the bar had just made
    /// and was refused as `already_granted_today`: the gate opened, the integration was told it
    /// had been paid already, and its read-back showed nothing earned. No test failed with the
    /// two reads swapped, which is why this one exists.
    #[test]
    fn a_first_push_that_meets_the_bar_is_paid_and_opens_the_gate() {
        let day = NaiveDate::from_ymd_opt(2026, 9, 16).unwrap();
        let mut cfg = Config::default();
        cfg.providers.insert(
            "studygo".into(),
            Provider {
                enabled: true,
                minutes: 30,
                daily_cap_mins: None,
                tiers: Vec::new(),
                probe: None,
                remind_every_check: false,
                gate: Some(Gate {
                    allowance_mins: 35,
                    questions: 15,
                    minutes_practised: 0,
                }),
            },
        );

        assert_eq!(
            cfg.earn("studygo", day, Some(done(15, 0))),
            Ok(Earn::Granted(30)),
            "nothing was granted before this push, so it is paid"
        );
        assert_eq!(
            cfg.gate_read_back("studygo", day),
            Some(GateReadBack {
                lifted: true,
                earned_mins: 30,
            }),
            "and it opened the gate on the same push"
        );
        assert_eq!(
            cfg.earn("studygo", day, Some(done(15, 0))),
            Ok(Earn::Refused(Refused::AlreadyGrantedToday)),
            "while the day's latch still holds for the next one"
        );
    }

    /// Recording the bar admits a new source under the same cap a grant does, and no further.
    ///
    /// Otherwise the cap on [`Config::earned`] would be one door with a second beside it: a source
    /// that could not be granted could still open a gate. One under the limit is asserted beside
    /// exactly at it, because the bound is a `<` and a test of one side passes its neighbours.
    #[test]
    fn recording_the_bar_admits_a_new_source_only_below_the_cap() {
        let day = NaiveDate::from_ymd_opt(2026, 9, 16).unwrap();
        let with_others = |count: usize, date: NaiveDate| {
            let mut cfg = Config::default();
            for i in 0..count {
                cfg.earned.insert(
                    format!("other{i}"),
                    EarnedDay {
                        date,
                        minutes: Some(1),
                        bar_met: false,
                    },
                );
            }
            cfg
        };
        let opened = EarnedDay {
            date: day,
            minutes: Some(0),
            bar_met: true,
        };

        let mut room = with_others(MAX_EARNED_SOURCES - 1, day);
        room.note_bar_met("studygo", day);
        assert_eq!(
            room.earned.get("studygo"),
            Some(&opened),
            "one under the cap: admitted, with nothing paid rather than an unmeasured grant"
        );

        let mut full = with_others(MAX_EARNED_SOURCES, day);
        full.note_bar_met("studygo", day);
        assert_eq!(
            full.earned.get("studygo"),
            None,
            "at the cap: a source that could not be granted cannot open a gate either"
        );

        let mut stale = with_others(MAX_EARNED_SOURCES, day.pred_opt().unwrap());
        stale.note_bar_met("studygo", day);
        assert_eq!(
            stale.earned.get("studygo"),
            Some(&opened),
            "yesterday's sources do not count against today"
        );
    }

    /// The gate's arithmetic, which is the whole of what a practice gate is.
    ///
    /// A ceiling on the day, raised by what this provider has already granted, gone once the bar
    /// is met — and gone entirely when the provider is off, removed, or cannot be checked. Every
    /// number here is written as a literal rather than derived from the fixture, because a test
    /// that computes its expectation the way the code does agrees with the code however wrong it
    /// is.
    #[test]
    fn a_gate_caps_the_day_until_its_bar_is_met() {
        let gated = || Provider {
            enabled: true,
            minutes: 0,
            daily_cap_mins: None,
            tiers: vec![Tier {
                questions: 10,
                minutes_practised: 20,
                reward_mins: 16,
            }],
            probe: None,
            remind_every_check: false,
            gate: Some(Gate {
                allowance_mins: 35,
                questions: 15,
                minutes_practised: 30,
            }),
        };
        let day = NaiveDate::from_ymd_opt(2026, 9, 16).unwrap();
        let none = BTreeSet::new();

        let mut cfg = Config::default();
        assert_eq!(
            cfg.gate_cap_mins(day, &none),
            None,
            "no provider, no ceiling — a household with no integration is not gated"
        );

        cfg.providers.insert("studygo".into(), gated());
        assert_eq!(
            cfg.gate_cap_mins(day, &none),
            Some(35),
            "installed and nothing done: the allowance is the day"
        );

        // A lower rung: the minutes it grants extend the GATE, not the day. That is the whole of
        // the difference between this and the shape it replaced, and it is why meeting the bar
        // afterwards lands on the normal day exactly rather than the normal day plus sixteen.
        cfg.earned.insert(
            "studygo".into(),
            EarnedDay {
                date: day,
                minutes: Some(16),
                bar_met: false,
            },
        );
        assert_eq!(
            cfg.gate_cap_mins(day, &none),
            Some(51),
            "35 + the rung he reached"
        );

        // Yesterday's rung buys nothing today.
        assert_eq!(
            cfg.gate_cap_mins(day.succ_opt().unwrap(), &none),
            Some(35),
            "a new day starts at the allowance again"
        );

        // The bar, recorded on the day it was met. The ceiling does not merely rise — it goes.
        cfg.earned.get_mut("studygo").unwrap().bar_met = true;
        assert_eq!(
            cfg.gate_cap_mins(day, &none),
            None,
            "the bar is met, so the parent's own limits are the only limits left"
        );

        // And the three ways a gate stops applying without any work being done.
        cfg.earned.get_mut("studygo").unwrap().bar_met = false;
        cfg.providers.get_mut("studygo").unwrap().enabled = false;
        assert_eq!(
            cfg.gate_cap_mins(day, &none),
            None,
            "switched off means the day is his again — this is the whole point of the switch"
        );
        cfg.providers.get_mut("studygo").unwrap().enabled = true;
        assert_eq!(
            cfg.gate_cap_mins(day, &none),
            Some(51),
            "and back on, it binds again"
        );

        let broken: BTreeSet<String> = ["studygo".to_string()].into_iter().collect();
        assert_eq!(
            cfg.gate_cap_mins(day, &broken),
            None,
            "a check that cannot run is not a child who has not practised: an outage must not \
             cost him his day"
        );

        cfg.providers.remove("studygo");
        assert_eq!(
            cfg.gate_cap_mins(day, &none),
            None,
            "removed takes the ceiling with it"
        );
    }

    /// Two gates, and which one governs.
    #[test]
    fn the_tightest_gate_is_the_one_that_binds() {
        let gate = |allowance| Provider {
            enabled: true,
            minutes: 0,
            daily_cap_mins: None,
            tiers: Vec::new(),
            probe: None,
            remind_every_check: false,
            gate: Some(Gate {
                allowance_mins: allowance,
                questions: 15,
                minutes_practised: 0,
            }),
        };
        let day = NaiveDate::from_ymd_opt(2026, 9, 16).unwrap();
        let none = BTreeSet::new();
        let mut cfg = Config::default();
        cfg.providers.insert("studygo".into(), gate(35));
        cfg.providers.insert("reading".into(), gate(90));
        // The tighter one, not the first one and not the sum: two households' rules both applying
        // means the child is under both, and being under both is being under the smaller.
        assert_eq!(cfg.gate_cap_mins(day, &none), Some(35));
        cfg.providers.get_mut("studygo").unwrap().enabled = false;
        assert_eq!(
            cfg.gate_cap_mins(day, &none),
            Some(90),
            "switching the tighter one off leaves the looser one in force, not nothing"
        );
    }

    /// A gate must have a bar, and the refusal is deliberate rather than advisory.
    #[test]
    fn a_gate_with_no_bar_is_refused_outright() {
        let gate = |allowance, questions, minutes_practised| Gate {
            allowance_mins: allowance,
            questions,
            minutes_practised,
        };
        assert!(gate(35, 15, 30).validate().is_ok());
        assert!(gate(35, 15, 0).validate().is_ok(), "one condition is a bar");
        assert!(gate(35, 0, 30).validate().is_ok());
        assert!(
            gate(35, 0, 0).validate().is_err(),
            "a gate nothing can lift caps the child every day until a parent notices"
        );
        assert!(
            gate(0, 15, 0).validate().is_err(),
            "an allowance of nothing is not a gate"
        );
        assert!(gate(1, 15, 0).validate().is_ok());
        assert!(gate(MAX_GATE_ALLOWANCE_MINS, 15, 0).validate().is_ok());
        assert!(gate(MAX_GATE_ALLOWANCE_MINS + 1, 15, 0).validate().is_err());
        assert!(gate(35, MAX_TIER_QUESTIONS, 0).validate().is_ok());
        assert!(gate(35, MAX_TIER_QUESTIONS + 1, 0).validate().is_err());
        assert!(gate(35, 0, MAX_TIER_MINUTES).validate().is_ok());
        assert!(gate(35, 0, MAX_TIER_MINUTES + 1).validate().is_err());
    }

    /// The settling period is optional and bounded above only. Zero is a real answer — *no
    /// settling period*, which is every config written before the field existed — so the floor is
    /// the absence of a floor, and that is asserted rather than left to the type.
    #[test]
    fn a_settling_period_is_optional_and_bounded_above() {
        let mut p = probe("p", MIN_PROBE_MINS);
        assert!(
            p.validate().is_ok(),
            "zero is no settling period, not a refusal"
        );
        p.first_check_after_mins = 1;
        assert!(p.validate().is_ok());
        p.first_check_after_mins = MAX_SETTLE_MINS;
        assert!(p.validate().is_ok());
        p.first_check_after_mins = MAX_SETTLE_MINS + 1;
        assert!(p.validate().is_err());
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
            remind_every_check: false,
            gate: None,
        };
        assert_eq!(
            serde_json::to_string(&provider).unwrap(),
            r#"{"enabled":true,"minutes":30}"#
        );
        provider.probe = Some(probe("studygo-probe.exe", 15));
        let json = serde_json::to_string(&provider).unwrap();
        assert!(
            json.contains(r#""probe":{"exe":"studygo-probe.exe","every_mins":15}"#),
            "a probe without a settling period writes exactly the two fields it always did, \
             got {json}"
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
        let entry = |date, minutes| EarnedDay {
            date,
            minutes,
            bar_met: false,
        };

        let latched = Provider {
            enabled: true,
            minutes: 30,
            daily_cap_mins: None,
            tiers: Vec::new(),
            probe: None,
            remind_every_check: false,
            gate: None,
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

    /// A gated provider is worth checking until its bar is met, whatever its rungs have paid.
    ///
    /// Under a gate a check can do one more thing than pay: it can open the gate. So *paid in
    /// full* is the wrong place to stop. A ladder whose top rung sits below the bar is paid in full
    /// while the gate is still shut, and a scheduler that stopped there never saw the practice that
    /// would have opened it. The reverse holds once the bar is met: a rung still unclimbed could
    /// only raise a ceiling that no longer applies, so there is nothing left to ask.
    #[test]
    fn a_gated_provider_is_checked_until_its_bar_is_met() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 8).unwrap();
        let yesterday = NaiveDate::from_ymd_opt(2026, 9, 7).unwrap();
        let entry = |date, minutes, bar_met| EarnedDay {
            date,
            minutes,
            bar_met,
        };
        let gated = Provider {
            enabled: true,
            minutes: 30,
            daily_cap_mins: None,
            // One rung, below the bar.
            tiers: vec![Tier {
                questions: 10,
                minutes_practised: 0,
                reward_mins: 16,
            }],
            probe: None,
            remind_every_check: false,
            gate: Some(Gate {
                allowance_mins: 35,
                questions: 15,
                minutes_practised: 0,
            }),
        };

        assert!(!gated.exhausted_for(today, None));
        assert!(
            !gated.exhausted_for(today, Some(&entry(today, Some(16), false))),
            "its only rung is paid and the gate is still shut: the next check is the one that can \
             open it"
        );
        assert!(
            !gated.exhausted_for(today, Some(&entry(today, None, false))),
            "nor does an unmeasured grant from before the gate was added stop the checking"
        );
        assert!(
            gated.exhausted_for(today, Some(&entry(today, Some(0), true))),
            "open with its rung unclimbed: that rung could only raise a ceiling that has gone"
        );
        assert!(!gated.exhausted_for(today, Some(&entry(yesterday, Some(16), true))));

        let capped = Provider {
            daily_cap_mins: Some(16),
            ..gated
        };
        assert!(
            !capped.exhausted_for(today, Some(&entry(today, Some(16), false))),
            "a typed maximum bounds what is paid, not whether the gate can still open"
        );
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
                minutes: None,
                bar_met: false
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
