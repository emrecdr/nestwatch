// What the child's page shows and sends, driven through `ask.js` itself (see `ask-harness.js`).
//
// The expected values are written out rather than taken from the script's own tables: a test that
// compared the page with `EN.noLimit` would agree with any change made to `EN.noLimit`.

import test from "node:test";
import assert from "node:assert/strict";
import { loadAsk, StubElement } from "./ask-harness.js";

/** Routes for a page whose `/status` answers `fields`, on a day with no limit unless they say so. */
const status = (fields = {}) => ({ "/status": { body: { limited: false, ...fields } } });

/** Answer the next polls of `/status` with `fields`, then run one. */
async function nextPoll(page, fields) {
  page.routes["/status"] = { body: { limited: false, ...fields } };
  page.poll();
  await page.settle();
}

test("a day with no limit says so, and a limited day shows what is left and what was used", async () => {
  const free = loadAsk(status());
  await free.settle();
  assert.equal(free.el("today").textContent, "No time limit today \u{1F389}");

  const page = loadAsk(status({ limited: true, remaining_mins: 25, used_mins: 95, budget_mins: 120 }));
  await page.settle();
  const [left, unit, bar, detail] = page.el("today").children;
  assert.equal(left.textContent, "25");
  assert.equal(unit.textContent, "minutes left today");
  assert.equal(bar.tagName, "PROGRESS");
  assert.equal(bar.max, 120);
  assert.equal(bar.value, 95);
  assert.equal(bar.getAttribute("aria-label"), "95 of 120 minutes used today");
  assert.equal(detail.textContent, "used 95 of 120 min");
});

test("one minute left is said in the singular, and the bar stops at the limit", async () => {
  const page = loadAsk(status({ limited: true, remaining_mins: 1, used_mins: 130, budget_mins: 120 }));
  await page.settle();
  const [, unit, bar] = page.el("today").children;
  assert.equal(unit.textContent, "minute left today");
  assert.equal(bar.value, 120, "a day past its limit fills the bar, it does not overflow it");
});

test("a status the page cannot fetch says so, rather than leaving the space empty", async () => {
  // The 500 with a body matters: read as a status, `{error}` has no `limited` and would claim a day
  // with no limit.
  for (const answer of [{ status: 500 }, { status: 500, body: { error: "boom" } }, new TypeError("x")]) {
    const page = loadAsk({ "/status": answer });
    await page.settle();
    assert.equal(page.el("today").textContent, "Couldn't check your time right now.");
  }
});

test("the page refreshes every minute while it is visible, and not while it is hidden", async () => {
  const page = loadAsk(status());
  await page.settle();
  const asked = () => page.fetches.filter((f) => f.url === "/status").length;
  assert.equal(asked(), 1, "fetched once as it loaded");
  page.document.hidden = true;
  page.poll();
  await page.settle();
  assert.equal(asked(), 1, "a hidden tab asks nothing");
  page.document.hidden = false;
  page.poll();
  await page.settle();
  assert.equal(asked(), 2);
});

test("the page turns Dutch when the parent chose Dutch, once, and keeps English for an unknown language", async () => {
  const nl = loadAsk(status({ language: "nl" }));
  await nl.settle();
  const by = (key, field = "i18n") => nl.translated.find((el) => el.dataset[field] === key);
  assert.equal(nl.document.documentElement.lang, "nl");
  assert.equal(by("askHeading").textContent, "Vraag om meer tijd");
  assert.equal(by("whyHint", "i18nPlaceholder").placeholder, "mijn huiswerk is af");
  assert.equal(
    by("disclosureLabel", "i18nLabel").getAttribute("aria-label"),
    "Wat dit programma kan zien",
  );
  assert.equal(nl.el("today").textContent, "Vandaag geen tijdslimiet \u{1F389}", "built text too");

  // Applied once: the next poll naming the same language swaps nothing again.
  by("askHeading").textContent = "left alone";
  await nextPoll(nl, { language: "nl" });
  assert.equal(by("askHeading").textContent, "left alone");

  // A key the Dutch table lacks, or one naming a function rather than a sentence, leaves the
  // markup's English in place instead of writing "undefined" or source code onto the page.
  const odd = loadAsk(status({ language: "nl" }));
  const extras = [
    ["i18n", "notAKey"],
    ["i18n", "usedOf"],
    ["i18nPlaceholder", "notAKey"],
    ["i18nLabel", "notAKey"],
  ].map(([field, key]) => {
    const el = new StubElement("span");
    el.dataset[field] = key;
    el.textContent = "English";
    el.placeholder = "English";
    el.setAttribute("aria-label", "English");
    odd.translated.push(el);
    return el;
  });
  await odd.settle();
  for (const el of extras) {
    assert.equal(el.textContent, "English");
    assert.equal(el.placeholder, "English");
    assert.equal(el.getAttribute("aria-label"), "English");
  }

  const unknown = loadAsk(status({ language: "xx" }));
  await unknown.settle();
  assert.equal(unknown.document.documentElement.lang, "en");
  assert.equal(unknown.el("today").textContent, "No time limit today \u{1F389}");
});

