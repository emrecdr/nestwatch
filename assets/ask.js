// --- Language --------------------------------------------------------
// The page ships English in the markup and swaps it here when the parent has chosen otherwise, so
// a browser with JS disabled still shows something readable rather than empty elements. `/status`
// carries the choice; `Accept-Language` deliberately does not decide it, because that is set in
// the child's own browser and the child does not choose the language of the notice explaining what
// is being watched.
//
// Keys match `data-i18n` in ask.html. `nl` must answer every key `en` does — the test at the
// bottom of web/test/app.test.js holds the two tables to the same shape, because a missing key
// would silently leave one English sentence in an otherwise Dutch page.
const STRINGS = {
  en: null, // the markup is already English; nothing to swap
  nl: {
    title: "Jouw schermtijd",
    checking: "Even kijken\u2026",
    needMore: "meer nodig?",
    askHeading: "Vraag om meer tijd",
    askBlurb: "Je verzoek gaat naar je ouder.",
    minutes: "Minuten",
    why: "Waarom? (niet verplicht)",
    whyHint: "mijn huiswerk is af",
    send: "Verstuur verzoek",
    or: "of",
    codeHeading: "Heb je een code?",
    codeBlurb: "Voer een tijdcode in om nu minuten toe te voegen.",
    codeLabel: "Tijdcode",
    redeem: "Inwisselen",
    weekHeading: "Je afgelopen 7 dagen",
    disclosureLabel: "Wat dit programma kan zien",
    disclosure:
      "Een ouder heeft dit ingesteld en kan dit scherm zien, welke apps je gebruikt en hoe lang. " +
      "Windows zet een gele rand om het scherm terwijl er gekeken wordt.",

    // Strings this script builds rather than swaps.
    noLimit: "Vandaag geen tijdslimiet \u{1F389}",
    minuteLeft: "minuut over vandaag",
    minutesLeft: "minuten over vandaag",
    usedOf: (used, budget) => `${used} van ${budget} min gebruikt`,
    usedAria: (used, budget) => `${used} van ${budget} minuten vandaag gebruikt`,
    cantCheck: "Kan je tijd nu niet ophalen.",
    sent: "Verstuurd \u2014 je ouder krijgt bericht.",
    tooMany: "Te veel verzoeken \u2014 wacht even en probeer het opnieuw.",
    sendFailed: "Versturen mislukt (klopt het aantal minuten?).",
    noServer: "Geen verbinding met de server.",
    approved: (n) => `Je ouder zei ja tegen ${n} extra minuten.`,
    denied: "Je ouder zei deze keer nee.",
    codeNoLimit: "Code geaccepteerd \u2014 maar vandaag geldt er toch geen limiet.",
    codeAdded: (n) => `${n} minuten toegevoegd!`,
    codeTooMany: "Te veel pogingen \u2014 wacht even en probeer het opnieuw.",
    codeInvalid: "Die code klopt niet.",
    notMeasured: "niet gemeten",
    minShort: (n) => `${n} min`,
    // Counted, never listed with times. Three forms rather than one with a number, because Dutch
    // and English both read badly as "1 keer"/"1 times" and this sentence is addressed to a child.
    watchedNone: "Er is vandaag niet naar je scherm gekeken.",
    watchedOnce: "Er is vandaag \u00e9\u00e9n keer naar je scherm gekeken.",
    watchedMany: (n) => `Er is vandaag ${n} keer naar je scherm gekeken.`,
  },
};

