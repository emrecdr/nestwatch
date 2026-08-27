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

// --- loadList --------------------------------------------------------------
//
// Every list on the dashboard arrives through this one method, so its failure mode is the whole
// dashboard's failure mode. Three callers pass an error message that could never fire: only a
// *thrown* exception reached the toast, and an HTTP error status does not throw. A 500 left the
// field at whatever it already held and said nothing — which on /api/time-requests renders
// identically to a child who has asked for nothing, on a screen whose entire job is to show that
// they did.

test("loadList surfaces an HTTP failure, not only a thrown one", async () => {
  const toasts = [];
  const app = loadApp({ fetch: async () => ({ ok: false, status: 500 }) });
  app.toast = (msg, kind) => toasts.push([msg, kind]);

  await app.loadList("/api/usage", "usage", null, "Failed to load usage history");

  assert.deepEqual(
    toasts,
    [["Failed to load usage history", "error"]],
    "a server error must reach the parent, not leave the card looking merely empty",
  );
});

test("loadList leaves the field alone when the server errors", async () => {
  const app = loadApp({ fetch: async () => ({ ok: false, status: 503 }) });
  app.toast = () => {};
  app.usage = [{ date: "2026-08-01" }];

  await app.loadList("/api/usage", "usage", null, "Failed to load usage history");

  assert.deepEqual(app.usage, [{ date: "2026-08-01" }], "a failed refresh must not blank the card");
});

// 401 is not a failure to report — it is the session ending, and the login screen says so already.
test("loadList treats 401 as a logout rather than an error to toast", async () => {
  const toasts = [];
  const app = loadApp({ fetch: async () => ({ ok: false, status: 401 }) });
  app.toast = (msg, kind) => toasts.push([msg, kind]);
  app.authed = true;

  await app.loadList("/api/usage", "usage", null, "Failed to load usage history");

  assert.equal(app.authed, false, "an expired session signs the parent out");
  assert.deepEqual(toasts, [], "a toast on top of the login screen is noise, not information");
});

// The spinner must stop whichever way the request ended, or a failed load reads as a load that is
// still going and the parent waits for something that is never coming.
test("loadList clears its loading flag after a failure", async () => {
  const app = loadApp({ fetch: async () => ({ ok: false, status: 500 }) });
  app.toast = () => {};

  await app.loadList("/api/usage", "usage", "loadingUsage", "Failed to load usage history");

  assert.equal(app.loadingUsage, false, "the spinner must not outlive the request");
});

// --- loadToday -------------------------------------------------------------
//
// `todayAsked` means "an attempt finished", including a failed one — which is exactly right for
// reaching the staleness warning, and exactly wrong for deciding whether the figures beside it can
// be trusted. Before a load succeeds, `today` is a literal of zeroes, and the card reads them out
// as fact: "0 min used today. No daily limit set — tracking only." A parent seeing that on a
// dashboard that could not reach the service is being told something nobody measured.
//
// `day` cannot stand in for this: a successful response carries `day: null` on a machine whose
// enforcer has not yet written a tally, so it means "no data recorded", not "no data received".

const okJson = (body) => async () => ({ ok: true, json: async () => body });

test("loadToday records the attempt but does not trust figures it never received", async () => {
  const app = loadApp({ fetch: async () => ({ ok: false, status: 500 }) });
  app.toast = () => {};

  await app.loadToday();

  assert.equal(app.todayAsked, true, "the attempt is recorded, so the staleness warning can fire");
  assert.equal(app.today, null, "a failed load must not license a confident zero");
});

test("loadToday trusts figures it did receive", async () => {
  const app = loadApp({ fetch: okJson({ day: "2026-08-24", budget_mins: 0, used_mins: 12 }) });
  app.toast = () => {};

  await app.loadToday();

  assert.notEqual(app.today, null);
  assert.equal(app.today.used_mins, 12);
});

// A response that legitimately has nothing recorded yet is still a response.
test("loadToday trusts a successful reply that carries no tally", async () => {
  const app = loadApp({ fetch: okJson({ day: null, budget_mins: 0, used_mins: 0 }) });
  app.toast = () => {};

  await app.loadToday();

  assert.notEqual(app.today, null, "'nothing recorded' is a measurement; 'no reply' is not");
});

// Once real figures are on screen, a failed refresh must not blank them — they are stale, not
// absent, and staleness already has its own warning.
test("figures already received survive a later failed refresh", async () => {
  let healthy = true;
  const app = loadApp({
    fetch: async () =>
      healthy ? { ok: true, json: async () => ({ used_mins: 12 }) } : { ok: false, status: 500 },
  });
  app.toast = () => {};

  await app.loadToday();
  healthy = false;
  await app.loadToday();

  assert.notEqual(app.today, null, "stale is not the same as never measured");
});

