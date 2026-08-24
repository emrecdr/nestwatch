// Tests for the pure decision and formatting methods in `assets/app.js`.
//
// Why these and not others: the dashboard's script is where the parent's *reading* of the system
// is decided — is enforcement alive, is this build current, how much time was used. Those answers
// are computed by a handful of small functions that need no browser, and until now nothing
// checked any of them. The methods that drive the DOM or the network are deliberately out of
// scope; see harness.js.
//
// Run with `npm test` in `web/`. No test framework is installed — this is `node:test`, which
// ships with Node, so the JavaScript side stays a zero-dependency addition to a project whose
// build has been one CSS compile.

import test from "node:test";
import assert from "node:assert/strict";
import { withState, loadApp } from "./harness.js";

// --- compareVersions -------------------------------------------------------
//
// This decides whether the dashboard tells a parent they are behind. Getting it wrong in the
// "looks current" direction is the bad one: it means a security fix has shipped, the parent is
// told nothing, and the machine stays on the old build. The string comparison this replaced
// ("0.2.10" < "0.2.9") is exactly the bug worth pinning against.

test("compareVersions orders releases numerically, not as strings", () => {
  const app = loadApp();
  assert.equal(app.compareVersions("0.2.3", "0.2.4"), -1, "older is less");
  assert.equal(app.compareVersions("0.2.4", "0.2.3"), 1, "newer is greater");
  assert.equal(app.compareVersions("0.2.3", "0.2.3"), 0, "equal is zero");
  // The case a lexicographic compare gets backwards.
  assert.equal(app.compareVersions("0.2.9", "0.2.10"), -1, "10 comes after 9");
  assert.equal(app.compareVersions("0.9.0", "0.10.0"), -1, "minor 10 comes after 9");
});

test("compareVersions tolerates a leading v and uneven lengths", () => {
  const app = loadApp();
  // GitHub's tag_name carries the v; the local version does not.
  assert.equal(app.compareVersions("v0.2.3", "0.2.3"), 0, "leading v is not a difference");
  assert.equal(app.compareVersions("0.2", "0.2.0"), 0, "missing parts read as zero");
  assert.equal(app.compareVersions("0.2", "0.2.1"), -1, "missing parts still compare");
});

test("compareVersions stays coherent on a tag it cannot fully parse", () => {
  // What `parseInt(n, 10) || 0` is actually for. Without it the subtraction yields NaN, `NaN !==
  // 0` is true, and the function answers "greater" for *both* orderings of the same pair — so a
  // release candidate on GitHub would tell a parent they are behind a version they already have.
  // Asserting the symmetry rather than one blessed answer: the point is that the result is a
  // consistent ordering, not which side of it a suffix lands on.
  const app = loadApp();
  const pairs = [["0.3.0-rc1", "0.3.0"], ["abc", "0.2.3"], ["0.2.3", ""]];
  for (const [a, b] of pairs) {
    const forward = app.compareVersions(a, b);
    const back = app.compareVersions(b, a);
    // Summed rather than negated: the two must cancel, and `assert.equal` compares with
    // `Object.is`, under which `-0` is not `0` — so asserting `forward === -back` fails on the
    // equal case for a reason that has nothing to do with version ordering.
    assert.equal(forward + back, 0, `compareVersions(${a}, ${b}) must be the reverse of (${b}, ${a})`);
  }
});

// --- isEnforcerStale -------------------------------------------------------
//
// `heartbeat.rs` calls a silently dead enforcer "the worst failure this product can have". This
// is the browser half of reporting it, and the `null` case is the one that matters most: it means
// the enforcer never checked in at all, which after a tick's uptime means the loops never
// started. Reading that as "fine" would hide a total enforcement failure behind a normal-looking
// dashboard.

test("isEnforcerStale treats never-reported as stale, not as fresh", () => {
  const app = loadApp();
  assert.equal(app.isEnforcerStale(null), true, "null means it never checked in");
});

test("isEnforcerStale draws the line at the shared threshold", () => {
  const app = loadApp();
  assert.equal(app.isEnforcerStale(0), false, "just beat");
  assert.equal(app.isEnforcerStale(149), false, "inside the window");
  assert.equal(app.isEnforcerStale(150), false, "exactly at the threshold is not yet stale");
  assert.equal(app.isEnforcerStale(151), true, "past it");
  assert.equal(app.isEnforcerStale(86_400), true, "a day later");
});

test("isEnforcerStale treats an absent age as stale, not as healthy", () => {
  // The regression this pins: `=== null` let `undefined` fall through both arms and return
  // false. The initial `today` literal has no `enforcer_age_secs` key, and `loadList` swallows a
  // failed fetch — so a load that never succeeded reported healthy enforcement for a service the
  // page could not reach.
  const app = loadApp();
  assert.equal(app.isEnforcerStale(undefined), true, "no answer is not a good answer");
});

// The two properties below are in tension, and the bug was fixing one by breaking the other.
// Both are asserted so neither can be traded away silently.