// English lives in the markup, so this is only ever consulted for another language.
const EN = {
  noLimit: "No time limit today \u{1F389}",
  minuteLeft: "minute left today",
  minutesLeft: "minutes left today",
  usedOf: (used, budget) => `used ${used} of ${budget} min`,
  usedAria: (used, budget) => `${used} of ${budget} minutes used today`,
  cantCheck: "Couldn't check your time right now.",
  sent: "Sent \u2014 waiting for a parent to reply.",
  tooMany: "Too many requests \u2014 wait a bit and try again.",
  sendFailed: "Couldn't send (check the minutes).",
  noServer: "Couldn't reach the server.",
  approved: (n) => `A parent said yes to ${n} more minutes.`,
  denied: "A parent said no this time.",
  codeNoLimit: "Code accepted \u2014 but there's no limit today anyway.",
  codeAdded: (n) => `Added ${n} minutes!`,
  codeTooMany: "Too many tries \u2014 wait a bit and try again.",
  codeInvalid: "That code isn't valid.",
  weekHeading: "Your last 7 days",
  notMeasured: "not measured",
  minShort: (n) => `${n} min`,
  watchedNone: "Your screen wasn't looked at today.",
  watchedOnce: "Your screen was looked at once today.",
  watchedMany: (n) => `Your screen was looked at ${n} times today.`,
};

let strings = EN;

// Swap the markup once, the first time /status names a language other than the one already shown.
let appliedLang = "en";
function applyLanguage(tag) {
  if (!tag || tag === appliedLang) return;
  const table = STRINGS[tag];
  if (!table) return; // unknown tag: leave the English markup rather than blanking it
  appliedLang = tag;
  strings = table;
  document.documentElement.lang = tag;
  for (const el of document.querySelectorAll("[data-i18n]")) {
    const v = table[el.dataset.i18n];
    if (typeof v === "string") el.textContent = v;
  }
  for (const el of document.querySelectorAll("[data-i18n-placeholder]")) {
    const v = table[el.dataset.i18nPlaceholder];
    if (typeof v === "string") el.placeholder = v;
  }
  for (const el of document.querySelectorAll("[data-i18n-label]")) {
    const v = table[el.dataset.i18nLabel];
    if (typeof v === "string") el.setAttribute("aria-label", v);
  }
}

// --- Today -----------------------------------------------------------
// Knowing how much time is left is the question that otherwise gets asked out loud, so
// it's the first thing on the page. /status is deliberately narrow: totals only, never
// the rules themselves.
const todayEl = document.getElementById("today");
const outcomeEl = document.getElementById("outcome");
let limitedToday = null; // null = unknown (server unreachable)

// What the last poll said about the child's newest request, so the outcome is only announced when
// it CHANGES. Re-writing the same text into an aria-live region on every poll would make a screen
// reader repeat "your request was approved" once a minute, forever.
let lastRequestState = null;

// Render the fate of the child's newest request.
//
// This is the half of the conversation that was missing: the page could ask, and could never
// report back. A denial in particular reached the child through no channel at all — it looked
// exactly like being ignored, which is worse than the answer.
function showOutcome(request) {
  const state = request ? request.state : null;
  if (state === lastRequestState) return;
  lastRequestState = state;

  if (!request || state === "pending") {
    // "Pending" is already covered by the submit handler's own message, and repeating it here
    // would put two live sentences on screen saying the same thing.
    outcomeEl.textContent = "";
    outcomeEl.className = "mt-1 text-center text-sm";
    return;
  }
  const approved = state === "approved";
  outcomeEl.textContent = approved
    ? strings.approved(request.minutes)
    : strings.denied;
  // Not styled as an error: a denial is a normal answer to a fair question, and colouring it red
  // makes the page look like the child did something wrong by asking.
  outcomeEl.className = `mt-1 text-center text-sm ${approved ? "text-success" : "opacity-70"}`;
}


// --- The child's own week --------------------------------------------
const weekEl = document.getElementById("week");
const watchedEl = document.getElementById("watched");

/** Tallest bar, in px. Small on purpose: this is a shape to recognise, not a chart to read. */
const WEEK_BAR_PX = 44;