// Signing out has to forget the figures too, not just the session. `logout` already clears the
// process list, so the intent is established; `today` was simply missed. Left behind, the next
// sign-in renders the previous session's numbers as current until a fetch replaces them — and if
// the tab sat overnight, "current" means yesterday.
test("logging out forgets today's figures, not just the session", async () => {
  const app = loadApp({ fetch: okJson({ used_mins: 42, budget_mins: 60 }) });
  app.toast = () => {};

  await app.loadToday();
  assert.notEqual(app.today, null, "precondition: figures were loaded");

  await app.logout();

  assert.equal(app.authed, false);
  assert.equal(
    app.today,
    null,
    "the next sign-in must not present the last one's numbers as today's",
  );
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
  // `bg-error st-over`, not `bg-error`: the colour alone measured 1.22 against `bg-primary` in this
  // theme, so over budget carries a texture too. See the accessibility tests at the end of
  // this file — this assertion is the exact-string half, those are the property half.
  assert.equal(app.stBarClass({ minutes_used: 90, over_budget: true }), "bg-error st-over");
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

// The rule `stDayFor` falls back on when no day is pinned, stated once. Worth its own test
// separate from stDayFor's, because a change to it — days no longer sorted oldest-first, or
// "carries data" becoming a flag rather than a non-empty list — breaks every panel at once,
// and a failure here says which of the two halves moved.
test("stRecentDayWith returns the newest day whose named list has entries", () => {
  const app = withState({
    screentime: {
      days: [
        { date: "2026-08-12", apps: [{ name: "a.exe", minutes: 1 }] },
        { date: "2026-08-13", apps: [{ name: "b.exe", minutes: 2 }] },
        { date: "2026-08-14", apps: [] },
      ],
    },
  });

  // Two days carry data, so this pins the *pick* as well as the filter. With only one day left
  // after filtering, `days[0]` and `days[days.length - 1]` are the same element and reversing the
  // order is invisible — which is what this fixture used to be, and it passed either way.
  assert.equal(app.stRecentDayWith("apps").date, "2026-08-13", "newest with data, not newest");
  assert.equal(app.stRecentDayWith("focused"), null, "a key no day carries is absent, not empty");
});

// --- More-time requests: three states, not two -------------------------------------------------
//
// The card and its badge were both gated on `timeRequests.length > 0`, so a failed fetch left the
// field at `[]` and removed every surface the parent could have noticed anything on. It rendered
// identically to a child who had asked for nothing. A pending request has a person waiting behind
// it, which makes this the one panel where an invisible failure has a cost beyond the display.

test("showRequests hides the card once the service answers and nothing is pending", () => {
  const a = loadApp();
  a.timeRequests = [];
  a.requestsAsked = true;
  assert.equal(a.showRequests(), false, "an answered empty queue is just clutter");
});

test("showRequests shows the card when the answer never arrived", () => {
  const a = loadApp();
  a.timeRequests = null;
  a.requestsAsked = true;
  assert.equal(
    a.showRequests(),
    true,
    "a service we could not reach must not render as a quiet evening",
  );
});

test("showRequests stays quiet before the first attempt finishes", () => {
  const a = loadApp();
  a.timeRequests = null;
  a.requestsAsked = false;
  assert.equal(
    a.showRequests(),
    false,
    "the unknown state must not flash on every page load before the first response",
  );
});

test("showRequests shows the card when something is pending", () => {
  const a = loadApp();
  a.timeRequests = [{ id: "r1", minutes: 20 }];
  a.requestsAsked = true;
  assert.equal(a.showRequests(), true);
});

test("the badge reports a count only when it has one", () => {
  const a = loadApp();

  a.timeRequests = null;
  assert.equal(a.requestCount(), null, "unknown is not a number");
  assert.equal(a.requestBadge(), "requests ?", "a badge is an assertion; do not assert zero");

  a.timeRequests = [];
  assert.equal(a.requestCount(), 0);

  a.timeRequests = [{ id: "r1" }];
  assert.equal(a.requestBadge(), "1 request", "singular");

  a.timeRequests = [{ id: "r1" }, { id: "r2" }];
  assert.equal(a.requestBadge(), "2 requests", "plural");
});

// --- Signing out forgets the session ------------------------------------------------------------

test("resetSessionData clears every field holding fetched data", () => {
  const a = loadApp();
  a.processes = [{ pid: 1 }];
  a.today = { used_mins: 90 };
  a.todayAsked = true;
  a.timeRequests = [{ id: "r1" }];
  a.requestsAsked = true;
  a.audit = [{ event: "login" }];
  a.usage = [{ event: "lock" }];
  a.codes = [{ code: "AAAA1111" }];
  a.routines = ["Homework"];
  a.screentime = { days: [{ date: "2026-08-24" }], total_mins: 400, measured_days: 1,
                   daily_avg_mins: 400, prev_total_mins: 300, change_pct: 33 };

  a.resetSessionData();

  // Lengths and values, never `deepEqual` against a literal built here. `app.js` is evaluated in a
  // `vm` context, so an array it creates has that realm's `Array.prototype` and
  // `deepStrictEqual` rejects it as a different type — "same structure but not reference-equal"
  // against two empty arrays, which reads as a bug in the code rather than in the comparison.
  assert.equal(a.processes.length, 0);
  assert.equal(a.today, null);
  assert.equal(a.todayAsked, false);
  assert.equal(a.timeRequests, null, "unknown again, not an empty queue");
  assert.equal(a.requestsAsked, false);
  assert.equal(a.audit.length, 0);
  assert.equal(a.usage.length, 0);
  assert.equal(a.codes.length, 0);
  assert.equal(a.routines.length, 0);
  assert.equal(a.screentime.days.length, 0);
  assert.equal(a.screentime.total_mins, 0);
  assert.equal(a.screentime.daily_avg_mins, null);
  // The windowed totals too. These were the fields that exposed the duplicated literal: added to
  // the initial state and missed in the reset, they would have survived a sign-out alone.
  assert.equal(a.screentime.app_totals.length, 0);
  assert.equal(a.screentime.focus_totals.length, 0);
  assert.equal(a.screentime.page_totals.length, 0);
  assert.equal(a.screentime.group_totals.length, 0);
});

test("a signed-out component reports nothing pending rather than nothing known", () => {
  const a = loadApp();
  a.timeRequests = [{ id: "r1" }];
  a.requestsAsked = true;

  a.resetSessionData();

  assert.equal(
    a.showRequests(),
    false,
    "after a sign-out the previous session's requests must not still be on screen",
  );
});

// --- Curfew day picker -------------------------------------------------------------------------
//
// `Days::includes` on the Rust side treats an all-false selector as *every* day, which is right for
// the data model — an omitted `days` field must mean daily. The markup offers seven live checkboxes,
// so a parent can clear them all and get the opposite of what clearing them looks like it means. The
// consequence runs the wrong way: deselect Friday and Saturday to lift bedtime for the weekend,
// clear the last box doing it, and the PC shuts down on exactly those two evenings.

test("clearing every day says the window still applies every day", () => {
  const a = loadApp();
  const w = { days: { mon: false, tue: false, wed: false, thu: false, fri: false, sat: false, sun: false } };
  assert.equal(a.windowDayLabel(w), "Applies: every day");
  assert.equal(a.windowDaysImplicit(w), true, "and it is the accidental kind, worth flagging");
});

test("ticking every day also says every day, but not accidentally", () => {
  const a = loadApp();
  const w = { days: { mon: true, tue: true, wed: true, thu: true, fri: true, sat: true, sun: true } };
  assert.equal(a.windowDayLabel(w), "Applies: every day");
  assert.equal(
    a.windowDaysImplicit(w),
    false,
    "all seven ticked is the same outcome by deliberate choice — do not warn about it",
  );
});

test("a partial selection is named in weekday order, not click order", () => {
  const a = loadApp();
  // Deliberately out of order in the object, to prove the output follows dayOptions.
  const w = { days: { sat: true, tue: true, mon: false, wed: false, thu: false, fri: false, sun: false } };
  assert.equal(a.windowDayLabel(w), "Applies: Tue, Sat");
  assert.equal(a.windowDaysImplicit(w), false);
});

test("a window with no days object at all is treated as every day, not as a crash", () => {
  const a = loadApp();
  // Config written by an older build, or hand-edited: `days` absent entirely. Rust deserializes
  // that to all-false, which means daily — the UI must agree rather than throw.
  assert.equal(a.windowDayLabel({}), "Applies: every day");
  assert.equal(a.windowDaysImplicit({}), true);
});

test("every weekday has a two-character caption and a full accessible name", () => {
  const a = loadApp();
  assert.equal(a.dayOptions.length, 7);
  const shorts = a.dayOptions.map((d) => d.short);
  assert.equal(new Set(shorts).size, 7, "two letters, so no two days share a caption");
  for (const d of a.dayOptions) {
    assert.equal(d.short.length, 2, `${d.full} caption must be two characters`);
    assert.ok(d.full.length > 2, `${d.key} needs a full name for the screen reader`);
    assert.ok(d.full.startsWith(d.short), `${d.full} and ${d.short} must agree`);
  }
});

// --- Report window and day selection ------------------------------------------------------------

const REPORT = {
  days: [
    { date: "2026-08-20", measured: true, minutes_used: 100, apps: [{ name: "a.exe", minutes: 10 }], focused: [], pages: [] },
    { date: "2026-08-21", measured: true, minutes_used: 50, apps: [], focused: [{ name: "b.exe", minutes: 5 }], pages: [] },
    { date: "2026-08-22", measured: false, minutes_used: null, apps: [], focused: [], pages: [] },
  ],
  total_mins: 150, measured_days: 2, daily_avg_mins: 75, prev_total_mins: null, change_pct: null,
};

test("with nothing pinned each panel still picks its own newest day with data", () => {
  const a = loadApp();
  a.screentime = REPORT;
  assert.equal(a.stDayFor("apps").date, "2026-08-20", "only the 20th has apps");
  assert.equal(a.stDayFor("focused").date, "2026-08-21", "only the 21st has focus");
  assert.equal(a.stDayFor("pages"), null, "no day has browser time");
});

test("pinning a day overrides all three panels, including the ones with nothing", () => {
  const a = loadApp();
  a.screentime = REPORT;
  a.toggleStDay("2026-08-21");

  assert.equal(a.stDayFor("apps").date, "2026-08-21", "a pin must win over each panel's own choice");
  assert.equal(a.stDayFor("focused").date, "2026-08-21");
  assert.equal(a.stDayFor("pages").date, "2026-08-21");
  assert.equal(a.stDayHas("apps"), false, "the 21st has no apps, and must say so");
  assert.equal(a.stDayHas("focused"), true);
});

test("choosing the pinned day again releases it", () => {
  const a = loadApp();
  a.screentime = REPORT;
  a.toggleStDay("2026-08-21");
  a.toggleStDay("2026-08-21");
  assert.equal(a.stPinned, null);
  assert.equal(a.stDayFor("apps").date, "2026-08-20", "back to each panel's own newest");
});

test("pinning an unmeasured day is allowed and reports nothing rather than hiding", () => {
  const a = loadApp();
  a.screentime = REPORT;
  a.toggleStDay("2026-08-22");
  assert.ok(a.stDayFor("apps"), "the panel stays on screen so the heading can name the day");
  assert.equal(a.stDayHas("apps"), false);
});

test("the heading only claims 'most recent with data' when that is why the day was chosen", () => {
  const a = loadApp();
  a.screentime = REPORT;

  assert.match(a.stHeading("apps"), /most recent day with data \(2026-08-20\)/);
  a.toggleStDay("2026-08-21");
  assert.equal(a.stHeading("apps"), "Apps running — 2026-08-21");
  assert.ok(
    !a.stHeading("apps").includes("most recent"),
    "a pinned day is not 'the most recent with data' and must not say so",
  );
});

test("changing the window drops a pin that may not exist in it", () => {
  const a = loadApp({ fetch: async () => ({ ok: true, status: 200, json: async () => REPORT }) });
  a.screentime = REPORT;
  a.toggleStDay("2026-08-21");
  a.setStDays(7);
  assert.equal(a.stDays, 7);
  assert.equal(a.stPinned, null, "a pinned date off the end of the new window names nothing");
});

test("the report request carries the chosen window", async () => {
  const seen = [];
  const a = loadApp({
    fetch: async (url) => {
      seen.push(url);
      return { ok: true, status: 200, json: async () => REPORT };
    },
  });

  await a.loadScreentime();
  assert.equal(seen[0], "/api/screentime?days=30", "the default is sent explicitly, not implied");

  await a.setStDays(90);
  assert.equal(seen[1], "/api/screentime?days=90");
});

// --- Display names and durations ----------------------------------------------------------------

test("known executables get a name a parent recognises", () => {
  const a = loadApp();
  assert.equal(a.appLabel("RobloxPlayerBeta.exe"), "Roblox", "case-insensitive, and the big one");
  assert.equal(a.appLabel("chrome.exe"), "Google Chrome");
  assert.equal(a.appLabel("javaw.exe"), "Minecraft (Java)");
});

test("unknown executables lose the extension rather than the name", () => {
  const a = loadApp();
  assert.equal(a.appLabel("SomeGame.exe"), "SomeGame");
  assert.equal(a.appLabel("weird-thing"), "weird-thing", "no extension is left alone");
  assert.equal(a.appLabel(""), "", "and nothing stays nothing rather than becoming undefined");
});

test("durations read as time once they pass an hour", () => {
  const a = loadApp();
  assert.equal(a.fmtDuration(0), "0 min");
  assert.equal(a.fmtDuration(59), "59 min");
  assert.equal(a.fmtDuration(60), "1 h", "an exact hour drops the empty minutes");
  assert.equal(a.fmtDuration(90), "1 h 30 min");
  assert.equal(a.fmtDuration(1847), "30 h 47 min", "the four-digit case this exists for");
});

// --- The tab title is the only alert this product can offer ------------------------------------
//
// Web Push needs an external service; the Badging API needs an installed app, which MOBILE-APP.md
// rules out because a home-screen app does not inherit the certificate exception; the Notifications
// API needs a secure context and whether an accepted self-signed cert on a private IP counts is
// unverified. The title needs no permission and no external anything.

test("the title carries a pending count and drops it at zero", () => {
  const a = loadApp();
  assert.equal(a.titleFor(0), "Nestwatch");
  assert.equal(a.titleFor(1), "(1) Nestwatch");
  assert.equal(a.titleFor(7), "(7) Nestwatch");
});

test("an unknown count is not titled as zero", () => {
  const a = loadApp();
  assert.equal(
    a.titleFor(null),
    "Nestwatch",
    "a service we could not reach must not be advertised as quiet...",
  );
  assert.notEqual(a.titleFor(null), "(0) Nestwatch", "...nor as a confident zero");
  assert.equal(a.titleFor(undefined), "Nestwatch", "before the first load, same");
});

test("syncTitle is a no-op without a document rather than a crash", () => {
  const a = loadApp();
  a.timeRequests = [{ id: "r1" }];
  assert.doesNotThrow(() => a.syncTitle(), "the component is evaluated where no document exists");
});

// --- At a glance --------------------------------------------------------------------------------
//
// Three answers, each with an *unknown* state distinct from its good and bad ones. That third state
// is what every earlier version of this page got wrong: an unreachable service rendered as a healthy
// enforcer, as zero minutes used, and as nothing waiting.

test("enforcement reads as unknown before the first check, not as healthy", () => {
  const a = loadApp();
  assert.equal(a.glanceEnforcement().tone, "muted");
  assert.match(a.glanceEnforcement().text, /checking/);
});

test("enforcement reads as broken when the service cannot be reached", () => {
  const a = loadApp();
  a.todayAsked = true;
  a.today = null; // the fetch failed, so no age came back
  assert.equal(a.glanceEnforcement().tone, "bad");
  assert.match(a.glanceEnforcement().text, /may not be running/);
});

test("a paused enforcer is called paused, not broken and not healthy", () => {
  const a = loadApp();
  a.todayAsked = true;
  a.today = { enabled: false, enforcer_age_secs: 5, budget_mins: 0, used_mins: 0 };
  assert.equal(a.glanceEnforcement().tone, "warn");
  assert.match(a.glanceEnforcement().text, /paused/);
});

test("a live enforcer reads as running", () => {
  const a = loadApp();
  a.todayAsked = true;
  a.today = { enabled: true, enforcer_age_secs: 5, budget_mins: 60, used_mins: 10 };
  assert.equal(a.glanceEnforcement().tone, "good");
});

test("today is not known before anything loads, rather than zero", () => {
  const a = loadApp();
  assert.equal(a.glanceToday().tone, "muted");
  assert.match(a.glanceToday().text, /not known/);
  assert.ok(!a.glanceToday().text.includes("0 min"), "must not assert a zero it does not have");
});

test("today warns as the budget runs down and turns bad when it is gone", () => {
  const a = loadApp();
  a.today = { budget_mins: 60, used_mins: 50, remaining_mins: 10 };
  assert.equal(a.glanceToday().tone, "warn");
  a.today = { budget_mins: 60, used_mins: 60, remaining_mins: 0 };
  assert.equal(a.glanceToday().tone, "bad");
  a.today = { budget_mins: 60, used_mins: 5, remaining_mins: 55 };
  assert.equal(a.glanceToday().tone, "good");
});

test("with no budget today reports use without pretending there is a limit", () => {
  const a = loadApp();
  a.today = { budget_mins: 0, used_mins: 125, remaining_mins: null };
  assert.match(a.glanceToday().text, /2 h 5 min used, no limit set/);
});

test("waiting requests are distinguished from none and from not knowing", () => {
  const a = loadApp();

  assert.match(a.glanceRequests().text, /checking/, "before the first attempt");

  a.requestsAsked = true;
  a.timeRequests = null;
  assert.equal(a.glanceRequests().tone, "warn");
  assert.match(a.glanceRequests().text, /not known/, "a failed check is not 'nothing waiting'");

  a.timeRequests = [];
  assert.match(a.glanceRequests().text, /Nothing waiting/);

  a.timeRequests = [{ id: "r1" }];
  assert.match(a.glanceRequests().text, /^1 request waiting$/);

  a.timeRequests = [{ id: "r1" }, { id: "r2" }];
  assert.match(a.glanceRequests().text, /^2 requests waiting$/);
});

// --- The curfew switch says what it is doing ----------------------------------------------------
//
// It was a bare toggle carrying only an `aria-label`. That satisfies the screen-reader guard and
// leaves a sighted parent guessing at the control that decides whether a child's PC powers itself
// off at night — a gap an automated name check cannot see, and did not.

test("the curfew switch reads Off when it is off", () => {
  const a = loadApp();
  a.curfew = { enabled: false, start: "22:00", end: "07:00", windows: [] };
  assert.equal(a.curfewStateLabel(), "Off");
});

test("the curfew switch reads On when there are hours that can fire", () => {
  const a = loadApp();
  a.curfew = { enabled: true, start: "22:00", end: "07:00", windows: [] };
  assert.equal(a.curfewStateLabel(), "On");
});

test("a curfew switched on with no usable hours says so rather than claiming to be on", () => {
  const a = loadApp();
  // start === end is never active — `is_within` treats it as an empty window.
  a.curfew = { enabled: true, start: "22:00", end: "22:00", windows: [] };
  assert.equal(a.curfewStateLabel(), "On — no hours set");
  assert.equal(a.curfewHasHours(), false);
});

test("per-day schedules decide it when they exist, not the simple pair", () => {
  const a = loadApp();
  // The simple pair is usable, but windows take over when present — and this one cannot fire.
  a.curfew = { enabled: true, start: "22:00", end: "07:00",
               windows: [{ start: "23:00", end: "23:00", days: {} }] };
  assert.equal(a.curfewStateLabel(), "On — no hours set");

  a.curfew.windows.push({ start: "21:00", end: "07:00", days: {} });
  assert.equal(a.curfewStateLabel(), "On", "one usable window is enough");
});

// --- Theme ---------------------------------------------------------------------------------------
//
// Three states, and "auto" is not a synonym for "light". The stylesheet ships light on :root and dim
// under prefers-color-scheme, so *no attribute* is what follows the device — setting one to any
// value pins a theme, which is the bug the daisyUI cleanup removed. That distinction is the whole
// reason this is three buttons and not a two-way toggle.

test("a fresh component follows the device", () => {
  const a = loadApp();
  assert.equal(a.theme, "auto", "no stored choice means follow the device");
});

test("choosing a theme records it without needing storage to work", () => {
  const a = loadApp();
  // The harness has no localStorage at all, which is also what a blocked private mode looks like.
  assert.doesNotThrow(() => a.setTheme("dark"));
  assert.equal(a.theme, "dark", "the choice still applies to this page");
  assert.doesNotThrow(() => a.setTheme("auto"));
  assert.equal(a.theme, "auto");
});

test("a stored choice is honoured, and anything unrecognised falls back to auto", () => {
  const store = {};
  const fake = {
    getItem: (k) => (k in store ? store[k] : null),
    setItem: (k, v) => { store[k] = v; },
    removeItem: (k) => { delete store[k]; },
  };

  store["nw-theme"] = "dark";
  assert.equal(loadApp({ localStorage: fake }).theme, "dark");

  store["nw-theme"] = "light";
  assert.equal(loadApp({ localStorage: fake }).theme, "light");

  // Anything else — a hand-edited value, a key from a future version — must not pin a theme.
  store["nw-theme"] = "dim";
  assert.equal(loadApp({ localStorage: fake }).theme, "auto", "unrecognised is not a theme");

  delete store["nw-theme"];
  assert.equal(loadApp({ localStorage: fake }).theme, "auto");
});

test("choosing auto clears the stored override rather than storing the word", () => {
  const store = { "nw-theme": "dark" };
  const fake = {
    getItem: (k) => (k in store ? store[k] : null),
    setItem: (k, v) => { store[k] = v; },
    removeItem: (k) => { delete store[k]; },
  };
  const a = loadApp({ localStorage: fake });
  assert.equal(a.theme, "dark");

  a.setTheme("auto");
  assert.equal("nw-theme" in store, false, "a stored 'auto' would pin light on a dark device");
});

test("every theme button carries a name, not only a glyph", () => {
  const a = loadApp();
  assert.equal(a.themeOptions.length, 3);
  // Joined, not `deepEqual`: the component is built inside a `vm` realm, so an array it creates
  // has that realm's prototype and `deepStrictEqual` rejects it as a different type. Documented on
  // the resetSessionData test above — and walked into again here, which is the argument for the
  // comment existing at all.
  assert.equal(a.themeOptions.map((t) => t.key).join(","), "auto,light,dark");
  for (const t of a.themeOptions) {
    assert.ok(t.label && t.label.length > 2, `${t.key} needs a readable label, got "${t.label}"`);
    assert.ok(t.glyph && t.glyph.length > 0, `${t.key} needs something to show`);
  }
  assert.equal(a.themeOptions[1].glyph, "☀️", "light is the sun");
  assert.equal(a.themeOptions[2].glyph, "🌙", "dark is the moon");
});

// --- the live view's tier, staleness and cancellation ----------------------
//
// The tier decides bandwidth, and getting it wrong is invisible: a live stream silently running at
// full resolution looks identical to one running correctly, just slower and far more expensive on
// the child's machine. Staleness is the opposite failure — visible only by its absence, because a
// frozen live view and a motionless child are the same picture.

/** An app whose fetch records the URLs asked for and returns a blob, so captures "succeed". */
function appRecordingShots(overrides = {}) {
  const calls = [];
  const app = loadApp({
    fetch: async (url) => (calls.push(url), { ok: true, status: 200, blob: async () => ({}) }),
    URL: { createObjectURL: () => "blob:x", revokeObjectURL: () => {} },
    AbortController: function () {
      this.signal = {};
      this.abort = () => { this.aborted = true; };
    },
    // A fuller document than most tests need: supplying one at all makes app.js's
    // `typeof document !== "undefined"` guard true, which switches on the theme init that reaches
    // `document.documentElement`. A stub with only `hidden` throws there.
    document: {
      hidden: false,
      documentElement: { setAttribute() {}, removeAttribute() {} },
      addEventListener() {},
    },
    matchMedia: () => ({ matches: false, addEventListener() {} }),
    setInterval: () => 1,
    clearInterval: () => {},
    ...overrides,
  });
  app.toast = () => {};
  return { app, calls };
}

test("a live frame asks for the preview tier and a human's click asks for full", async () => {
  const { app, calls } = appRecordingShots();

  await app.takeScreenshot();                    // the "Take screenshot" button
  await app.takeScreenshot(false, "full");       // the same request, asked for explicitly
  await app.takeScreenshot(true, "preview");     // the live timer

  assert.equal(calls.length, 3);
  assert.ok(calls[0].endsWith("tier=full"), `button asked for ${calls[0]}`);
  assert.ok(calls[1].endsWith("tier=full"), `refresh asked for ${calls[1]}`);
  assert.ok(calls[2].endsWith("tier=preview"), `live timer asked for ${calls[2]}`);
});

test("the tier is its own argument, not a synonym for how loud the failure is", async () => {
  const { app, calls } = appRecordingShots();

  // The two combinations no current call site uses. They are the whole point: `silent` says
  // whether a failure raises a toast, `tier` says how many pixels to ask for, and they are
  // independent. Written because the test above did NOT catch deriving the tier from `silent` —
  // all three of its cases happened to have the two agree, so a mutation that collapsed them
  // passed. A test whose cases are perfectly correlated cannot see the correlation being assumed.
  await app.takeScreenshot(true, "full");
  await app.takeScreenshot(false, "preview");

  assert.ok(
    calls[0].endsWith("tier=full"),
    `a silent capture must still be able to ask for full, got ${calls[0]}`,
  );
  assert.ok(
    calls[1].endsWith("tier=preview"),
    `a loud capture must still be able to ask for a preview, got ${calls[1]}`,
  );
});

test("switching Live on takes a full frame first, so a failure surfaces at once", async () => {
  const { app, calls } = appRecordingShots();
  app.startAutoRefresh();
  // startAutoRefresh kicks off an un-awaited capture; let it land.
  await new Promise((r) => setTimeout(r, 0));
  assert.equal(calls.length, 1, "exactly one frame on switch-on");
  assert.ok(
    calls[0].endsWith("tier=full"),
    "the first frame must be full — mapping the tier onto `silent` would make it a preview and " +
      "the picture would visibly soften one tick later",
  );
});

test("a failed live frame marks the picture stale instead of silently keeping it", async () => {
  const { app } = appRecordingShots({ fetch: async () => ({ ok: false, status: 500 }) });
  app.shotAt = Date.now();

  await app.takeScreenshot(true, "preview");
  assert.equal(app.shotStale, true, "a silent failure must still be visible on the page");
  assert.match(app.shotAge(), /not updating/, `said: ${app.shotAge()}`);
});

test("a successful frame clears a stale marker", async () => {
  const { app } = appRecordingShots();
  app.shotStale = true;
  await app.takeScreenshot(true, "preview");
  assert.equal(app.shotStale, false);
  assert.match(app.shotAge(), /^updated /, `said: ${app.shotAge()}`);
});

test("turning Live off aborts the capture already in flight", () => {
  const { app } = appRecordingShots();
  let aborted = false;
  app._shotAbort = { abort: () => { aborted = true; } };
  app.autoRefresh = true;

  app.stopAutoRefresh();
  assert.equal(aborted, true, "a frame arriving after the parent said stop must be dropped");
  assert.equal(app._shotAbort, null);
  assert.equal(app.autoRefresh, false);
});

test("an aborted capture is not reported as a failure", async () => {
  const err = new Error("aborted");
  err.name = "AbortError";
  const { app } = appRecordingShots({ fetch: async () => { throw err; } });
  let toasted = 0;
  app.toast = () => { toasted += 1; };

  await app.takeScreenshot(true, "preview");
  assert.equal(app.shotStale, false, "the parent stopped it; nothing is stale");
  await app.takeScreenshot();
  assert.equal(toasted, 0, "cancelling must not look like a broken screenshot");
});

test("the age line reads plainly at every scale and never leaks a raw number", () => {
  const { app } = appRecordingShots();
  assert.equal(app.shotAge(), "", "nothing captured yet says nothing");

  const base = 1_700_000_000_000;
  app.shotAt = base;
  for (const [elapsed, want] of [[0, "updated 0s ago"], [4000, "updated 4s ago"],
                                 [65_000, "updated 1m 5s ago"]]) {
    app.now = base + elapsed;
    assert.equal(app.shotAge(), want);
  }
  app.shotStale = true;
  app.now = base + 4000;
  assert.equal(app.shotAge(), "not updating — last frame 4s ago");
  assert.equal(app.shotAgeClass(), "text-error", "a stalled view must be visible, not just legible");
});

test("every offered cadence is slower than the one that shipped, and the default is not the fastest", () => {
  const app = loadApp();
  const offered = app.refreshOptions.map((o) => o.ms);
  assert.equal(offered.join(","), "2000,5000,15000");
  assert.ok(
    offered.includes(app._refreshMs),
    `the default ${app._refreshMs} must be one of the offered cadences`,
  );
  assert.ok(
    app._refreshMs > Math.min(...offered),
    "defaulting to the fastest cadence would keep the most expensive setting as the only one a " +
      "parent ever sees",
  );
});

test("choosing a cadence re-arms the timer without buying another capture", async () => {
  const cleared = [];
  const { app, calls } = appRecordingShots({
    setInterval: () => cleared.length + 100,
    clearInterval: (id) => cleared.push(id),
  });
  app.startAutoRefresh();
  // Let the opening capture settle. Until it does, `_shotBusy` is still true and would swallow a
  // second capture on its own — which would make the assertion below pass for the wrong reason.
  for (let i = 0; i < 20 && app._shotBusy; i++) await Promise.resolve();
  assert.equal(app._shotBusy, false, "the opening capture must settle before the cadence click");

  const first = app._shotTimer;
  const afterStart = calls.length;
  app.setRefreshMs(15000);

  assert.equal(app._refreshMs, 15000);
  assert.ok(cleared.includes(first), "the previous interval must be cleared, not left running");
  assert.equal(app.autoRefresh, true, "changing cadence must not switch the live view off");
  assert.equal(
    calls.length,
    afterStart,
    "clicking a slower cadence to make the live view cheaper must not itself commission a " +
      "capture — and the one it used to commission was full tier, the most expensive of all",
  );
});

test("expanding a frame that is already full does not capture the desktop again", async () => {
  const { app, calls } = appRecordingShots();

  await app.takeScreenshot();                 // the "Take screenshot" button: full tier
  assert.equal(calls.length, 1);
  assert.ok(calls[0].endsWith("tier=full"));

  app.openShotFull();
  assert.equal(app.shotFull, true, "the overlay must still open");
  assert.equal(
    calls.length,
    1,
    "the browser already holds a full frame; expanding it must not spawn a second helper, " +
      "capture the whole desktop and encode it again for bytes it has",
  );
});

test("expanding a live preview still refetches, or the overlay stretches 960x540 to fill it", async () => {
  const { app, calls } = appRecordingShots();

  await app.takeScreenshot(true, "preview");  // a live timer frame
  assert.equal(calls.length, 1);

  app.openShotFull();
  assert.equal(calls.length, 2, "a preview must be replaced by a full frame when opened large");
  assert.ok(calls[1].endsWith("tier=full"), `expand asked for ${calls[1]}`);
});

test("the fifteen-minute auto-stop does not freeze the age line into a lie", async () => {
  const { app } = appRecordingShots();
  await app.takeScreenshot();
  assert.ok(app.shotAt, "a frame must have landed for there to be an age to show");
  assert.notEqual(app._clockTimer, null, "a frame on screen needs a clock to age it");

  // Exactly what the auto-stop does when `_liveUntil` passes.
  app.stopAutoRefresh();

  assert.equal(app.autoRefresh, false, "the live view really has stopped");
  assert.notEqual(
    app._clockTimer,
    null,
    "the picture is still on screen and still getting older. Freezing the line at 'updated 4s " +
      "ago' is not silence, it is a confident wrong answer — and it is the exact failure the age " +
      "line was added to prevent, arriving through the one stop path that is not an error",
  );

  app.now = app.shotAt + 3_600_000;
  assert.match(app.shotAge(), /updated 60m/, "the age must keep counting after the stop");
});

test("signing out forgets how old the picture was", () => {
  const { app } = appRecordingShots();
  app.shotAt = Date.now();
  app.shotStale = true;
  app.syncTitle = () => {};

  app.resetSessionData();
  assert.equal(app.shotAt, null, "'updated 3s ago' must not outlive the session it belonged to");
  assert.equal(app.shotStale, false);
  assert.equal(app.shotTier, null, "nor may the tier it was captured at");
  assert.equal(
    app._clockTimer,
    null,
    "the age clock outlives the Live toggle now, so signing out is the one place that must stop " +
      "it — otherwise it ticks forever against a frame that is gone",
  );
});

// --- the first-seen notice -------------------------------------------------
//
// Three states the UI must keep apart: `null` = the report could not tell, empty = it checked and
// nothing was new, non-empty = the notice. Merging the first two makes a working check look broken
// on the first day the watcher ran; merging the last two shows an empty panel every quiet day,
// and a notice that is always present stops being read.

// `loadApp()` already starts with `screentime: emptyScreentime()` — the real shape, from the one
// place that defines it. The hand-written stand-in that used to sit here was a third copy of that
// shape and was already a field behind: it omitted `first_seen`, which is the field these very
// tests are about, and only survived because `Object.assign` put it back.
function appWithFirstSeen(first_seen) {
  const a = loadApp();
  a.screentime.first_seen = first_seen;
  return a;
}

test("a report that cannot tell shows nothing at all", () => {
  const a = appWithFirstSeen(null);
  assert.equal(a.firstSeen, null);
  assert.equal(a.showFirstSeen, false, "'could not tell' must not render as 'nothing new'");
  assert.equal(a.showFirstSeenQuiet, false, "nor as a quiet day");
  assert.equal(a.showFirstSeenStopped, false);
});

// Three states were carried all the way from `screentime.rs` and two of them rendered as the same
// blank space, which is the failure the `Option` was introduced to prevent arriving one layer past
// where it is guarded. The quiet day now reaches the reader as words — plainly, not as the warning
// panel, since a warning that appears every day stops being read.
test("a quiet day says so, rather than looking like a check that never ran", () => {
  const a = appWithFirstSeen({
    date: "2026-08-19", apps: [], count: 0, baseline_days: 12, baseline_overflow: false,
  });
  assert.equal(a.showFirstSeen, false, "an empty notice is not worth the warning panel");
  assert.equal(a.showFirstSeenQuiet, true, "but it must reach the reader as words");
  assert.match(a.firstSeenQuietNote(), /nothing new/i);
  assert.match(a.firstSeenQuietNote(), /12 earlier days/, "carrying the strength of the claim");
});

// The one case the cap exists for. A child cycling executable names to keep every day looking new
// pushes the baseline past its limit, and the check abandons the answer — correctly, since a
// truncated baseline would name familiar programs as new. What was wrong is that giving up looked
// exactly like a fresh install, on the dashboard and in the log both.
test("a check that gave up says so, instead of looking like nothing happened", () => {
  const a = appWithFirstSeen({
    date: "2026-08-19", apps: [], count: 0, baseline_days: 3, baseline_overflow: true,
  });
  assert.equal(a.showFirstSeenStopped, true, "the parent has to learn the check stopped");
  assert.equal(a.showFirstSeenQuiet, false, "a stopped check is not a quiet day");
  assert.equal(a.showFirstSeen, false, "and it must name nothing, or it is a false alarm");
  assert.match(a.firstSeenStoppedNote(), /stopped/i);
});

test("a new app is announced with the strength of the claim beside it", () => {
  const a = appWithFirstSeen({
    date: "2026-08-19",
    apps: [{ name: "discord.exe", minutes: 42 }],
    count: 1, baseline_days: 12,
  });
  assert.equal(a.showFirstSeen, true);
  assert.equal(a.firstSeenHeading(), "1 new app");
  assert.match(a.firstSeenNote(), /2026-08-19/);
  assert.match(a.firstSeenNote(), /12 earlier days/,
    "a parent cannot weigh 'new' without knowing what it is new against");
});

test("one day of history is described in the singular, not '1 earlier days'", () => {
  const a = appWithFirstSeen({ date: "2026-08-19", apps: [{ name: "x.exe", minutes: 1 }],
                               count: 1, baseline_days: 1 });
  assert.match(a.firstSeenNote(), /1 earlier day\b/);
  assert.doesNotMatch(a.firstSeenNote(), /1 earlier days/);
});

test("a capped list says so, so the notice cannot understate what happened", () => {
  const apps = Array.from({ length: 8 }, (_, i) => ({ name: "n" + i + ".exe", minutes: 10 }));
  const a = appWithFirstSeen({ date: "2026-08-19", apps, count: 13, baseline_days: 30 });
  const h = a.firstSeenHeading();
  assert.match(h, /13 new apps/, `must lead with the true total: ${h}`);
  assert.match(h, /showing the 8/, `must admit the list is capped: ${h}`);
});

test("the plural is right at every count", () => {
  const mk = (count, shown) => appWithFirstSeen({
    date: "2026-08-19", count, baseline_days: 5,
    apps: Array.from({ length: shown }, (_, i) => ({ name: i + ".exe", minutes: 1 })),
  });
  assert.equal(mk(1, 1).firstSeenHeading(), "1 new app");
  assert.equal(mk(2, 2).firstSeenHeading(), "2 new apps");
  assert.doesNotMatch(mk(1, 1).firstSeenHeading(), /apps/);
});

test("a missing screentime object does not throw", () => {
  const a = loadApp();
  a.screentime = null;
  assert.equal(a.firstSeen, null);
  assert.equal(a.showFirstSeen, false);
  assert.equal(a.firstSeenNote(), "");
  assert.equal(a.firstSeenHeading(), "");
});

// --- today's usage timeline ------------------------------------------------
//
// The only view that answers *when* rather than *how much*. Its whole difficulty is pairing, and
// pairing has exactly one dangerous failure: joining a start to a stop that belongs to a different
// session. That draws a bar from an afternoon crash through to bedtime and calls it use — which is
// the defect this feature was blocked on (OPEN-FINDINGS O36), reappearing one layer up.

/** Events as `/api/usage` delivers them: newest first, RFC 3339 with a local-time offset. */
function ev(kind, hhmm, day = "2026-08-25") {
  const [h, m] = hhmm.split(":");
  const d = new Date(`${day}T${h}:${m}:00`);           // local time, matching what the API renders
  return { event: kind, ts: d.toISOString() };
}
function timelineOf(list, nowMin = 23 * 60) {
  return loadApp().dayTimeline(list, "2026-08-25", nowMin);
}
// Both forward the optional day. Dropping it silently is not hypothetical: the first version of
// these took only the time, so `STOP("10:00", "2026-08-24")` quietly built a *today* event and the
// other-days test failed against correct code.
const START = (t, day) => ev("session_start", t, day);
const STOP = (t, day) => ev("session_stop", t, day);

test("a paired session becomes one span", () => {
  const spans = timelineOf([STOP("10:30"), START("09:00")]);   // newest first, as delivered
  assert.equal(spans.length, 1);
  assert.deepEqual({ ...spans[0] }, { from: 540, to: 630, kind: "use" });
});

test("events arrive newest-first and are still ordered correctly", () => {
  const spans = timelineOf([STOP("22:00"), START("21:00"), STOP("10:00"), START("09:00")]);
  assert.equal(spans.map((s) => s.from).join(","), "540,1260", "oldest span first");
});

test("a start with no stop before the next start never spans the gap", () => {
  // The enforcer died at some point after 09:00. Its end is unknowable.
  const spans = timelineOf([START("20:00"), START("09:00")]);
  const orphan = spans.find((s) => s.kind === "unknown");
  assert.ok(orphan, `expected an unknown-end span, got ${JSON.stringify(spans)}`);
  assert.equal(orphan.from, orphan.to,
    "an unknown end must have no duration — stretching it to the next start is the original bug");
  assert.equal(orphan.from, 540);
});

test("a still-running session is bounded by now, not by midnight", () => {
  const spans = timelineOf([START("21:00")], 22 * 60 + 30);
  assert.equal(spans.length, 1);
  assert.equal(spans[0].kind, "live");
  assert.equal(spans[0].to, 1350, "ends at 22:30, not 24:00");
});

test("a stop with nothing open is discarded rather than inventing a span", () => {
  // Its start was yesterday; this axis is one day wide.
  // `.length`, never `deepEqual`: the component is built in a `vm` realm, so an array it creates
  // has that realm's prototype and `deepStrictEqual` rejects it as a different type even when
  // empty. Documented on the harness and on two earlier tests — and walked into again here, which
  // is the argument for the comment existing at all.
  assert.equal(timelineOf([STOP("07:00")]).length, 0);
});

test("events from other days are excluded", () => {
  const spans = timelineOf([STOP("10:00", "2026-08-24"), START("09:00", "2026-08-24")]);
  assert.equal(spans.length, 0, "yesterday's session is not today's timeline");
});

test("non-session events are ignored", () => {
  const noise = ev("budget_lock", "12:00");
  const spans = timelineOf([noise, STOP("10:00"), START("09:00")]);
  assert.equal(spans.length, 1, "a lock is not a session boundary");
});

test("unparseable or missing timestamps are skipped, not crashed on", () => {
  const junk = [{ event: "session_start", ts: "not a date" }, { event: "session_stop" },
                STOP("10:00"), START("09:00")];
  const spans = timelineOf(junk);
  assert.equal(spans.length, 1);
  assert.equal(spans[0].kind, "use");
});

test("an empty or absent history yields no timeline rather than throwing", () => {
  assert.equal(timelineOf([]).length, 0);
  assert.equal(timelineOf(null).length, 0);
  assert.equal(timelineOf(undefined).length, 0);
});

test("geometry is a percentage of the day and never collapses to invisible", () => {
  const a = loadApp();
  assert.match(a.spanStyle({ from: 0, to: 720, kind: "use" }), /left:0\.000%;width:50\.000%/);
  const hairline = a.spanStyle({ from: 600, to: 600, kind: "unknown" });
  assert.match(hairline, /width:0\.400%/, "a zero-width marker would be invisible");
  const tiny = a.spanStyle({ from: 600, to: 601, kind: "use" });
  assert.match(tiny, /width:0\.400%/, "a one-minute session must still be seen");
});

test("no span kind is told apart by colour alone", () => {
  const a = loadApp();
  // Measured in Chrome on the dark theme: bg-primary vs bg-success is a 1.01 contrast ratio —
  // identical luminance, differing only in hue, and green-against-teal is the textbook
  // deuteranopia confusion pair. So every kind must carry a non-colour cue as well.
  //
  // `live` gets a ring; `unknown` gets no width (`spanStyle`), because its duration is unknown
  // rather than short. `use` is the plain case both are read against.
  assert.match(a.spanClass({ kind: "live" }), /\bring-/,
    "the live span must be marked by shape, not only by being a different green");
  assert.equal(a.spanStyle({ from: 600, to: 600, kind: "unknown" }).includes("width:0.400%"), true,
    "an unknown end must stay a hairline — width is its distinguishing cue");
  assert.doesNotMatch(a.spanClass({ kind: "use" }), /\bring-/,
    "the ordinary case must not also be ringed, or the cue distinguishes nothing");
});

test("every span kind is visually and verbally distinct", () => {
  const a = loadApp();
  const kinds = ["use", "live", "unknown"];
  const classes = kinds.map((kind) => a.spanClass({ kind }));
  assert.equal(new Set(classes).size, 3, `kinds must not share a colour: ${classes}`);
  // An unknown end must say so, not imply a duration a parent would read as fact.
  assert.match(a.spanLabel({ from: 540, to: 540, kind: "unknown" }), /end unknown/);
  assert.match(a.spanLabel({ from: 540, to: 600, kind: "use" }), /09:00 to 10:00/);
  assert.match(a.spanLabel({ from: 540, to: 600, kind: "live" }), /still on/);
});

test("the axis is labelled every four hours across the whole day", () => {
  const t = loadApp().dayTicks;
  assert.equal(t.map((x) => x.label).join(" "), "00:00 04:00 08:00 12:00 16:00 20:00");
  assert.equal(t[0].pct, 0);
  assert.ok(t[t.length - 1].pct < 100, "the last tick must sit inside the axis");
});

// --- the over-budget bar ----------------------------------------------------
//
// `bg-error` against `bg-primary` measures 1.22 in this theme — near-identical luminance differing
// mostly in hue, and green-against-salmon is a red-green confusion pair. It was the only over/under
// cue on the bar (the ring in the markup marks the *pinned* day). Screen readers were always told;
// the reader with neither channel was the sighted colour-blind parent.

test("an over-budget bar is textured, not only coloured", () => {
  const a = loadApp();
  const over = a.stBarClass({ minutes_used: 200, over_budget: true });
  const under = a.stBarClass({ minutes_used: 100, over_budget: false });

  assert.match(over, /\bst-over\b/, "over budget must carry a texture, not just bg-error");
  assert.doesNotMatch(under, /\bst-over\b/, "the ordinary case must be plain, or the cue means nothing");
  assert.match(over, /\bbg-error\b/, "the colour stays as reinforcement for those who can see it");
});

test("the three bar states are mutually distinguishable without colour", () => {
  const a = loadApp();
  const states = {
    nodata: a.stBarClass({ minutes_used: null }),
    over: a.stBarClass({ minutes_used: 200, over_budget: true }),
    under: a.stBarClass({ minutes_used: 100, over_budget: false }),
  };
  // "not measured" and "over budget" both use a pattern, so they must not use the *same* one —
  // .st-nodata stripes at 45deg and .st-over at 135deg, deliberately mirrored.
  assert.notEqual(states.nodata, states.over);
  assert.match(states.nodata, /\bst-nodata\b/);
  assert.doesNotMatch(states.nodata, /\bst-over\b/, "an unmeasured day is not an over-budget day");
  assert.equal(new Set(Object.values(states)).size, 3);
});

test("every swatch in the chart's key is painted by the same method as the bars", () => {
  const a = loadApp();
  // A real day row for each state the chart can draw.
  const bars = {
    "within budget": { date: "2026-08-01", minutes_used: 42, over_budget: false },
    "over budget": { date: "2026-08-02", minutes_used: 300, over_budget: true },
    "not measured": { date: "2026-08-03", minutes_used: null },
  };

  const key = a.stBarKey;
  assert.equal(key.length, 3, "a key that omits a state the chart can draw is a puzzle");

  for (const k of key) {
    assert.ok(bars[k.label], `the key names a state this test does not know about: ${k.label}`);
    assert.equal(
      a.stBarClass(k),
      a.stBarClass(bars[k.label]),
      `the "${k.label}" swatch does not look like the bars it claims to explain. This is the ` +
        "exact failure the key used to have: the markup spelled the three class strings out by " +
        "hand, so adding the .st-over texture repainted the bars and left the swatch flat",
    );
  }

  assert.equal(
    new Set(key.map((k) => a.stBarClass(k))).size,
    3,
    "two swatches that render identically explain nothing",
  );
});

test("the text cue that screen readers rely on is still there", () => {
  const a = loadApp();
  // The texture is for sighted colour-blind readers; this is the channel that already worked and
  // must not regress while fixing the other one.
  assert.match(a.stDayLabel({ minutes_used: 200, budget: 120, over_budget: true }), /over budget/);
  assert.doesNotMatch(a.stDayLabel({ minutes_used: 100, budget: 120, over_budget: false }), /over budget/);
});

// --- game portals recognised from the page title -------------------------
//
// The product question is "an evening of Roblox or an evening of homework". Native Roblox is already
// exact, by process name. Browser portals were undifferentiated page titles until this.
test("game portals are recognised from the page title alone", () => {
  const a = loadApp();
  assert.equal(a.gamePortal("Poki - Free Online Games"), "Poki");
  assert.equal(a.gamePortal("CrazyGames - Free Online Games on CrazyGames"), "CrazyGames");
  assert.equal(a.gamePortal("Coolmath Games - Free Online Math Games"), "Coolmath Games");
  assert.equal(a.gamePortal("Roblox - now.gg"), "now.gg", "the cloud player, not the native game");
  assert.equal(a.gamePortal("POKI - FREE ONLINE GAMES"), "Poki", "case-insensitive");
});

// The whole feature is a *label* on data already collected. A false positive is worse than a miss,
// because the parent acts on it — so the table matches distinctive brand tokens, never the word
// "games", which appears in news headlines and shop pages that are nobody's business.
test("the portal table stays quiet on pages that merely mention games", () => {
  const a = loadApp();
  assert.equal(a.gamePortal("The 50 best games of 2026 | The Guardian"), "");
  assert.equal(a.gamePortal("Khan Academy | Free Online Courses"), "");
  assert.equal(a.gamePortal("Buy games - Microsoft Store"), "");
});

// Absence of a match means "nothing was recognised", never "no game sites were visited" — the same
// null-vs-zero rule the rest of this product keeps. Empty input must not become "undefined".
test("an unrecognised or absent title is not labelled anything", () => {
  const a = loadApp();
  assert.equal(a.gamePortal("Some Random Page"), "");
  assert.equal(a.gamePortal(""), "");
  assert.equal(a.gamePortal(null), "");
  assert.equal(a.gamePortal(undefined), "");
});

// --- capture concurrency ----------------------------------------------------
//
// One guard, `if (this._shotBusy) return;`, was written to stop the LIVE TIMER stacking captures:
// the helper can take ~15s while the timer fires every 2s. But it sits on the shared function, so
// it also dropped human clicks — Take screenshot, Expand and the modal's Refresh — whenever a
// silent preview happened to be in flight. `:disabled="loadingShot"` does not cover that, because
// `loadingShot` is only set for non-silent captures, so the buttons looked enabled, accepted the
// click and did nothing. With a 15s worst case against a 2s cadence that is the common case.
//
// The fix has to distinguish the two callers rather than delete the guard, and it has to survive
// the response that was already in flight: aborting a fetch closes the connection but the helper
// keeps capturing, so a superseded reply can still arrive and must not win.

/**
 * A capture-capable app whose fetches are resolved by hand.
 *
 * One factory for every capture test. They need the same six stubs and differ on only two axes —
 * whether the mock fetch honours the abort signal, and which tier the response names — so this
 * block stood in four copies while these tests were being written. A stub the app later needs
 * would have had to be added in four places, and could have drifted in one.
 *
 * `honourAbort: false` is not a shortcut, it is the whole point of one test. Aborting a fetch
 * closes the connection but cannot call the capture helper back, so a reply already on the wire
 * still resolves. Ignoring the signal is the only way to reach the generation guard and prove it
 * is not dead code that the abort path happens to hide.
 */
function captureHarness({ honourAbort = true, servedTier = null, hidden = null } = {}) {
  const calls = [];
  const pending = [];
  const revoked = [];
  let blobs = 0;
  const globals = {
    fetch: (url, opts) => {
      const signal = opts && opts.signal;
      calls.push({ url, signal });
      return new Promise((resolve, reject) => {
        pending.push(resolve);
        if (honourAbort && signal) {
          signal.addEventListener("abort", () => {
            const e = new Error("aborted");
            e.name = "AbortError";
            reject(e);
          });
        }
      });
    },
    AbortController,
    URL: {
      createObjectURL: () => "blob:" + ++blobs,
      revokeObjectURL: (u) => revoked.push(u),
    },
    Date,
    setInterval: () => 0,
    clearInterval: () => {},
  };
  // `hidden` left null supplies NO `document`, which is what every capture test wants:
  // `takeScreenshot` never touches it, and `applyTheme` guards on `typeof document === "undefined"`
  // at load time, so a *partial* stub defeats that guard and fails the whole file before a single
  // assertion runs. Passing a boolean supplies a FULL one instead — `applyTheme` reaches
  // `documentElement` and `matchMedia`, so both have to be there. Same set, and the same reason, as
  // `appRecordingShots` above.
  if (hidden !== null) {
    globals.document = {
      hidden,
      documentElement: { setAttribute() {}, removeAttribute() {} },
      addEventListener() {},
    };
    globals.matchMedia = () => ({ matches: false, addEventListener() {} });
  }
  const app = loadApp(globals);
  app.toast = () => {};
  app.startShotClock = () => {};
  /**
   * Resolve the i-th capture that reached the endpoint; a later request has the higher index.
   *
   * `status` covers the exits that are not a frame — 401 when the session ended mid-capture.
   */
  const settle = (i = 0, status = 200) => {
    pending[i]({
      status,
      ok: status === 200,
      blob: async () => ({}),
      headers: { get: (n) => (n.toLowerCase() === "x-shot-tier" ? servedTier : null) },
    });
    return new Promise((r) => setImmediate(r));
  };
  return { app, calls, revoked, settle };
}

test("a parent's click is not discarded while a live preview is in flight", async () => {
  const { app, calls } = captureHarness();
  app.takeScreenshot(true, "preview"); // the live timer, still in flight
  app.takeScreenshot(); // the parent presses Take screenshot
  await new Promise((r) => setImmediate(r));
  assert.equal(calls.length, 2, "the human capture must reach the endpoint, not silently return");
});

test("the live timer still does not stack captures on itself", async () => {
  const { app, calls } = captureHarness();
  app.takeScreenshot(true, "preview");
  app.takeScreenshot(true, "preview");
  await new Promise((r) => setImmediate(r));
  assert.equal(calls.length, 1, "a silent tick must skip while one is in flight — this is why the guard exists");
});

test("a superseded capture cannot overwrite the frame that replaced it", async () => {
  const { app, calls, settle } = captureHarness();
  app.takeScreenshot(true, "preview");
  app.takeScreenshot(false, "full");
  await new Promise((r) => setImmediate(r));
  assert.equal(calls.length, 2);
  await settle(1); // the human frame lands
  const winner = app.shotUrl;
  await settle(0); // the superseded preview arrives late
  assert.equal(app.shotUrl, winner, "a late reply from a superseded capture must not replace a newer frame");
  assert.equal(app.shotTier, "full", "nor downgrade the recorded tier");
});

test("a superseded capture does not clear the loading state of the one that replaced it", async () => {
  const { app, settle } = captureHarness();
  app.takeScreenshot(true, "preview");
  app.takeScreenshot(false, "full");
  await new Promise((r) => setImmediate(r));
  await settle(0); // the superseded preview resolves first
  assert.equal(app.loadingShot, true, "the human capture is still running, so its spinner must stay");
  assert.equal(app._shotBusy, true, "and the busy flag must still belong to it");
});

// The abort covers the common path, but not the one the generation guard is actually for: a reply
// whose body was already on the wire when abort() landed still resolves. The helper had finished
// capturing either way — aborting cannot call it back. `honourAbort: false` ignores the signal on
// purpose, which is the only way to prove the guard is not dead code the abort path hides.

test("a reply that outlives its abort still cannot win", async () => {
  const { app, calls, settle } = captureHarness({ honourAbort: false });
  app.takeScreenshot(true, "preview");
  app.takeScreenshot(false, "full");
  await new Promise((r) => setImmediate(r));
  assert.equal(calls.length, 2);
  await settle(1); // the newer, human capture lands
  const winner = app.shotUrl;
  await settle(0); // the superseded preview resolves anyway, ignoring its abort
  assert.equal(app.shotUrl, winner, "the older frame must not replace the newer one");
  assert.equal(app.shotTier, "full", "nor relabel it as a preview");
  assert.equal(app._shotBusy, false, "and the current capture had already finished cleanly");
});

// --- which tier is actually on screen ---------------------------------------
//
// The client used to record the tier it *asked* for. `ShotTier::from_arg` maps unknown and absent
// alike to full, so a typo in the query string returns a full frame on a two-second timer while the
// client believes it is showing previews — and `openShotFull` skips its refetch when it thinks the
// frame is already full, so the mislabel decides whether the parent gets a sharp picture.


test("the recorded tier is the one the server served, not the one requested", async () => {
  const { app, settle } = captureHarness({ servedTier: "full" });
  app.takeScreenshot(true, "preview"); // asked for preview...
  await new Promise((r) => setImmediate(r));
  await settle(); // ...but the server says it served full
  assert.equal(app.shotTier, "full", "record what arrived, so a silent full-tier stream is visible");
});

test("a response that names no tier falls back to the one requested", async () => {
  const { app, settle } = captureHarness();
  app.takeScreenshot(true, "preview");
  await new Promise((r) => setImmediate(r));
  await settle();
  assert.equal(app.shotTier, "preview", "an older service without the header must still work");
});

// --- which tier a live frame should be -------------------------------------
//
// The two tiers were introduced on an explicit promise: a parent who wants to READ something can
// still get a full-resolution frame. That promise held only while Live was off. With it on, the
// timer overwrote the overlay's sharp frame within one refresh interval — and Live being on is
// precisely the state a parent is in when they press Expand, so the promise failed exactly where
// it was needed. The tier was decided by who asked for the frame; it should follow which surface
// is showing it.

test("live frames follow the surface being viewed, not whoever asked for the first one", () => {
  const a = loadApp();
  a.shotFull = false;
  assert.equal(a.liveTier(), "preview", "the thumbnail cannot show more than a preview holds");
  a.shotFull = true;
  assert.equal(a.liveTier(), "full", "the overlay exists so a parent can read what is on screen");
});

test("a live tick asks for the tier the visible surface needs", () => {
  const asked = [];
  const a = loadApp();
  a.takeScreenshot = (silent, tier) => asked.push([silent, tier]);
  a._liveUntil = Number.MAX_SAFE_INTEGER;

  a.shotFull = false;
  a._liveTick();
  a.shotFull = true;
  a._liveTick();

  assert.deepEqual(asked, [[true, "preview"], [true, "full"]], "silent both times, sharp only when the overlay is up");
});

test("a live tick past its own deadline stops instead of capturing", () => {
  const asked = [];
  const a = loadApp();
  a.takeScreenshot = (...x) => asked.push(x);
  let stopped = false;
  a.stopAutoRefresh = () => { stopped = true; };
  a._liveUntil = 0;
  a._liveTick();
  assert.equal(stopped, true, "the unattended-tab cap must still fire");
  assert.deepEqual(asked, [], "and it must not capture on the way out");
});

// A session that ends mid-capture is the one exit that never released the frame it was holding.
// Every other one does — superseded, replaced, signed out. It mattered little while a frame was
// ~25 KiB; with the full-size view open it is megabyte-scale, and the tab can live for hours on a
// login screen afterwards.
test("a session that ends mid-capture does not leak the frame it was holding", async () => {
  const { app, revoked, settle } = captureHarness();

  app.takeScreenshot();
  await new Promise((r) => setImmediate(r));
  await settle();
  const held = app.shotUrl;
  assert.ok(held, "a frame must be on screen before the session ends");

  // The next capture finds the session gone.
  app.takeScreenshot();
  await new Promise((r) => setImmediate(r));
  await settle(1, 401);

  assert.equal(app.authed, false, "a 401 must sign the dashboard out");
  assert.ok(
    revoked.includes(held),
    "the frame it was holding must be released — nothing will replace it once signed out",
  );
});

// A hidden tab is nobody watching, and every tick spawns a helper process in the child's session
// to capture and JPEG-encode their whole desktop — by `_armShotTimer`'s own account the most
// expensive thing this tool does. Without this guard a dashboard left open in a pocket keeps
// paying that cost on the child's machine all day.
//
// It had no test. `_liveTick` was extracted from an interval closure precisely so its decisions
// could be reached, and this decision was the one still left unreached: deleting the guard
// outright left every JS test passing. Reaching it needs a document, and a full one — see
// `captureHarness`.
test("a hidden tab stops the live timer capturing, a visible one does not", () => {
  const shown = captureHarness({ hidden: false });
  shown.app._liveUntil = Number.MAX_SAFE_INTEGER;
  shown.app._liveTick();
  assert.equal(shown.calls.length, 1, "a visible tab must still refresh the picture");

  const pocketed = captureHarness({ hidden: true });
  pocketed.app._liveUntil = Number.MAX_SAFE_INTEGER;
  pocketed.app._liveTick();
  assert.equal(
    pocketed.calls.length,
    0,
    "a hidden tab must not spawn a capture helper on the child's machine",
  );
});

// The audit keys on who asked, so the request has to say. Tier cannot carry it any more: the timer
// asks for `full` whenever the full-size view is open, and auditing those one-for-one would evict
// the whole security history to make room for a timer.
test("only the live timer marks its captures as timer-driven", async () => {
  const { app, calls } = captureHarness();
  app._liveUntil = Number.MAX_SAFE_INTEGER;

  app._liveTick();
  await new Promise((r) => setImmediate(r));
  app.takeScreenshot(); // a person, superseding the frame in flight
  await new Promise((r) => setImmediate(r));

  assert.equal(calls.length, 2);
  assert.ok(calls[0].url.includes("live=1"), `timer frame must be marked: ${calls[0].url}`);
  assert.ok(!calls[1].url.includes("live=1"), `a person's capture must not be: ${calls[1].url}`);
});

// --- rejection -------------------------------------------------------------
//
// Five `400` handlers used to name one cause and show it whatever the server had actually
// objected to. The worst read "Warning seconds must be ≤ 600" for any rules rejection — so a
// 5,000-minute daily budget sent the parent to a field they had not touched, and which could not
// have been the problem since it is the one input carrying a `max`. `src/error.rs` renders every
// `AppError` as `{"error": "..."}` and `Rules::validate` alone has five distinct reasons, so the
// correct answer was already on the wire and being discarded.
//
// The fallback is not a nicety: axum's `Json` extractor rejects a malformed body before any
// handler runs and answers in plain text, so "invalid type: string, expected u32 at line 1
// column 42" is a reachable string. It must never reach a toast.

const stubResponse = (body) => ({
  json: async () => {
    if (body instanceof Error) throw body;
    return body;
  },
});

test("rejection shows the reason the server gave, capitalised", async () => {
  const app = loadApp();
  const msg = await app.rejection(
    stubResponse({ error: "daily limit must be <= 10080 minutes" }),
    "Could not save rules",
  );
  assert.equal(msg, "Daily limit must be <= 10080 minutes");
});

test("rejection keeps a multi-line message whole rather than clipping it", async () => {
  // `change_password` reuses console-shaped messages whose second line carries the counted and
  // required totals. First-line-only would drop exactly the part worth reading.
  const app = loadApp();
  const msg = await app.rejection(
    stubResponse({ error: "that password is too short.\n  counted:  5 characters" }),
    "fallback",
  );
  assert.ok(msg.includes("counted:  5 characters"), `detail was dropped: ${msg}`);
});

test("rejection falls back when the body is not ours", async () => {
  const app = loadApp();
  // Axum's own extractor rejection: plain text, so `.json()` throws.
  assert.equal(
    await app.rejection(stubResponse(new SyntaxError("Unexpected token")), "Could not save rules"),
    "Could not save rules",
  );
  // Valid JSON, but not our envelope.
  assert.equal(
    await app.rejection(stubResponse({ detail: "nope" }), "Could not save curfew"),
    "Could not save curfew",
  );
  // Present but empty, which would otherwise toast an empty alert.
  assert.equal(
    await app.rejection(stubResponse({ error: "   " }), "Could not save routine"),
    "Could not save routine",
  );
  // Present but not a string.
  assert.equal(
    await app.rejection(stubResponse({ error: 400 }), "Could not save rules"),
    "Could not save rules",
  );
});

// --- the three handlers that stopped restating the server's limits ----------
//
// `grantExtra`, `issueCode` and `applyRoutine` each printed a sentence of their own on a 400. Two
// of them named a *number* -- "Minutes out of range (1-240)" -- copied from a constant living on
// the other side of the wire, and `src/web.rs` carried a loop asserting the copies still matched
// `MAX_REQUEST_MINUTES` and `MAX_CODE_MINUTES`. `api::require_minutes` and the active-code cap now
// send their bound with the refusal, so the copies are gone and the loop went with them.
//
// These tests are what replaced that loop, and they pin a strictly stronger property. The loop
// could only show that two literals still matched two constants; it could not show that either
// literal ever reached a parent, and it would have gone on passing if the toast had been deleted
// outright. What is asserted here is that whatever the server said is what gets shown -- so a
// limit raised server-side arrives at the toast with no client edit at all, which is the whole
// reason for the change.

/** An app whose fetch refuses with `error`, recording every toast instead of rendering it. */
function appRefusedWith(error) {
  const toasts = [];
  const app = loadApp({
    fetch: async () => ({ ok: false, status: 400, json: async () => ({ error }) }),
  });
  app.toast = (msg, tone) => toasts.push([msg, tone]);
  // The success paths refetch; none of them should run here, but stub them so a regression that
  // takes the wrong branch fails on the assertion rather than on a missing method.
  for (const reload of ["loadToday", "loadUsage", "loadCodes", "loadRules", "loadRoutines"]) {
    app[reload] = () => {};
  }
  return { app, toasts };
}

test("a refused grant shows the bound the server sent, not one copied from it", async () => {
  const { app, toasts } = appRefusedWith("minutes must be between 1 and 240");
  await app.grantExtra(9999);
  assert.deepEqual(toasts, [["Minutes must be between 1 and 240", "error"]]);
});

test("a refused code says which of its two limits fired", async () => {
  // The old string named both at once -- "Minutes 1-240, and at most 50 active codes" -- because
  // the client had no way to tell which had. That was a workaround, not duplication, so removing
  // it makes the message more accurate rather than merely shorter. Each cause now arrives named.
  for (const [sent, shown] of [
    ["minutes must be between 1 and 240", "Minutes must be between 1 and 240"],
    ["at most 50 codes can be active at once", "At most 50 codes can be active at once"],
  ]) {
    const { app, toasts } = appRefusedWith(sent);
    app.newCodeMins = 9999;
    await app.issueCode();
    assert.deepEqual(toasts, [[shown, "error"]], `server sent: ${sent}`);
  }
});

test("a routine that is gone says so, instead of that something could not be done", async () => {
  const { app, toasts } = appRefusedWith("no such routine");
  await app.applyRoutine("Homework");
  assert.deepEqual(toasts, [["No such routine", "error"]]);
});

test("a refusal carrying no message still falls back to the handler's own sentence", async () => {
  // The fallback is the half that matters most here: axum's `Json` extractor rejects a malformed
  // body in plain text *before* any handler runs, so a 400 with no `error` field is reachable and
  // "invalid type: string, expected u32 at line 1 column 42" must never reach a toast.
  for (const [method, arg, fallback] of [
    ["grantExtra", 30, "Could not grant time"],
    ["issueCode", undefined, "Could not generate a code"],
    ["applyRoutine", "Homework", "Could not apply routine"],
  ]) {
    const toasts = [];
    const app = loadApp({
      fetch: async () => ({
        ok: false,
        status: 400,
        json: async () => {
          throw new SyntaxError("Unexpected token");
        },
      }),
    });
    app.toast = (msg, tone) => toasts.push([msg, tone]);
    for (const reload of ["loadToday", "loadUsage", "loadCodes", "loadRules", "loadRoutines"]) {
      app[reload] = () => {};
    }
    await app[method](arg);
    assert.deepEqual(toasts, [[fallback, "error"]], `${method} lost its fallback`);
  }
});

// --- budgetTone ------------------------------------------------------------
//
// Extracted when the sticky summary strip needed the same thresholds the Today card's progress bar
// already had inline. The value of extracting it is that the two cannot drift: a strip showing
// amber above a bar showing green is worse than either being wrong alone, because the parent has
// to work out which one to believe.

test("budgetTone escalates at the thresholds, not around them", () => {
  const app = loadApp();
  assert.equal(app.budgetTone(0), "error", "no time left is the urgent case");
  assert.equal(app.budgetTone(1), "warning");
  assert.equal(app.budgetTone(15), "warning", "15 is the boundary and must be inclusive");
  assert.equal(app.budgetTone(16), "primary", "one past the boundary is not yet a warning");
  assert.equal(app.budgetTone(600), "primary");
});

test("budgetTone treats an unknown remaining as ordinary, never as urgent", () => {
  const app = loadApp();
  // `today` is null before the first load and after a failed one. Painting the bar red there would
  // tell a parent their child is out of time when nothing has been measured at all.
  assert.equal(app.budgetTone(null), "primary");
  assert.equal(app.budgetTone(undefined), "primary");
});