test("stEnforcementStale stays quiet until the first attempt has finished", () => {
  const app = withState({ todayAsked: false, today: {} });
  assert.equal(app.stEnforcementStale(), false, "a page that has not loaded yet is not evidence");
});

test("stEnforcementStale warns once an attempt finished without an age", () => {
  // Reached when the fetch failed: loadToday marks the attempt done, `today` keeps its initial
  // literal, and that literal has no age in it.
  const app = withState({ todayAsked: true, today: {} });
  assert.equal(app.stEnforcementStale(), true, "asked, and the answer never came");
});

test("stEnforcementStale follows the age once one is known", () => {
  assert.equal(withState({ todayAsked: true, today: { enforcer_age_secs: 10 } }).stEnforcementStale(), false);
  assert.equal(withState({ todayAsked: true, today: { enforcer_age_secs: 999 } }).stEnforcementStale(), true);
  assert.equal(
    withState({ todayAsked: true, today: { enforcer_age_secs: null } }).stEnforcementStale(),
    true,
    "an explicit null means the enforcer never checked in",
  );
});

test("enforcementDetail says which of the three things went wrong", () => {
  // The NaN case is the one that shipped for as long as the banner was reachable at all: the
  // markup divided an absent age by 60 and rendered "No check-in for NaN min."
  assert.equal(
    withState({ today: {} }).enforcementDetail(),
    "The dashboard could not reach the service to ask.",
    "no answer at all",
  );
  assert.equal(
    withState({ today: { enforcer_age_secs: null } }).enforcementDetail(),
    "The background checks haven't reported yet.",
    "answered, but the enforcer has never ticked",
  );
  assert.equal(
    withState({ today: { enforcer_age_secs: 600 } }).enforcementDetail(),
    "No check-in for 10 min.",
    "answered, and it is late",
  );
});