test("a parent's answer is shown once: yes with the minutes, no without blame, nothing while pending", async () => {
  const page = loadAsk(status({ request: { state: "approved", minutes: 20 } }));
  await page.settle();
  const outcome = page.el("outcome");
  assert.equal(outcome.textContent, "A parent said yes to 20 more minutes.");
  assert.equal(outcome.className, "mt-1 text-center text-sm text-success");

  await nextPoll(page, { request: { state: "denied", minutes: 20 } });
  assert.equal(outcome.textContent, "A parent said no this time.");
  assert.equal(outcome.className, "mt-1 text-center text-sm opacity-70", "an answer, not an error");

  // The same answer is not written again: a live region rewritten every minute is read out every
  // minute.
  outcome.textContent = "left alone";
  await nextPoll(page, { request: { state: "denied", minutes: 20 } });
  assert.equal(outcome.textContent, "left alone");

  await nextPoll(page, { request: { state: "pending", minutes: 10 } });
  assert.equal(outcome.textContent, "");
  assert.equal(outcome.className, "mt-1 text-center text-sm");
});

test("the week draws measured days to scale, hatches an unmeasured one, and reads as one sentence", async () => {
  const page = loadAsk(
    status({
      recent_days: [
        { date: "2026-11-09", minutes: 120 },
        { date: "2026-11-10", minutes: null },
        { date: "2026-11-11", minutes: 1 },
        { date: "2026-11-12", minutes: 60 },
        { date: "2026-11-13" },
      ],
    }),
  );
  await page.settle();
  const week = page.el("week");
  const bars = week.children.map((col) => col.children[0].children[0]);
  // The longest day is the full 44px, a minute still shows as the 2px floor, and the unmeasured
  // day is a full-height hatch — never a zero.
  // A day with no figure at all is left out of the scale, as an unmeasured one is.
  assert.deepEqual(
    bars.map((b) => b.style.height),
    ["44px", "44px", "2px", "22px", "44px"],
  );
  assert.deepEqual(
    week.children.map((col) => col.children[0].style.height),
    Array(5).fill("44px"),
    "every column's track is the full height, so the bars share one scale",
  );
  assert.match(bars[1].className, /\bst-nodata\b/);
  assert.doesNotMatch(bars[0].className, /st-nodata/);
  assert.deepEqual(
    week.children.map((col) => col.children[1].textContent),
    ["Mon", "Tue", "Wed", "Thu", "Fri"],
    "named from the local date, not from UTC midnight",
  );
  assert.equal(week.textContent, "MonTueWedThuFri", "the week holds its columns and nothing else");
  assert.equal(week.getAttribute("role"), "img");
  assert.equal(
    week.getAttribute("aria-label"),
    "Your last 7 days: Mon 120 min, Tue not measured, Wed 1 min, Thu 60 min, Fri not measured",
  );

  // Redrawn rather than appended to.
  await nextPoll(page, { recent_days: [{ date: "2026-11-13", minutes: 30 }] });
  assert.equal(week.children.length, 1);
});

test("an install with no history yet draws no week and announces none", async () => {
  const page = loadAsk(status({ recent_days: [] }));
  await page.settle();
  assert.equal(page.el("week").children.length, 0);
  assert.equal(page.el("week").getAttribute("aria-label"), null);
});

test("how often the screen was looked at is said in three forms, and only when it changes", async () => {
  const page = loadAsk(status({ watched_today: 0 }));
  await page.settle();
  const watched = page.el("watched");
  assert.equal(watched.textContent, "Your screen wasn't looked at today.");
  await nextPoll(page, { watched_today: 1 });
  assert.equal(watched.textContent, "Your screen was looked at once today.");
  await nextPoll(page, { watched_today: 3 });
  assert.equal(watched.textContent, "Your screen was looked at 3 times today.");
  watched.textContent = "left alone";
  await nextPoll(page, { watched_today: 3 });
  assert.equal(watched.textContent, "left alone");
  await nextPoll(page, {});
  assert.equal(watched.textContent, "left alone", "a status without the count says nothing about it");
});

test("asking for time sends the minutes and the reason, and says it was sent", async () => {
  let busy = null;
  let shown = null;
  const page = loadAsk({
    ...status(),
    "/time-request": () => {
      busy = page.el("btn").disabled;
      shown = page.el("msg").textContent;
      return { body: {} };
    },
  });
  await page.settle();
  page.el("msg").textContent = "the last answer";
  page.el("minutes").value = "30";
  page.el("reason").value = "homework done";
  const before = page.fetches.length;
  const event = await page.submit("form");

  assert.ok(event.prevented, "the form must not navigate away");
  assert.equal(busy, true, "the button is held down while the request is out");
  assert.equal(shown, "", "the last answer is cleared while the new request is out");
  assert.deepEqual(
    page.fetches.find((f) => f.url === "/time-request"),
    {
      url: "/time-request",
      method: "POST",
      type: "application/json",
      body: { minutes: 30, reason: "homework done" },
    },
  );
  assert.equal(page.el("msg").textContent, "Sent — waiting for a parent to reply.");
  assert.equal(page.el("msg").className, "mt-1 text-center text-sm text-success");
  assert.equal(page.el("form").resets, 1);
  assert.equal(page.el("btn").disabled, false);
  await page.settle();
  assert.ok(
    page.fetches.slice(before).some((f) => f.url === "/status"),
    "the answer area refreshes at once, so an old refusal is not left under the new request",
  );
});