/** The weekday name for a `YYYY-MM-DD` string, in whichever language the page is showing.
 *
 *  Built from the parts rather than `new Date(d.date)`, which parses a bare date as **UTC**
 *  midnight — so west of Greenwich every column would be labelled with the day before. The
 *  three-argument form is local, which is what the date means here.
 *
 *  `short` ("Mon"), not `narrow` ("M"). Narrow fits more easily and was tried first, but English
 *  narrow renders Saturday and Sunday both as S and Tuesday and Thursday both as T — so three of
 *  seven columns are unidentifiable, on the one view whose entire purpose is recognising which
 *  days are heavy. Three letters still fit: seven columns of a 390px phone leave ~46px each.
 */
function weekdayName(iso) {
  const [y, m, d] = iso.split("-").map(Number);
  return new Date(y, m - 1, d).toLocaleDateString(appliedLang, { weekday: "short" });
}

/** Seven columns, oldest to newest. A day with no row is hatched, never drawn as a zero.
 *
 *  `.st-nodata` is the same class the parent's chart paints an unmeasured day with, so the two
 *  views cannot come to disagree about what "nothing recorded" looks like. The distinction is the
 *  point of showing this at all: a child who sees a flat zero for a day the service was off
 *  learns something false about their own week.
 *
 *  Labelled as one image rather than seven, so a screen reader gets a sentence instead of a
 *  column-by-column crawl.
 */
function renderWeek(days) {
  weekEl.textContent = "";
  if (!Array.isArray(days) || days.length === 0) return;

  const known = days.filter((d) => typeof d.minutes === "number");
  const peak = Math.max(1, ...known.map((d) => d.minutes));
  const spoken = [];

  for (const d of days) {
    const col = document.createElement("div");
    col.className = "flex flex-1 flex-col items-center gap-1";

    const track = document.createElement("div");
    track.className = "flex w-full items-end";
    track.style.height = `${WEEK_BAR_PX}px`;

    const bar = document.createElement("div");
    const measured = typeof d.minutes === "number";
    if (measured) {
      bar.className = "w-full rounded-sm bg-primary";
      // A floor of 2px so a short-but-real day is still visibly different from an empty one.
      bar.style.height = `${Math.max(2, Math.round((d.minutes / peak) * WEEK_BAR_PX))}px`;
    } else {
      bar.className = "st-nodata w-full rounded-sm opacity-70";
      bar.style.height = `${WEEK_BAR_PX}px`;
    }
    track.append(bar);

    const label = document.createElement("span");
    label.className = "text-[10px] opacity-60";
    label.textContent = weekdayName(d.date);

    col.append(track, label);
    weekEl.append(col);

    spoken.push(
      `${weekdayName(d.date)} ${measured ? strings.minShort(d.minutes) : strings.notMeasured}`,
    );
  }

  weekEl.setAttribute("role", "img");
  weekEl.setAttribute("aria-label", `${strings.weekHeading}: ${spoken.join(", ")}`);
}

// Only written when the number changes. The page re-polls every minute, and rewriting an
// `aria-live` region with identical text is how a screen reader comes to repeat the same sentence
// forever — the same reason `showOutcome` above tracks its last value.
let lastWatched = null;
function renderWatched(n) {
  if (typeof n !== "number" || n === lastWatched) return;
  lastWatched = n;
  watchedEl.textContent =
    n === 0 ? strings.watchedNone
    : n === 1 ? strings.watchedOnce
    : strings.watchedMany(n);
}