test("enforcementDetail never renders NaN, whatever the age is", () => {
  for (const age of [undefined, null, 0, 59, 600, 86_400]) {
    const text = withState({ today: { enforcer_age_secs: age } }).enforcementDetail();
    assert.doesNotMatch(text, /NaN|undefined|\[object/, `age ${String(age)} produced: ${text}`);
  }
});

// --- resolveTimeRequest ----------------------------------------------------
//
// Not a pure method — it posts — so what is tested is only which endpoint it decides on. That is
// the part that went wrong: the parameter was a boolean, so any truthy value approved, and the
// string "deny" is truthy. Approving a request a parent denied is the wrong direction to fail in.

/** An app whose fetch records the URLs it was given and always reports failure. */
function appRecordingFetch() {
  const calls = [];
  const app = loadApp({ fetch: async (url) => (calls.push(url), { ok: false }) });
  app.toast = () => {};
  return { app, calls };
}

test("resolveTimeRequest denies anything that is not exactly an approval", async () => {
  const { app, calls } = appRecordingFetch();
  const decisions = ["deny", "", null, undefined, "nonsense", true, 1, {}];
  for (const decision of decisions) {
    await app.resolveTimeRequest("req-1", decision);
  }
  assert.equal(calls.length, decisions.length, "every call reached the endpoint");
  const approved = calls.filter((u) => u.endsWith("/approve"));
  assert.deepEqual(approved, [], `these decisions approved: ${JSON.stringify(approved)}`);
});

test("resolveTimeRequest approves on the literal approval", async () => {
  const { app, calls } = appRecordingFetch();
  await app.resolveTimeRequest("req-1", "approve");
  assert.equal(calls.length, 1);
  assert.ok(calls[0].endsWith("/req-1/approve"), `posted to ${calls[0]}`);
});

// --- stBarPct --------------------------------------------------------------
//
// The chart's heights, including the 3% floor that app.js documents at length and tells you not
// to "correct" to zero. A measured-zero day is a real observation and needs a mark you can hover;
// at height 0 it renders no area and its tooltip becomes unreachable. That is a behaviour a
// future tidy-up would plausibly undo, so it is pinned here rather than only described.

const days = (...mins) => ({ screentime: { days: mins.map((m) => ({ minutes_used: m })) } });

test("stBarPct keeps a measured-zero day visible and distinct", () => {
  const app = withState(days(0, 60));
  assert.equal(app.stBarPct({ minutes_used: 0 }), 3, "measured zero gets a visible floor");
  assert.notEqual(
    app.stBarPct({ minutes_used: 0 }),
    app.stBarPct({ minutes_used: null }),
    "a measured zero must not look like a day that was never measured",
  );
});

test("stBarPct fills the column for a day that was never measured", () => {
  assert.equal(withState(days(30)).stBarPct({ minutes_used: null }), 100, "the hatch fills it");
});

test("stBarPct scales against the tallest measured day", () => {
  const app = withState(days(0, 50, 100));
  assert.equal(app.stBarPct({ minutes_used: 100 }), 100, "the peak is full height");
  assert.equal(app.stBarPct({ minutes_used: 50 }), 50, "half the peak is half height");
});

test("stBarPct never returns a height too small to see or hover", () => {
  // One enormous day would otherwise round every small day to 0%.
  const app = withState(days(1, 100_000));
  assert.ok(app.stBarPct({ minutes_used: 1 }) >= 4, "a small non-zero day stays visible");
});

test("stBarPct survives a period with no measured days at all", () => {
  // `Math.max(1, ...)` is what stops a division by zero here; a fresh install is exactly this.
  const app = withState({ screentime: { days: [{ minutes_used: null }, { minutes_used: null }] } });
  assert.equal(app.stBarPct({ minutes_used: null }), 100);
  assert.ok(Number.isFinite(app.stBarPct({ minutes_used: 5 })), "no NaN or Infinity");
});

// --- stDayLabel / stBarTitle ----------------------------------------------
//
// One phrasing shared by the chart tooltip and the day-by-day table. They each formatted it
// themselves once and had already drifted apart in the same commit, which is why it was hoisted.

test("stDayLabel says what was used, against what budget, and whether it went over", () => {
  const app = loadApp();
  assert.equal(app.stDayLabel({ minutes_used: 90, budget: 60, over_budget: true }), "90 of 60 min (over budget)");
  assert.equal(app.stDayLabel({ minutes_used: 30, budget: 60, over_budget: false }), "30 of 60 min");
  assert.equal(app.stDayLabel({ minutes_used: 30, budget: null, over_budget: false }), "30 min", "no budget set");
  assert.equal(app.stDayLabel({ minutes_used: null }), "not measured");
});

test("stBarTitle explains an unmeasured day rather than showing a blank", () => {
  const app = loadApp();
  const title = app.stBarTitle({ date: "2026-08-01", minutes_used: null });
  assert.match(title, /2026-08-01/, "names the day");
  assert.match(title, /not measured/, "says why there is no figure");
  assert.equal(
    app.stBarTitle({ date: "2026-08-02", minutes_used: 30, budget: 60, over_budget: false }),
    "2026-08-02: 30 of 60 min",
    "a measured day reuses stDayLabel verbatim",
  );
});

// --- stBarClass ------------------------------------------------------------
//
// These class names exist nowhere else in the project: Tailwind finds them by scanning this file,
// which is why `@source` includes *.js. A rename here with no CSS rebuild renders unstyled bars.

test("stBarClass distinguishes the three states the legend promises", () => {
  const app = loadApp();
  assert.equal(app.stBarClass({ minutes_used: null }), "st-nodata");
  assert.equal(app.stBarClass({ minutes_used: 90, over_budget: true }), "bg-error");
  assert.equal(app.stBarClass({ minutes_used: 30, over_budget: false }), "bg-primary");
  assert.equal(app.stBarClass({ minutes_used: 0, over_budget: false }), "bg-primary", "measured zero is measured");
});

// --- anyRulesSet -----------------------------------------------------------
//
// Drives the "no limits are set" empty state. A false positive here tells a parent limits are in
// place when nothing is enforced.

const NO_RULES = {
  rules: { daily_budget_mins: 0, budget_by_weekday: null, blocklist: [] },
  appLimitRows: [],
  groupRows: [],
};

test("anyRulesSet is false when nothing is configured", () => {
  assert.equal(withState(NO_RULES).anyRulesSet(), false);
});

test("anyRulesSet ignores blank and zero entries", () => {
  const app = withState({
    ...NO_RULES,
    rules: { ...NO_RULES.rules, blocklist: ["", "   "] },
    appLimitRows: [{ name: "", mins: 0 }, { name: "game.exe", mins: 0 }],
    groupRows: [{ appsText: "  ", limit_mins: 30 }],
  });
  assert.equal(app.anyRulesSet(), false, "a half-filled row is not a rule");
});

test("anyRulesSet notices each kind of rule on its own", () => {
  const set = (over) => withState({ ...NO_RULES, ...over }).anyRulesSet();
  assert.equal(set({ rules: { ...NO_RULES.rules, daily_budget_mins: 60 } }), true, "daily budget");
  assert.equal(set({ rules: { ...NO_RULES.rules, budget_by_weekday: [0, 0, 0, 0, 0, 0, 90] } }), true, "per-weekday");
  assert.equal(set({ rules: { ...NO_RULES.rules, blocklist: ["game.exe"] } }), true, "blocklist");
  assert.equal(set({ appLimitRows: [{ name: "game.exe", mins: 30 }] }), true, "per-app limit");
  assert.equal(set({ groupRows: [{ appsText: "a.exe", limit_mins: 30 }] }), true, "group limit");
});

// --- fmtBytes --------------------------------------------------------------

test("fmtBytes scales units and shows a dash for nothing", () => {
  const app = loadApp();
  assert.equal(app.fmtBytes(0), "—", "zero is not '0 B'");
  assert.equal(app.fmtBytes(null), "—");
  assert.equal(app.fmtBytes(512), "512 B", "whole bytes carry no decimal");
  assert.equal(app.fmtBytes(1536), "1.5 KB");
  assert.equal(app.fmtBytes(5 * 1024 ** 3), "5.0 GB", "stops at GB");
});
