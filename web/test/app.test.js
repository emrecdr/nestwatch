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

// --- stRecentFocusDay ------------------------------------------------------
//
// Focus minutes and running minutes are two different measurements of the same day, and a day can
// carry either, both, or neither. The screen-time card picks the most recent day that has focus
// data to show, which must be chosen independently of the running-app data — a day can have apps
// and no focus (recorded before the watcher existed, or while it was dead) and picking that one
// would render an empty focus list under a heading claiming otherwise.

test("stRecentFocusDay picks the newest day that actually has focus data", () => {
  const app = withState({
    screentime: {
      days: [
        { date: "2026-08-14", apps: [{ name: "a.exe", minutes: 5 }], focused: [{ name: "a.exe", minutes: 3 }] },
        { date: "2026-08-15", apps: [{ name: "b.exe", minutes: 9 }], focused: [] },
      ],
    },
  });

  const day = app.stRecentFocusDay();
  assert.ok(day, "a day with focus data exists");
  assert.equal(day.date, "2026-08-14", "the newer day has no focus data, so it is not the one");
});

test("stRecentFocusDay is null when nothing has been measured", () => {
  const app = withState({
    screentime: {
      days: [{ date: "2026-08-15", apps: [{ name: "b.exe", minutes: 9 }], focused: [] }],
    },
  });

  assert.equal(
    app.stRecentFocusDay(),
    null,
    "no focus data must render as absent, not as an empty list under a date heading",
  );
});

test("stRecentPageDay picks the newest day carrying browser page titles", () => {
  const app = withState({
    screentime: {
      days: [
        { date: "2026-08-14", pages: [{ name: "Roblox", minutes: 40 }] },
        { date: "2026-08-15", pages: [] },
      ],
    },
  });

  assert.equal(app.stRecentPageDay().date, "2026-08-14");
});

test("stRecentPageDay is null when no browser time was recorded", () => {
  const app = withState({ screentime: { days: [{ date: "2026-08-15", pages: [] }] } });
  assert.equal(app.stRecentPageDay(), null);
});

// The rule the three stRecent*Day helpers share, stated once. Worth its own test because a change
// to it — days no longer sorted oldest-first, or "carries data" becoming a flag rather than a
// non-empty list — is otherwise a three-place edit with no signal if you miss one.
test("stRecentDayWith returns the newest day whose named list has entries", () => {
  const app = withState({
    screentime: {
      days: [
        { date: "2026-08-13", apps: [{ name: "a.exe", minutes: 1 }] },
        { date: "2026-08-14", apps: [] },
      ],
    },
  });

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
// enforcer (O10), as zero minutes used, and as nothing waiting.

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