test("a refused, throttled or unsent request says which, and the button comes back", async () => {
  for (const [answer, said] of [
    [{ status: 429 }, "Too many requests — wait a bit and try again."],
    [{ status: 400 }, "Couldn't send (check the minutes)."],
    [new TypeError("offline"), "Couldn't reach the server."],
  ]) {
    const page = loadAsk({ ...status(), "/time-request": answer });
    await page.settle();
    page.el("minutes").value = "lots";
    await page.submit("form");
    assert.equal(page.el("msg").textContent, said);
    assert.equal(page.el("msg").className, "mt-1 text-center text-sm text-error");
    assert.equal(page.el("btn").disabled, false);
    assert.equal(page.el("form").resets, 0, "a request that failed keeps what the child typed");
    assert.equal(
      page.fetches.find((f) => f.url === "/time-request").body.minutes,
      0,
      "minutes that are not a number are sent as none, for the server to refuse",
    );
  }
});

test("a time code is sent trimmed and in capitals, and the answer says what it did", async () => {
  let shown = null;
  let busy = null;
  const redeemed = {
    "/redeem-code": () => {
      shown = limited?.el("codeMsg").textContent;
      busy = limited?.el("codeBtn").disabled;
      return { body: { ok: true, minutes: 15 } };
    },
  };
  // Declared first because the route above reads it when the code is posted, after it is set.
  let limited = null;
  limited = loadAsk({
    ...status({ limited: true, remaining_mins: 10, used_mins: 50, budget_mins: 60 }),
    ...redeemed,
  });
  await limited.settle();
  limited.el("codeMsg").textContent = "the last answer";
  limited.el("code").value = "  ab12cd ";
  const statusFetches = () => limited.fetches.filter((f) => f.url === "/status").length;
  const before = statusFetches();
  const event = await limited.submit("codeForm");
  assert.ok(event.prevented);
  const sent = limited.fetches.find((f) => f.url === "/redeem-code");
  assert.equal(sent.method, "POST");
  assert.deepEqual(sent.body, { code: "AB12CD" });
  assert.equal(sent.type, "application/json", "the server reads JSON only");
  assert.equal(shown, "", "the last answer is cleared while the code is checked");
  assert.equal(busy, true, "the button is held down while the code is checked");
  assert.equal(limited.el("codeMsg").textContent, "Added 15 minutes!");
  assert.equal(limited.el("codeMsg").className, "mt-1 text-center text-sm text-success");
  assert.equal(limited.el("codeForm").resets, 1);
  assert.equal(limited.el("codeBtn").disabled, false);
  await limited.settle();
  assert.equal(statusFetches(), before + 1, "the minutes left refresh at once after a code");

  // No limit today: the code still counts, and the page does not claim it added anything.
  const free = loadAsk({ ...status(), ...redeemed });
  await free.settle();
  await free.submit("codeForm");
  assert.equal(free.el("codeMsg").textContent, "Code accepted — but there's no limit today anyway.");

  // Not knowing whether a limit applies is not the same as knowing none does.
  const unknown = loadAsk({ "/status": new TypeError("offline"), ...redeemed });
  await unknown.settle();
  await unknown.submit("codeForm");
  assert.equal(unknown.el("codeMsg").textContent, "Added 15 minutes!");
});

test("a wrong, throttled or unsent code says which, and the button comes back", async () => {
  for (const [answer, said] of [
    [{ status: 429, body: {} }, "Too many tries — wait a bit and try again."],
    [{ status: 200, body: { ok: false } }, "That code isn't valid."],
    [{ status: 400 }, "That code isn't valid."],
    // An answer with no body is not a redeemed code, and not a lost connection either.
    [{ status: 200 }, "That code isn't valid."],
    [new TypeError("offline"), "Couldn't reach the server."],
  ]) {
    const page = loadAsk({ ...status(), "/redeem-code": answer });
    await page.settle();
    page.el("code").value = "NOPE";
    await page.submit("codeForm");
    assert.equal(page.el("codeMsg").textContent, said);
    assert.equal(page.el("codeMsg").className, "mt-1 text-center text-sm text-error");
    assert.equal(page.el("codeBtn").disabled, false);
    assert.equal(page.el("codeForm").resets, 0);
  }
});