async function loadStatus() {
  try {
    const r = await fetch("/status");
    if (!r.ok) throw new Error("status " + r.status);
    const s = await r.json();
    limitedToday = !!s.limited;
    // Rendered before the early return below: a pending request still has an answer worth
    // showing on a day that has no minute limit at all.
    applyLanguage(s.language);
    showOutcome(s.request);
    // Before the `!s.limited` early return below: a week and a look-count are facts about the
    // child's own machine whether or not a minute limit applies today.
    renderWeek(s.recent_days);
    renderWatched(s.watched_today);
    todayEl.innerHTML = "";

    if (!s.limited) {
      const p = document.createElement("p");
      p.className = "py-2 text-lg font-semibold text-success";
      p.textContent = strings.noLimit;
      todayEl.append(p);
      return;
    }

    const left = document.createElement("p");
    left.className = "text-4xl font-bold tabular-nums";
    left.textContent = s.remaining_mins;
    const unit = document.createElement("p");
    unit.className = "text-sm opacity-70";
    unit.textContent =
      s.remaining_mins === 1 ? strings.minuteLeft : strings.minutesLeft;

    const bar = document.createElement("progress");
    bar.className = "progress progress-primary mt-3 w-full";
    bar.max = s.budget_mins;
    bar.value = Math.min(s.used_mins, s.budget_mins);
    bar.setAttribute(
      "aria-label",
      strings.usedAria(s.used_mins, s.budget_mins),
    );

    const detail = document.createElement("p");
    detail.className = "mt-1 text-xs opacity-60";
    detail.textContent = strings.usedOf(s.used_mins, s.budget_mins);

    todayEl.append(left, unit, bar, detail);
  } catch {
    limitedToday = null;
    todayEl.innerHTML = "";
    const p = document.createElement("p");
    p.className = "text-sm opacity-70";
    p.textContent = strings.cantCheck;
    todayEl.append(p);
  }
}
loadStatus();
// Cheap and only while the page is open; keeps the number honest as time is used.
setInterval(() => {
  if (!document.hidden) loadStatus();
}, 60000);

// One place for the message styling, so success/error can't drift between the two forms.
const setMsg = (el, text, ok = false) => {
  el.textContent = text;
  el.className = `mt-1 text-center text-sm text-${ok ? "success" : "error"}`;
};

const form = document.getElementById("form");
const msg = document.getElementById("msg");
const btn = document.getElementById("btn");
form.addEventListener("submit", async (e) => {
  e.preventDefault();
  btn.disabled = true;
  msg.textContent = "";
  const minutes = parseInt(document.getElementById("minutes").value, 10) || 0;
  const reason = document.getElementById("reason").value;
  try {
    const r = await fetch("/time-request", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ minutes, reason }),
    });
    if (r.ok) {
      setMsg(msg, strings.sent, true);
      form.reset();
      // Clear any previous answer straight away rather than at the next poll: leaving "a parent
      // said no this time" on screen underneath "sent, waiting for a reply" reads as a refusal of
      // the request just made.
      loadStatus();
    } else if (r.status === 429) {
      setMsg(msg, strings.tooMany);
    } else {
      setMsg(msg, strings.sendFailed);
    }
  } catch {
    setMsg(msg, strings.noServer);
  } finally {
    btn.disabled = false;
  }
});

const codeForm = document.getElementById("codeForm");
const codeMsg = document.getElementById("codeMsg");
const codeBtn = document.getElementById("codeBtn");
codeForm.addEventListener("submit", async (e) => {
  e.preventDefault();
  codeBtn.disabled = true;
  codeMsg.textContent = "";
  // The field only *looks* uppercase via CSS; normalize before sending.
  const code = document.getElementById("code").value.trim().toUpperCase();
  try {
    const r = await fetch("/redeem-code", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ code }),
    });
    const j = await r.json().catch(() => ({}));
    if (r.ok && j.ok) {
      // Don't claim minutes were "added" when no limit applies today — the grant is
      // recorded, but it changes nothing the child would notice, and saying otherwise
      // reads as the tool lying to them.
      setMsg(
        codeMsg,
        limitedToday === false
          ? strings.codeNoLimit
          : strings.codeAdded(j.minutes),
        true,
      );
      codeForm.reset();
      loadStatus();
    } else if (r.status === 429) {
      setMsg(codeMsg, strings.codeTooMany);
    } else {
      setMsg(codeMsg, strings.codeInvalid);
    }
  } catch {
    setMsg(codeMsg, strings.noServer);
  } finally {
    codeBtn.disabled = false;
  }
});
