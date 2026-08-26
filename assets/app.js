// Both enforcers stamp a heartbeat every 30s; past this many seconds since the last one,
// "enforcement is alive" can no longer be assumed. One shared constant so the "Today" panel
// banner and the screen-time card's staleness warning can never disagree about what counts
// as stale — they used to (150 vs 300), so a gap between them could show "no check-in" in
// one place and "all fine" in the other for the same age.
const ENFORCER_STALE_SECS = 150;

// --- Theme -------------------------------------------------------------------------------------
//
// Three states, not two, and "auto" is the default: the stylesheet ships `light` on `:root` and
// `dim` under `prefers-color-scheme: dark`, so with no attribute set the page already follows the
// device. This adds the override for the case the device setting gets wrong — a parent checking at
// eleven at night on a phone that is still in light mode, or the reverse.
//
// Applied at the top of this file rather than from an inline `<script>` in the head, which is the
// usual anti-flash trick: `script-src` does not admit `'unsafe-inline'` and
// `no_inline_script_on_any_served_page` keeps it that way. This file is deferred, so it runs before
// Alpine reveals the body, which is early enough.
const THEME_KEY = "nw-theme";

function readTheme() {
  // `try` because storage can be absent (the test harness) or blocked (private modes, some
  // enterprise policies), and a dashboard that fails to load over a theme preference would be a
  // poor trade.
  try {
    const t = localStorage.getItem(THEME_KEY);
    return t === "light" || t === "dark" ? t : "auto";
  } catch {
    return "auto";
  }
}

function applyTheme(theme) {
  if (typeof document === "undefined") return; // evaluated by the tests, where there is no document
  const root = document.documentElement;
  // Removing the attribute is what hands control back to `prefers-color-scheme`. Setting it to
  // anything — including "auto" — would pin a theme, which is the bug this replaces.
  if (theme === "auto") root.removeAttribute("data-theme");
  else root.setAttribute("data-theme", theme === "dark" ? "dim" : "light");
}

applyTheme(readTheme());

// Register the component under a name Alpine can look up.
//
// `x-data="app()"` worked with the standard build because that build evaluates the attribute as
// JavaScript, where `app` is a global. The CSP build reaches no globals at all -- that is the point
// of it -- so the component is handed over by name instead, and the markup says `x-data="app"`.
// Guarded like `applyTheme` above, and for the same reason: this file is also evaluated by the
// tests, in a `vm` context with no `document`. An unguarded call here throws while the file is
// being read, so *every* test fails at once with a ReferenceError that names none of them.
if (typeof document !== "undefined") {
  document.addEventListener("alpine:init", () => {
    Alpine.data("app", app);
  });
}

// Game portals, recognised from a page title alone.
//
// Module scope, built once: `gamePortal` is called twice per row by the template (once to decide
// whether to show the badge, once to fill it) across up to `MAX_PAGES` rows, and regex literals in
// a function body are re-created on every call.
//
// Word boundaries, not substrings, and brand tokens, never the word "games". A false positive is
// worse than a miss because the parent acts on it -- \b keeps "poki" from matching "Pokimane", and
// nothing here matches a news headline about games.
//
// Most-specific first: "Roblox - now.gg" is now.gg, because the interesting fact is that Roblox is
// being played *through a browser*, which is how a blocked native app gets dodged.
const GAME_PORTALS = [
  [/\bnow\.gg\b/, "now.gg"],
  [/\bcrazygames\b/, "CrazyGames"],
  [/\bcoolmath\b/, "Coolmath Games"],
  [/\bpoki\b/, "Poki"],
  [/\bminiclip\b/, "Miniclip"],
  [/\bkongregate\b/, "Kongregate"],
  [/\bitch\.io\b/, "itch.io"],
  [/\barmor games\b/, "Armor Games"],
  [/\baddicting ?games\b/, "Addicting Games"],
  [/\bgame ?jolt\b/, "Game Jolt"],
  [/\bsilvergames\b/, "Silvergames"],
  [/\by8\.com\b/, "Y8"],
  [/\bfriv\b/, "Friv"],
  [/\blagged\.com\b/, "Lagged"],
  [/\broblox\b/, "Roblox"],
];

// The screen-time report before anything has been fetched.
//
// One definition, called from both the initial state and `resetSessionData`. It was written out
// twice, and a field added to the report reached one copy and not the other — so a sign-out left
// the new arrays holding the previous session's values while everything beside them reset. Shape
// duplicated in two places is the same defect `resetSessionData` exists to prevent, one level down.
function emptyScreentime() {
  return {
    days: [],
    total_mins: 0,
    measured_days: 0,
    daily_avg_mins: null,
    prev_total_mins: null,
    change_pct: null,
    app_totals: [],
    focus_totals: [],
    page_totals: [],
    group_totals: [],
    // `null` rather than an empty object: the server returns null when it cannot answer honestly,
    // and that is a different state from "checked, nothing new". Collapsing them would make a
    // working check look like a broken one on the first day the watcher ran.
    first_seen: null,
  };
}

function app() {
  return {
    theme: readTheme(),
    // Sun and moon carry light and dark without a word. "Follow device" has no icon anyone reads
    // reliably, so it keeps a word — the alternative is a monitor glyph that says "computer", not
    // "match whatever this one is set to".
    //
    // Every one of them carries `aria-label` *and* `title`: a bare glyph names itself to nobody,
    // which is the same gap the unlabelled curfew switch had. The title is the sighted half.
    themeOptions: [
      { key: "auto", glyph: "Auto", label: "Follow this device's setting" },
      { key: "light", glyph: "\u2600\uFE0F", label: "Light" },
      { key: "dark", glyph: "\u{1F319}", label: "Dark" },
    ],
    authed: null,
    busy: false,
    password: "",
    loginError: "",
    processes: [],
    loadingProcs: false,
    shotUrl: null,
    loadingShot: false,
    autoRefresh: false,
    // When the frame on screen was captured (epoch ms), and whether the last live attempt failed.
    // Together these are what make a stalled live view distinguishable from a child sitting still:
    // without them a stopped service, a signed-out child or a wedged helper all leave the last good
    // picture up with the toggle still on, and nothing on the page ever says so.
    shotAt: null,
    shotStale: false,
    // Which tier produced the frame currently on screen, so `openShotFull` can tell whether it
    // already has a sharp one. `null` before the first capture.
    shotTier: null,
    // Re-read once a second so "updated 4s ago" counts up on its own rather than only when a new
    // frame lands — the case that matters is precisely the one where no new frame is landing.
    now: Date.now(),
    _shotTimer: null,
    _shotBusy: false,
    // Which capture is the current one. Incremented on every start; a reply whose generation
    // is no longer the newest has been superseded and must not touch shared state.
    _shotGen: 0,
    _clockTimer: null,
    // Aborts the in-flight capture when Live is switched off or the session ends. Without it a
    // capture started at second 0 still assigns `shotUrl` when it returns, up to 15s after the
    // parent turned the live view off — a picture arriving after the switch says stop.
    _shotAbort: null,
    _pollTimer: null,
    _pollMs: 60000,
    // How often the live view asks for a frame. Chosen by the parent from `refreshOptions`; 5s is
    // the default rather than the 3s this shipped with, because every tick spawns a helper in the
    // child's session and 3s was both the most expensive setting available and the only one.
    _refreshMs: 5000,
    // Offered cadences. Slower is cheaper on the child's machine, and nothing here needs to be
    // fast: the point is noticing what someone is doing, not watching them type.
    refreshOptions: [
      { ms: 2000, label: "2s" },
      { ms: 5000, label: "5s" },
      { ms: 15000, label: "15s" },
    ],
    // Live view stops itself after this long. `document.hidden` already covers a backgrounded tab,
    // but a tab left *visible* on a second monitor casts all day, and the parent who opened it to
    // glance at one thing has long since walked away.
    _liveMaxMs: 15 * 60 * 1000,
    _liveUntil: 0,
    killTarget: null,
    confirmShutdown: false,
    shotFull: false,
    curfew: { enabled: false, start: "22:00", end: "07:00", warn_secs: 60, windows: [] },
    savingCurfew: false,
    // The seven weekday boxes, defined once here rather than built in the markup.
    //
    // `short` is two characters, not one. `d[0]` rendered M T W T F S S — Tuesday and Thursday both
    // "T", Saturday and Sunday both "S", four of seven ambiguous, at the smallest size the design
    // has, on the phone this product is meant to be used from. Order was the only thing telling
    // them apart. `full` is the accessible name: that single letter was also the whole of what a
    // screen reader announced, so "T, checkbox, checked" named no day at all.
    dayOptions: [
      { key: "mon", short: "Mo", full: "Monday" },
      { key: "tue", short: "Tu", full: "Tuesday" },
      { key: "wed", short: "We", full: "Wednesday" },
      { key: "thu", short: "Th", full: "Thursday" },
      { key: "fri", short: "Fr", full: "Friday" },
      { key: "sat", short: "Sa", full: "Saturday" },
      { key: "sun", short: "Su", full: "Sunday" },
    ],
    rules: { enabled: true, daily_budget_mins: 0, budget_by_weekday: null, blocklist: [], app_limits: {}, app_groups: [], warn_secs: 60, budget_action: "lock" },
    appLimitRows: [],
    groupRows: [],
    savingRules: false,
    routines: [],
    loadingRoutines: false,
    newRoutineName: "",
    savingRoutine: false,
    // `null` until figures actually arrive — the card is gated on that, so there is no second flag
    // to keep in sync with this one. A zeroed literal here would be a lie the markup reads out as
    // measurement: "0 min used today" on a dashboard that may never have reached the service.
    // Same shape, and the same reason, as `foreground::Feed`'s `Option<Sample>` on the Rust side,
    // and the same rule the six other panels already follow (their own emptiness is the signal).
    today: null,
    loadingToday: false,
    // Whether the first /api/usage/today attempt has finished, succeeded or not. A *separate*
    // question from whether it arrived, and not derivable from `today`: this one is what makes the
    // staleness warning reachable after a failure, where `today` is still null. See
    // isEnforcerStale, which treats a missing age as stale.
    todayAsked: false,
    grantingExtra: false,
    _lastPerDay: null, // remembers the per-day array while single-limit mode is active
    audit: [],
    loadingAudit: false,
    usage: [],
    loadingUsage: false,
    screentime: emptyScreentime(),
    loadingScreentime: false,
    // How many completed days the report covers. `/api/screentime?days=N` has always accepted this
    // and been clamped to 1..=365 server-side; nothing in the interface ever sent it, so every
    // parent saw exactly thirty days forever.
    stDays: 30,
    // The day whose breakdown is pinned, or null to follow the most recent day carrying data.
    // Selecting is what makes the chart answer the question it invites — "what was that Saturday?"
    // — which it previously could not.
    stPinned: null,
    // `null` until an answer arrives, never `[]`. An empty array is a *fact* — the child has asked
    // for nothing — and the card is hidden on it. A failed fetch is not that fact, and the two used
    // to be the same value: on any error the card and its badge both vanished, leaving a dashboard
    // that looked exactly like a quiet evening. See `showRequests`.
    timeRequests: null,
    // Whether the first attempt has finished, succeeded or not. Without it the "unknown" state
    // renders for the few hundred milliseconds before the first response and the card flashes a
    // failure notice on every page load — the same trade `todayAsked` exists to make.
    requestsAsked: false,
    codes: [],
    loadingCodes: false,
    newCodeMins: 30,
    issuingCode: false,
    pwCurrent: "",
    pwNew: "",
    savingPw: false,
    toasts: [],
    version: "",
    latestVersion: "",
    latestUrl: "",
    updateState: "",
    updateError: "",
    checkingUpdate: false,

    async init() {
      try {
        const r = await fetch("/session");
        const j = await r.json();
        this.authed = !!j.authenticated;
        // The build running on the monitored PC. Displayed only -- nothing here goes
        // looking for a newer one, because that would mean this machine reaching out.
        this.version = j.version || "";
      } catch {
        this.authed = false;
      }
      if (this.authed) this.loadAll();
    },

    // Load every dashboard panel — used by both init() and a fresh login() so the two
    // entry points can't drift.
    loadAll() {
      this.loadProcesses();
      this.loadCurfew();
      this.loadRules();
      this.loadRoutines();
      this.loadToday();
      this.loadAudit();
      this.loadUsage();
      this.loadScreentime();
      this.loadTimeRequests();
      this.loadCodes();
      this.startPolling();
    },

    // Keep the live-changing panels (today's usage + pending requests) fresh without a
    // manual refresh. Light: two small JSON GETs a minute. Stops itself once logged out.
    startPolling() {
      this.stopPolling();
      this._pollTimer = setInterval(() => {
        if (!this.authed) { this.stopPolling(); return; }
        if (document.hidden) return; // skip fetches while the tab is backgrounded
        this.loadToday();
        this.loadTimeRequests();
      }, this._pollMs);
    },

    stopPolling() {
      if (this._pollTimer) { clearInterval(this._pollTimer); this._pollTimer = null; }
    },

    async loadCurfew() {
      try {
        const r = await fetch("/api/curfew");
        if (r.ok) {
          this.curfew = await r.json();
          if (!this.curfew.windows) this.curfew.windows = [];
        }
      } catch {}
    },

    // Which days a window actually applies on, as words.
    //
    // The rule this surfaces is a real inversion. `Days::includes` treats an all-false selector as
    // *every* day — correct for the data model, since an omitted `days` field has to mean "daily" —
    // but the markup offers seven live checkboxes, so a parent can clear them all. The natural
    // reading of that gesture is "no days"; the effect is the opposite. Clearing the last box while
    // exempting a weekend gives a machine that shuts down on exactly the two evenings you meant to
    // spare.
    //
    // It was disclosed in the faintest text on the card, below the whole set of windows, and it
    // never changed when a box was cleared. This says what the window will do, beside the window,
    // as it changes — state the parent can see instead of a rule they must recall.
    windowDayLabel(w) {
      const on = this.dayOptions.filter((d) => w.days && w.days[d.key]);
      if (on.length === 0) return "Applies: every day";
      if (on.length === this.dayOptions.length) return "Applies: every day";
      return "Applies: " + on.map((d) => d.full.slice(0, 3)).join(", ");
    },

    // True when the window applies daily *because nothing is ticked*, rather than because all seven
    // are. Same outcome, very different intent, and only the first is worth drawing attention to.
    windowDaysImplicit(w) {
      return !this.dayOptions.some((d) => w.days && w.days[d.key]);
    },

    addWindow() {
      this.curfew.windows.push({
        start: "22:00",
        end: "07:00",
        days: { mon: false, tue: false, wed: false, thu: false, fri: false, sat: false, sun: false },
      });
    },

    removeWindow(i) {
      this.curfew.windows.splice(i, 1);
    },

    async saveCurfew() {
      this.savingCurfew = true;
      try {
        const r = await this.postJSON("/api/curfew", this.curfew);
        if (r.ok) {
          this.toast("Curfew saved", "success");
        } else if (r.status === 400) {
          this.toast("Invalid times — use HH:MM", "error");
        } else {
          this.toast("Could not save curfew", "error");
        }
      } catch {
        this.toast("Save request failed", "error");
      } finally {
        this.savingCurfew = false;
      }
    },

    async loadRules() {
      try {
        const r = await fetch("/api/rules");
        if (r.ok) {
          this.rules = await r.json();
          if (!this.rules.blocklist) this.rules.blocklist = [];
          if (!this.rules.app_limits) this.rules.app_limits = {};
          if (!this.rules.app_groups) this.rules.app_groups = [];
          // Expand the app_limits map into editable rows.
          this.appLimitRows = Object.entries(this.rules.app_limits)
            .map(([name, mins]) => ({ name, mins }));
          // Expand groups into editable rows (apps shown as a comma-separated string).
          this.groupRows = this.rules.app_groups.map((g) => ({
            name: g.name, appsText: (g.apps || []).join(", "), limit_mins: g.limit_mins,
          }));
          // Drop any stashed per-day array so a later per-day toggle reflects THESE rules,
          // not a stale array from before (e.g. after applying a routine).
          this._lastPerDay = null;
        }
      } catch {}
    },

    // Switch between one everyday limit and a per-weekday array. Toggling on restores the
    // last per-day array if there is one (so an accidental off→on round-trip doesn't wipe a
    // weekend config), otherwise seeds all 7 days from the current everyday value.
    togglePerDay(on) {
      if (on) {
        const base = this.rules.daily_budget_mins || 0;
        this.rules.budget_by_weekday = this._lastPerDay || [base, base, base, base, base, base, base];
      } else {
        this._lastPerDay = this.rules.budget_by_weekday ? [...this.rules.budget_by_weekday] : null;
        this.rules.budget_by_weekday = null;
      }
    },

    // Is anything actually configured to enforce? Mirrors `Rules::has_targets()` on the
    // server — deliberately NOT `any_configured()`, which folds in the pause toggle; the
    // caller composes that separately to get the three-state badge.
    //
    // Reads the live form rows (not the last-saved `rules`) so the badge reacts as soon as
    // the parent types a limit, not only after Save. That's why this can't just be a field
    // on `GET /api/rules`.
    anyRulesSet() {
      const budget =
        Number(this.rules.daily_budget_mins) > 0 ||
        (Array.isArray(this.rules.budget_by_weekday) &&
          this.rules.budget_by_weekday.some((d) => Number(d) > 0));
      const blocked = (this.rules.blocklist || []).some((b) => (b || "").trim());
      const limits = (this.appLimitRows || []).some(
        (r) => (r.name || "").trim() && Number(r.mins) > 0,
      );
      const groups = (this.groupRows || []).some(
        (g) => (g.appsText || "").trim() && Number(g.limit_mins) > 0,
      );
      return budget || blocked || limits || groups;
    },

    // Absolute URL of the child's page, so it can be copied and sent verbatim.
    askUrl() {
      return `${location.origin}/ask`;
    },

    async copyAskUrl() {
      try {
        await navigator.clipboard.writeText(this.askUrl());
        this.toast("Link copied");
      } catch {
        this.toast("Couldn't copy — select the link instead", "error");
      }
    },

    // Fold the editable rows (per-app limits, blocklist, groups) back into `this.rules`.
    // Shared by Save and Save-as-routine so both capture exactly the on-screen settings.
    collapseRules() {
      this.rules.app_limits = {};
      for (const row of this.appLimitRows) {
        const name = (row.name || "").trim();
        if (name) this.rules.app_limits[name] = row.mins || 0;
      }
      this.rules.blocklist = this.rules.blocklist.map((b) => b.trim()).filter(Boolean);
      // Split each group's apps text on commas/whitespace; drop empty groups.
      this.rules.app_groups = this.groupRows
        .map((g) => ({
          name: (g.name || "").trim(),
          apps: (g.appsText || "").split(/[,\s]+/).map((a) => a.trim()).filter(Boolean),
          limit_mins: g.limit_mins || 0,
        }))
        .filter((g) => g.name && g.apps.length);
    },

    async saveRules() {
      this.savingRules = true;
      this.collapseRules();
      try {
        const r = await this.postJSON("/api/rules", this.rules);
        if (r.ok) {
          this.toast("Rules saved", "success");
          this.loadToday(); // keep the Today card (budget/paused badge) in sync
        } else if (r.status === 400) {
          this.toast("Warning seconds must be ≤ 600", "error");
        } else {
          this.toast("Could not save rules", "error");
        }
      } catch {
        this.toast("Save request failed", "error");
      } finally {
        this.savingRules = false;
      }
    },

    loadRoutines() {
      return this.loadList("/api/routines", "routines", "loadingRoutines", "Failed to load routines");
    },

    async saveRoutine() {
      const name = (this.newRoutineName || "").trim();
      if (!name) { this.toast("Enter a routine name", "error"); return; }
      this.savingRoutine = true;
      this.collapseRules();
      try {
        const r = await this.postJSON("/api/routines", { name, rules: this.rules });
        if (r.ok) {
          this.toast(`Saved routine "${name}"`, "success");
          this.newRoutineName = "";
          this.loadRoutines();
        } else if (r.status === 400) {
          this.toast("Invalid name, or too many routines (max 20)", "error");
        } else {
          this.toast("Could not save routine", "error");
        }
      } catch {
        this.toast("Request failed", "error");
      } finally {
        this.savingRoutine = false;
      }
    },

    async applyRoutine(name) {
      try {
        const r = await fetch(`/api/routines/${encodeURIComponent(name)}/apply`, { method: "POST" });
        if (r.ok) {
          this.toast(`Applied "${name}"`, "success");
          this.loadRules();
          this.loadToday();
        } else {
          this.toast("Could not apply routine", "error");
        }
      } catch {
        this.toast("Request failed", "error");
      }
    },

    async deleteRoutine(name) {
      try {
        const r = await fetch(`/api/routines/${encodeURIComponent(name)}/delete`, { method: "POST" });
        if (r.ok) {
          this.toast(`Deleted "${name}"`, "success");
          this.loadRoutines();
        } else {
          this.toast("Could not delete routine", "error");
        }
      } catch {
        this.toast("Request failed", "error");
      }
    },

    async login() {
      this.busy = true;
      this.loginError = "";
      try {
        const r = await this.postJSON("/login", { password: this.password });
        if (r.ok) {
          this.password = "";
          this.authed = true;
          this.loadAll();
        } else if (r.status === 429) {
          this.loginError = "Too many attempts — wait a minute and try again.";
        } else {
          this.loginError = "Incorrect password.";
        }
      } catch {
        this.loginError = "Could not reach the server.";
      } finally {
        this.busy = false;
      }
    },

    async logout() {
      this.stopAutoRefresh();
      this.stopPolling();
      await fetch("/logout", { method: "POST" });
      this.authed = false;
      if (this.shotUrl) { URL.revokeObjectURL(this.shotUrl); this.shotUrl = null; }
      // Forget everything the session fetched. Left set, the next sign-in shows the last one's
      // numbers as current until a fetch replaces them — and a window left open overnight makes
      // "current" mean yesterday. Clearing the data itself, not a flag about it, so there is one
      // thing to reset rather than two that must agree.
      //
      // This used to clear three fields and leave eight. `audit`, `usage`, `screentime`, `codes`,
      // `timeRequests`, `routines`, `rules` and `curfew` all survived a sign-out, so a second
      // person signing in on the same browser saw the first one's child. `resetSessionData` is one
      // place rather than clearing scattered across the callers — but it *is* still an inventory
      // that must be kept aligned with the state above, and its own doc says so. The alignment is
      // deliberate rather than fixed: the thirteen fields sit beside the comments explaining them
      // and beside their `loading*` partners, and hoisting them into a shared factory would buy one
      // inventory at the price of detaching every one of them from its reason.
      this.resetSessionData();
    },

    // Every field holding data fetched for a signed-in session, back to "nothing known".
    //
    // `null` where a later reader must tell "unknown" from "empty" (see `showRequests`), the empty
    // shape elsewhere. Anything added to the component that holds server data belongs here too —
    // the failure is silent, and shows up as a tab left open overnight presenting yesterday as now.
    //
    // `rules` and `curfew` are deliberately **not** reset. They describe the machine rather than the
    // session, they are the same for whoever signs in next, and they are bound to form inputs with
    // `x-model` — blanking them would flash empty fields and then a saved-looking form built from
    // defaults, which is a worse lie than a stale one.
    resetSessionData() {
      this.processes = [];
      this.today = null;
      this.todayAsked = false;
      this.timeRequests = null;
      this.requestsAsked = false;
      this.audit = [];
      this.usage = [];
      this.codes = [];
      this.routines = [];
      this.screentime = emptyScreentime();
      // The capture's own metadata. `shotUrl` is revoked by the caller (it owns a blob), but these
      // two describe it and would otherwise outlive it — leaving "updated 3s ago" under a picture
      // that has been cleared, or a stale-view warning on the login page.
      this.shotAt = null;
      this.shotTier = null;
      this.shotStale = false;
      // No frame, nothing to age. The clock now outlives the Live toggle, so this is the one place
      // that must stop it.
      this.stopShotClock();
      // The title outlives the component state, so a sign-out that left "(2) Nestwatch" up would
      // advertise the previous session's child from the login page.
      this.syncTitle();
    },

    async loadProcesses() {
      this.loadingProcs = true;
      try {
        const r = await fetch("/api/processes");
        if (r.status === 401) { this.authed = false; return; }
        this.processes = await r.json();
      } catch {
        this.toast("Failed to load processes", "error");
      } finally {
        this.loadingProcs = false;
      }
    },

    // Fetch one frame.
    //
    // `tier` is separate from `silent` because they answer different questions: `silent` is how
    // loudly a failure is reported, `tier` is how many pixels are asked for. They agree at every
    // call site today, but that is a fact about the call sites rather than a rule.
    // `live` is a third, separate question from `silent` and `tier`: *who asked*. Only the audit
    // reads it, and it must not be inferred from the other two. It happens to agree with `silent`
    // at every call site today, but that is a fact about the call sites -- the same trap that made
    // the server key its audit on `tier` and then break silently when the timer started asking for
    // full frames.
    async takeScreenshot(silent = false, tier = "full", live = false) {
      // Two callers, two rules. The live timer must never stack captures — the helper can take
      // ~15s while the timer fires as often as every 2s — so a silent tick skips while one is in
      // flight. A
      // person must never be ignored: their click IS the interaction, and the old shared guard
      // dropped it silently, with the button still looking enabled because `loadingShot` is only
      // set for non-silent captures. At a 15s worst case against the 5s default — 2s if the
      // parent picks it — that was the common case, not a race.
      if (this._shotBusy) {
        if (silent) return;
        // Supersede the frame in flight. This saves the DOWNLOAD — on the order of a megabyte at
        // full tier against ~25 KiB for a preview — and nothing else.
        //
        // Do NOT reach for the 20,641 KiB figure here. That is `control/mod.rs`'s measurement of
        // the same 4K frame as **PNG**, and this endpoint has served JPEG since the tiers landed;
        // the closest measured JPEG number is `Cargo.toml`'s 979 KiB at q70, and full tier encodes
        // at q90, so somewhat above that. The PNG figure is the memorable one in this codebase and
        // it is the wrong one for anything on the wire — it was cited here once already.
        //
        // `spawn_blocking` on the server is not cancelled when the connection
        // drops, so the helper it already started runs to completion and this click commissions a
        // SECOND desktop capture alongside it. Two concurrent captures is the price of not
        // dropping the click, and it is the deliberate trade: the click is the parent's, the frame
        // it supersedes is a timer's. It is also why the generation guard below exists as well as
        // this abort rather than instead of it — that first capture still finishes, and still
        // replies.
        if (this._shotAbort) this._shotAbort.abort();
      }
      const gen = ++this._shotGen;
      this._shotBusy = true;
      if (!silent) this.loadingShot = true;
      const ctrl = new AbortController();
      this._shotAbort = ctrl;
      try {
        const endpoint = "/api/screenshot?tier=" + tier + (live ? "&live=1" : "");
        const r = await fetch(endpoint, { signal: ctrl.signal });
        if (r.status === 401) { this.authed = false; this.stopAutoRefresh(); return; }
        if (!r.ok) throw new Error();
        const blob = await r.blob();
        // Superseded while in flight. Abort closes the connection, but a reply can still arrive
        // after a newer capture has started — the helper finished its work regardless. Letting it
        // land would replace a newer frame with an older one and mislabel the tier with it.
        //
        // Checked BEFORE `createObjectURL`, so the superseded path never mints a URL it has to
        // revoke on the next line. The blob itself is collected with the response.
        if (gen !== this._shotGen) return;
        const url = URL.createObjectURL(blob);
        if (this.shotUrl) URL.revokeObjectURL(this.shotUrl);
        this.shotUrl = url;
        this.shotAt = Date.now();
        // What the server says it served, not what was asked for. `ShotTier::from_arg` maps
        // unknown and absent alike to full, so a typo in the query string would otherwise
        // stream full frames on a two-second timer while this recorded "preview" -- and
        // `openShotFull` reads this value to decide whether the frame is already sharp.
        // Falls back to the request so an older service without the header still works.
        this.shotTier = (r.headers && r.headers.get("X-Shot-Tier")) || tier;
        this.shotStale = false;
        // There is a frame on screen now, so the age line under it needs a clock — whatever put
        // the frame there. See `startShotClock`.
        this.startShotClock();
      } catch (e) {
        // An abort is the parent switching Live off mid-capture. Nothing failed, so it must not
        // raise a toast or mark the picture stale.
        if (e && e.name === "AbortError") return;
        // A superseded capture's failure belongs to nobody: the parent is already watching a newer
        // one, so neither a toast nor a stale mark should come from it.
        if (gen !== this._shotGen) return;
        // A failed LIVE frame is the case this whole flag exists for. It used to be swallowed
        // entirely, so a stopped service, a signed-out child or a wedged helper all left the last
        // good picture on screen with the toggle still on, indefinitely.
        if (silent) this.shotStale = true;
        else this.toast("Screenshot failed", "error");
      } finally {
        if (this._shotAbort === ctrl) this._shotAbort = null;
        // Only the current capture may release the shared flags. Without this a superseded reply
        // clears the spinner and the busy flag belonging to the capture that replaced it — which
        // is exactly the "dropped click traded for out-of-order frames" the naive fix produces.
        if (gen === this._shotGen) {
          if (!silent) this.loadingShot = false;
          this._shotBusy = false;
        }
      }
    },

    toggleAutoRefresh() {
      if (this.autoRefresh) this.startAutoRefresh();
      else this.stopAutoRefresh();
    },

    startAutoRefresh() {
      this.stopAutoRefresh(); // never stack two timers when the cadence changes mid-session
      this.autoRefresh = true;
      this.shotStale = false;
      this._liveUntil = Date.now() + this._liveMaxMs;
      // The first frame is full so switching Live on gives an immediately sharp picture and
      // surfaces a failure at once; every frame after it is a preview.
      this.takeScreenshot();
      this._armShotTimer();
    },

    // Arm, or re-arm, the capture timer at the current cadence.
    //
    // Split out of `startAutoRefresh` because changing the cadence must not restage the session:
    // that path aborts the in-flight capture and commissions a **full-tier** one, so clicking
    // "15s" to make the live view cheaper bought the most expensive capture available.
    _armShotTimer() {
      if (this._shotTimer) clearInterval(this._shotTimer);
      // Skip while the tab is hidden, matching the data poll. Each tick spawns a helper
      // in the child's session to capture and encode their whole desktop — by far
      // the most expensive thing this tool does — and without the guard it kept doing it
      // on their laptop for as long as the parent left the tab open in a pocket.
      this._shotTimer = setInterval(() => this._liveTick(), this._refreshMs);
    },

    // One tick of the live timer. A method rather than a closure so the decisions in it can be
    // tested; the interval callback that used to hold this body could not be reached from a test.
    _liveTick() {
      if (Date.now() > this._liveUntil) {
        this.stopAutoRefresh();
        return;
      }
      // A hidden tab is nobody watching, and a frame costs a whole helper process in the child's
      // session. The `typeof` guard is for the tests, which have no document.
      if (typeof document !== "undefined" && document.hidden) return;
      this.takeScreenshot(true, this.liveTier(), true);
    },

    // Which tier a live frame should be: the one the surface currently on screen needs.
    //
    // The tiers were introduced on an explicit promise -- a parent who wants to READ something can
    // still get a full-resolution frame. That held only while Live was OFF. With it on, the timer
    // overwrote the overlay's sharp frame within one refresh interval, and Live being on is exactly
    // the state a parent is in when they press Expand. The tier was being decided by *who asked for
    // the frame* when the property that matters is *which surface is displaying it*.
    //
    // The cost is real and deliberate: a full frame is roughly thirty times a preview's bytes, so
    // this is the most expensive thing the tool can be asked to do. It is bounded three ways -- the
    // overlay is a transient state a person opened, `_liveUntil` still caps an unattended session,
    // and the cadence selector is on the card if the parent would rather trade rate for cost.
    //
    // The rejected alternative was giving the overlay its own buffer that only human captures
    // write. That keeps a *stale* sharp frame: a picture that looks live and is not, which is the
    // failure mode this codebase exists to avoid rather than a fix for it.
    liveTier() {
      return this.shotFull ? "full" : "preview";
    },

    // Change cadence without making the parent toggle Live off and on.
    setRefreshMs(ms) {
      this._refreshMs = ms;
      if (!this.autoRefresh) return;
      // Re-arming pushes the auto-stop out again, exactly as restarting the session used to: a
      // parent adjusting the cadence is plainly still watching, and should not be cut off because
      // they touched a button at minute fourteen.
      this._liveUntil = Date.now() + this._liveMaxMs;
      this._armShotTimer();
    },

    stopAutoRefresh() {
      if (this._shotTimer) { clearInterval(this._shotTimer); this._shotTimer = null; }
      // Drop an in-flight capture on the floor: its frame would land after the parent said stop.
      if (this._shotAbort) { this._shotAbort.abort(); this._shotAbort = null; }
      // The age clock deliberately keeps running. The picture is still on screen and still getting
      // older, and this is the path the fifteen-minute auto-stop takes — see `startShotClock`.
      this.autoRefresh = false;
    },

    // Ticks `now` so the age under the picture counts up by itself.
    //
    // Runs whenever a frame is on screen, **not** only while Live is on. Bound to the toggle it
    // lied: the fifteen-minute auto-stop calls `stopAutoRefresh`, which stopped this clock without
    // setting `shotStale`, so the age froze at "updated 4s ago" — in `opacity-60`, over a picture
    // by then hours old. That is not silence, it is a confident wrong answer, and it is the exact
    // failure the line was added to prevent. Following `shotAt` makes every stop path truthful.
    //
    // Idempotent, so landing a frame does not restart the interval.
    startShotClock() {
      if (this._clockTimer) return;
      this._clockTimer = setInterval(() => { this.now = Date.now(); }, 1000);
    },

    stopShotClock() {
      if (this._clockTimer) { clearInterval(this._clockTimer); this._clockTimer = null; }
    },

    // "updated 4s ago", or a plain statement that it is not updating any more.
    //
    // Deliberately says "not updating" rather than staying silent: the whole failure being
    // addressed is that a frozen live view and a motionless child look identical.
    shotAge() {
      if (!this.shotAt) return "";
      const secs = Math.max(0, Math.round((this.now - this.shotAt) / 1000));
      const ago = secs < 60
        ? secs + "s ago"
        : Math.floor(secs / 60) + "m " + (secs % 60) + "s ago";
      if (this.shotStale) return "not updating — last frame " + ago;
      return "updated " + ago;
    },

    // Tone for the age line, so a stalled view is not merely readable but visible.
    shotAgeClass() {
      return this.shotStale ? "text-error" : "opacity-60";
    },

    askKill(p) { this.killTarget = p; },

    async killProcess() {
      const p = this.killTarget;
      this.killTarget = null;
      if (!p) return;
      try {
        const r = await fetch(`/api/processes/${p.pid}/kill`, { method: "POST" });
        if (r.ok) {
          this.toast(`Closed ${p.name}`, "success");
          this.loadProcesses();
        } else {
          this.toast(`Could not close ${p.name}`, "error");
        }
      } catch {
        this.toast("Kill request failed", "error");
      }
    },

    async doShutdown() {
      this.confirmShutdown = false;
      try {
        const r = await fetch("/api/shutdown", { method: "POST" });
        this.toast(r.ok ? "Shutting down…" : "Shutdown failed", r.ok ? "success" : "error");
      } catch {
        this.toast("Shutdown request failed", "error");
      }
    },

    // Compare two dotted versions numerically. "0.10.0" is newer than "0.9.0", which a
    // string compare gets backwards -- and will bite the first time a minor reaches 10.
    // Missing or non-numeric parts count as 0, so "1.2" and "1.2.0" are equal.
    compareVersions(a, b) {
      const parts = (v) => String(v).replace(/^v/, "").split(".").map((n) => parseInt(n, 10) || 0);
      const [x, y] = [parts(a), parts(b)];
      for (let i = 0; i < Math.max(x.length, y.length); i++) {
        const d = (x[i] || 0) - (y[i] || 0);
        if (d !== 0) return d < 0 ? -1 : 1;
      }
      return 0;
    },

    // Ask GitHub what the latest release is.
    //
    // Runs in the parent's browser, on the parent's own device, and only on a click. The
    // monitored PC makes no outbound connection -- it serves this page and nothing else,
    // which is what "nothing leaves the house" means. Nothing is fetched on page load, so
    // simply opening the dashboard contacts no one.
    async checkForUpdate() {
      this.checkingUpdate = true;
      this.updateState = "";
      this.updateError = "";
      try {
        const r = await fetch(
          "https://api.github.com/repos/emrecdr/nestwatch/releases/latest",
          { headers: { Accept: "application/vnd.github+json" } }
        );
        if (r.status === 403 || r.status === 429) {
          // 60 requests an hour per IP, unauthenticated. Worth naming rather than
          // reporting as a generic failure, since waiting genuinely fixes it.
          throw new Error("GitHub is rate-limiting this device — try again in a few minutes.");
        }
        if (!r.ok) throw new Error("GitHub returned " + r.status + ".");
        const j = await r.json();
        const tag = (j.tag_name || "").replace(/^v/, "");
        if (!tag) throw new Error("GitHub did not report a version.");
        this.latestVersion = tag;
        this.latestUrl = j.html_url || "https://github.com/emrecdr/nestwatch/releases/latest";
        const d = this.compareVersions(this.version, tag);
        this.updateState = d < 0 ? "newer" : d > 0 ? "ahead" : "current";
      } catch (e) {
        // Offline, blocked, or CSP-refused. This is a convenience -- say what happened and
        // leave the manual link, rather than implying anything is wrong with the install.
        this.updateState = "failed";
        this.updateError =
          "Could not reach GitHub from this device (" +
          (e?.message || "network error") +
          ") — use the releases link instead.";
      } finally {
        this.checkingUpdate = false;
      }
    },

    // Open the full-size view, refetching at full tier first.
    //
    // The thumbnail and this overlay bind the same `shotUrl`, so while Live is running that value
    // holds a 960x540 preview, and opening the overlay on it would stretch a preview across the
    // whole window at the moment the parent is looking hardest.
    //
    // The overlay is shown *before* the fetch resolves, so it opens instantly with the frame
    // already on screen and sharpens a moment later, rather than pausing on a click.
    //
    // While Live runs, the timer keeps this sharp rather than overwriting it: `liveTier()` returns
    // "full" for as long as the overlay is open. It used to return a preview regardless of what was
    // on screen, so the frame fetched here survived at most one refresh interval and was then
    // replaced by a 960x540 preview stretched across the window -- at the moment the parent was
    // looking hardest. See `liveTier` for what that costs and why it is bounded.
    openShotFull() {
      this.shotFull = true;
      // Only when the frame on screen is not already a full one. Pressing "Take screenshot" and
      // then Expand used to commission a second complete capture — helper process, whole desktop,
      // resize, encode, pipe, 15s watchdog — for bytes the browser already had.
      if (this.shotTier !== "full") this.takeScreenshot();
    },

    // Close the full-size view, and leave real fullscreen if we entered it -- otherwise the
    // browser stays fullscreen over a dashboard the parent can no longer see the chrome of.
    closeShotFull() {
      this.shotFull = false;
      if (document.fullscreenElement) {
        document.exitFullscreen().catch(() => {});
      }
    },

    // True fullscreen, for "as big as the monitor gets". Separate from the overlay on
    // purpose: the overlay always works, while requestFullscreen needs a user gesture and
    // can be refused outright by policy. If it fails the overlay is still up and usable,
    // so the failure costs nothing and does not need reporting.
    toggleBrowserFullscreen(el) {
      if (document.fullscreenElement) {
        document.exitFullscreen().catch(() => {});
      } else if (el?.requestFullscreen) {
        el.requestFullscreen().catch(() => {});
      }
    },

    async doLock() {
      try {
        const r = await fetch("/api/lock", { method: "POST" });
        this.toast(r.ok ? "Screen locked" : "Lock failed", r.ok ? "success" : "error");
      } catch {
        this.toast("Lock request failed", "error");
      }
    },

    async changePassword() {
      this.savingPw = true;
      try {
        const r = await this.postJSON("/api/password", { current: this.pwCurrent, new: this.pwNew });
        if (r.ok) {
          this.toast("Password changed", "success");
          this.pwCurrent = "";
          this.pwNew = "";
        } else if (r.status === 401) {
          this.toast("Current password is wrong", "error");
        } else if (r.status === 400) {
          // The server explains exactly what is wrong (how many characters it counted, or
          // which guessable pattern it matched). This used to repeat a fixed "too short",
          // which told the parent the wrong thing whenever length was not the problem.
          // `postJSON` hands back the raw Response, so read the body -- guarded, because a
          // non-JSON error body must not turn a rejection into a thrown exception.
          let detail = "";
          try { detail = (await r.json())?.error || ""; } catch {}
          this.toast(detail || "That password was rejected", "error");
        } else {
          this.toast("Could not change password", "error");
        }
      } catch {
        this.toast("Password request failed", "error");
      } finally {
        this.savingPw = false;
      }
    },

    toast(msg, kind) {
      const id = (this._n = (this._n || 0) + 1);
      const cls = { success: "alert-success", error: "alert-error", info: "alert-info" }[kind] || "alert-info";
      this.toasts.push({ id, msg, cls });
      setTimeout(() => { this.toasts = this.toasts.filter((t) => t.id !== id); }, 3500);
    },

    // POST a JSON body; the caller inspects the returned Response for status handling.
    postJSON(url, body) {
      return fetch(url, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
    },

    // GET a list into `this[field]`. `flag` (optional) toggles a loading spinner; `errMsg`
    // (optional) toasts on failure. Logs out on 401.
    //
    // `this[field]` is assigned only on success, so the field itself answers "did this arrive"
    // and no caller needs a second flag for it — start it at `null`/`[]` and let its own emptiness
    // be the signal, the way every panel here already does.
    async loadList(url, field, flag, errMsg) {
      if (flag) this[flag] = true;
      try {
        const r = await fetch(url);
        if (r.status === 401) { this.authed = false; return; }
        // Throw on a failed status so it lands in the same catch a network fault does. `if (r.ok)`
        // alone meant an errored server was indistinguishable from an empty one: the field kept
        // its old value, no toast fired, and every caller's `errMsg` was dead code for the failure
        // it actually names. Cards gated on `length > 0` then render as "nothing to show".
        if (!r.ok) throw new Error(`HTTP ${r.status}`);
        this[field] = await r.json();
      } catch {
        if (errMsg) this.toast(errMsg, "error");
      } finally {
        if (flag) this[flag] = false;
      }
    },

    loadAudit() { return this.loadList("/api/audit", "audit", "loadingAudit", "Failed to load access log"); },
    loadUsage() { return this.loadList("/api/usage", "usage", "loadingUsage", "Failed to load usage history"); },
    async loadToday() {
      await this.loadList("/api/usage/today", "today", "loadingToday");
      // Set even when the fetch failed: loadList swallows that error, and a failure is exactly
      // when the staleness warning needs to be reachable. Whether the figures *arrived* needs no
      // flag beside it — `today` is null until they do, and stays at its last real value if a
      // later refresh fails, which is stale rather than imaginary and already has its own warning.
      this.todayAsked = true;
    },
    // Choose a theme. "auto" clears the override and follows the device again.
    setTheme(theme) {
      this.theme = theme;
      try {
        if (theme === "auto") localStorage.removeItem(THEME_KEY);
        else localStorage.setItem(THEME_KEY, theme);
      } catch {
        // Storage refused. The choice still applies to this page; it just will not be remembered,
        // which is a better outcome than refusing to change the theme at all.
      }
      applyTheme(theme);
    },

    // --- Written for Alpine's CSP build ---------------------------------------------------------
    //
    // That build parses attribute expressions with its own small parser instead of handing them to
    // `new Function`, which is the whole reason `script-src` can drop `'unsafe-eval'`. Four
    // constructs it cannot parse, established by probing the build rather than by reading — the
    // documentation is silent on two of them, and this entry's own history is a confident claim
    // about `x-model` that died on contact with the code:
    //
    //     `?.`     CSP Parser Error: Unexpected token: PUNCTUATION "."
    //     `??`     CSP Parser Error: Unexpected token: PUNCTUATION "?"
    //     backtick CSP Parser Error: Unexpected token: OPERATOR
    //     [...x]   renders nothing at all, with no error
    //
    // Everything in this block exists to move one of those out of an attribute and into JavaScript,
    // where all four are ordinary. Property paths, ternaries, comparisons, method calls with
    // arguments, assignment, `x-model` and array literals *do* parse, and are left in the markup —
    // the migration is deliberately the smallest one that works, not a wholesale move of logic.
    //
    // The `?.` here is load-bearing and must not be "simplified" by making `today` a zeroed object
    // again: that placeholder is what the card used to read out as measurement, telling a parent
    // "0 min used today" on a dashboard that had never reached the service.
    get isPaused() { return this.today?.enabled === false; },
    get hasPerApp() { return !!this.today?.per_app?.length; },
    get focusMissing() { return !!this.today?.focus_missing; },
    get hasFocused() { return !!this.today?.focused?.length; },
    get hasPages() { return !!this.today?.pages?.length; },
    get hasGroups() { return !!this.today?.groups?.length; },
    get killTargetName() { return this.killTarget?.name ?? ""; },
    get killTargetPid() { return this.killTarget?.pid ?? ""; },

    todayUsedOrZero() { return this.today?.used_mins ?? 0; },
    bonusLabel() { return " (incl. +" + this.today.extra_mins + " bonus)"; },
    todayBarLabel() {
      return "Screen time: " + this.today.used_mins + " of " + this.today.budget_mins + " minutes used";
    },
    // Both the per-app and per-group rows carry `{name, used_mins, limit_mins}`, so one label
    // serves both rather than two that could drift.
    limitLabel(row) {
      return row.name + ": " + row.used_mins + " of " + row.limit_mins + " minutes";
    },

    stTotalLabel() { return this.screentime.total_mins + " min"; },
    stAvgLabel() {
      const a = this.screentime.daily_avg_mins;
      return a == null ? "—" : a + " min";
    },
    stMeasuredLabel() {
      return this.screentime.measured_days + "/" + this.screentime.days.length;
    },
    stTotalsHeading(what) { return what + " over " + this.stDays + " days"; },

    // Newest first for the table, oldest first for the chart — the same days read in opposite
    // orders, which is deliberate and explained at the call site.
    get stDaysNewestFirst() { return this.screentime.days.slice().reverse(); },

    // `x-for` over a possibly-absent list. The markup cannot say `?? []`, and iterating `undefined`
    // renders nothing *silently*, which is the failure this whole file keeps being about.
    stRows(key) {
      const day = this.stDayFor(key);
      return (day && day[key]) || [];
    },
    get timeRequestRows() { return this.timeRequests || []; },
    requestBadgeCount() {
      const n = this.requestCount();
      return n === null ? "?" : n;
    },

    // Does the curfew have any hours that could actually fire?
    //
    // The same three-state honesty the rules card needs, for the same reason: `enabled` only means
    // "not switched off". A window whose start equals its end is never active — `is_within` treats
    // it as empty — so a curfew can be on and still do nothing, and saying "On" there tells a
    // parent the PC has a bedtime when it does not.
    curfewHasHours() {
      const windows = this.curfew.windows;
      if (windows && windows.length) {
        return windows.some((w) => w.start && w.end && w.start !== w.end);
      }
      return !!(this.curfew.start && this.curfew.end && this.curfew.start !== this.curfew.end);
    },

    // What the switch beside "Curfew" is doing, in words.
    //
    // It was a bare toggle with only an `aria-label`, which names it for a screen reader and leaves
    // a sighted parent guessing — and this is the control that decides whether a child's PC powers
    // itself off at night. The equivalent switch on the rules card has carried visible state all
    // along; these two doing the same job and looking different was the oversight.
    curfewStateLabel() {
      if (!this.curfew.enabled) return "Off";
      return this.curfewHasHours() ? "On" : "On — no hours set";
    },

    // The three answers a parent opens this page for, in one place above everything else.
    //
    // They were spread across three non-adjacent cards, one of them below the fold on any phone —
    // and a phone is what the onboarding hands you, since `install` prints a QR precisely because
    // typing an IP on a phone keyboard is the worst friction in setup. Source order was standing in
    // for priority order.
    //
    // Pure and separately testable, because each has a state that is neither good nor bad but
    // *unknown*, and that is the one every earlier version of this page got wrong.
    glanceEnforcement() {
      if (!this.todayAsked) return { text: "Enforcement: checking…", tone: "muted" };
      if (this.stEnforcementStale()) return { text: "Enforcement may not be running", tone: "bad" };
      if (this.today && this.today.enabled === false) return { text: "Enforcement paused", tone: "warn" };
      return { text: "Enforcement running", tone: "good" };
    },

    glanceToday() {
      if (!this.today) return { text: "Today: not known yet", tone: "muted" };
      const used = this.today.used_mins ?? 0;
      if (!(this.today.budget_mins > 0)) {
        return { text: `Today: ${this.fmtDuration(used)} used, no limit set`, tone: "muted" };
      }
      const left = this.today.remaining_mins;
      const tone = left === 0 ? "bad" : left <= 15 ? "warn" : "good";
      return { text: `Today: ${this.fmtDuration(used)} used, ${this.fmtDuration(left)} left`, tone };
    },

    glanceRequests() {
      const n = this.requestCount();
      if (n === null) return this.requestsAsked
        ? { text: "Requests: not known", tone: "warn" }
        : { text: "Requests: checking…", tone: "muted" };
      if (n === 0) return { text: "Nothing waiting for you", tone: "muted" };
      return { text: n === 1 ? "1 request waiting" : `${n} requests waiting`, tone: "warn" };
    },

    // One place mapping a tone to its classes, so the three above stay free of styling and the
    // markup stays free of conditionals.
    glanceClass(tone) {
      if (tone === "bad") return "text-error font-semibold";
      if (tone === "warn") return "text-warning font-semibold";
      if (tone === "good") return "text-success";
      return "opacity-70";
    },

    // The page title, given a pending count. Pure, so the decision is testable; `syncTitle` below
    // is the two lines that touch the document.
    //
    // A tab title is the whole mechanism available here, and that is a consequence of the product
    // rather than a shortcut. Web Push needs an external push service, which "nothing leaves the
    // house" forbids outright. The Badging API — `navigator.setAppBadge` — badges an *installed*
    // app's icon, and `docs/MOBILE-APP.md` already establishes that an installable page cannot work
    // for this product, because a home-screen app does not inherit the browser's certificate
    // exception. The Notifications API needs a secure context, and whether a self-signed
    // certificate accepted on a private IP counts as one is **unverified** — localhost is the
    // documented exception, not private addresses generally. So: the title, which needs no
    // permission, no service worker and no external anything.
    //
    // `null` is unknown, not zero. Titling the tab "(0)" for a service we could not reach is the
    // same false confidence the badge used to show.
    titleFor(pending) {
      if (pending === null || pending === undefined) return "Nestwatch";
      return pending > 0 ? `(${pending}) Nestwatch` : "Nestwatch";
    },

    // Reflect the pending count in the tab title. Guarded rather than assumed: the component is
    // also evaluated by the tests, where there is no document, and a method that throws there would
    // make every caller untestable rather than marking itself as needing a browser.
    syncTitle() {
      if (typeof document === "undefined") return;
      document.title = this.titleFor(this.requestCount());
    },

    // Whether the more-time requests card belongs on screen.
    //
    // Three states, not two, and the third is the one this exists for. `[]` means the service
    // answered and the child has asked for nothing — hide the card, it is only clutter. `null`
    // *after* an attempt means the service did not answer, and hiding on that is how a pending
    // request became invisible: the card and its badge were both gated on `length > 0`, so a failed
    // fetch removed the only surface on which anything could have been noticed. `null` *before* an
    // attempt is simply "too early", and shows nothing rather than flashing a failure on load.
    showRequests() {
      if (this.timeRequests === null) return this.requestsAsked;
      return this.timeRequests.length > 0;
    },

    // How many are waiting, or null when that is not known. Kept separate from `showRequests` so
    // the badge and the card cannot disagree about the same three states.
    requestCount() {
      return this.timeRequests === null ? null : this.timeRequests.length;
    },

    // Badge text. "?" rather than a number when the answer did not arrive — a badge is an assertion,
    // and asserting zero for a service we could not reach is the whole bug.
    requestBadge() {
      const n = this.requestCount();
      if (n === null) return "requests ?";
      return n === 1 ? "1 request" : `${n} requests`;
    },

    loadScreentime() {
      return this.loadList(
        `/api/screentime?days=${this.stDays}`,
        "screentime",
        "loadingScreentime",
        "Failed to load screen-time report",
      );
    },

    // Change the window and reload. The pin is dropped because the day it named may not be in the
    // new window, and a pinned date that is no longer on the chart is a heading pointing at nothing.
    setStDays(days) {
      this.stDays = days;
      this.stPinned = null;
      return this.loadScreentime();
    },

    // Pin a day, or unpin it by choosing it again. Toggling on re-click is what makes the chart
    // usable without a separate clear control for the common case.
    toggleStDay(date) {
      this.stPinned = this.stPinned === date ? null : date;
    },

    // The day a breakdown should show for `key` ("apps" | "focused" | "pages").
    //
    // With nothing pinned this is the old behaviour, and the old reasoning holds: each list picks
    // the most recent day carrying *that* kind of data, independently, because a day can have
    // running-app figures and no focus figures and rendering an empty focus panel under a date that
    // does have focus data elsewhere reads as "he looked at nothing" rather than "nothing was
    // watching".
    //
    // With a day pinned, that independence is exactly wrong: the parent asked about one date, and
    // three panels quietly showing three different dates would answer a question nobody asked. So a
    // pin overrides all three, and a panel with nothing for that day says so rather than substitute.
    stDayFor(key) {
      if (this.stPinned !== null) {
        return this.screentime.days.find((d) => d.date === this.stPinned) ?? null;
      }
      return this.stRecentDayWith(key);
    },

    // Minutes as something a person reads. Over a month a heavy app reaches four digits, and
    // "1847 min" is a number you have to do arithmetic on before it means anything.
    fmtDuration(mins) {
      const m = Number(mins) || 0;
      if (m < 60) return m + " min";
      const h = Math.floor(m / 60);
      const rest = m % 60;
      return rest === 0 ? h + " h" : h + " h " + rest + " min";
    },

    // --- today's usage timeline ---------------------------------------------
    //
    // When the machine was actually in use today, as spans on a 24-hour axis. Every figure the
    // report offers answers *how much*; this is the only one that answers *when*, which is the
    // question a parent arrives with at 2am.
    //
    // Derived entirely from `session_start` / `session_stop` in `this.usage`, which `loadUsage()`
    // already fetches on sign-in — no endpoint, no extra request, no new storage.
    //
    // **Pairing is the whole difficulty and it must fail safe.** A `session_start` with no stop
    // before the next start means the enforcer died between them (a crash, an upgrade, `sc stop`)
    // and the end of that span is genuinely unknowable. Pairing across it would shade a bar from an
    // afternoon crash through to bedtime and call it use — which is the bug this feature was
    // blocked on, wearing a different hat. Such a span is marked `unknown` and drawn as a marker
    // with no width, never as a duration.

    // A Date's LOCAL calendar day, as YYYY-MM-DD. Never `toISOString()`, which is UTC and would
    // file a 01:00 session under yesterday. Written once because the timeline compares two
    // independently built day stamps: if one site took the toISOString shortcut they would stop
    // agreeing and every span would vanish with nothing failing — `dayTimeline` is tested against
    // a literal `dayISO`, never through the getter that builds one.
    localDayISO(d) {
      return d.getFullYear() + "-" +
        String(d.getMonth() + 1).padStart(2, "0") + "-" +
        String(d.getDate()).padStart(2, "0");
    },

    // Local minutes from midnight, or `null` if the event is not from `dayISO` (or is unparseable).
    minutesIntoDay(ts, dayISO) {
      if (!ts) return null;
      const d = new Date(ts);
      if (isNaN(d.getTime())) return null;
      // Compare in local time, because the axis is the parent's day, not UTC's.
      if (this.localDayISO(d) !== dayISO) return null;
      return d.getHours() * 60 + d.getMinutes();
    },

    // Spans for `dayISO`. `nowMin` bounds a still-open session so a live one does not run to
    // midnight. Pure: takes the events rather than reading `this.usage`, so it is testable.
    dayTimeline(events, dayISO, nowMin) {
      const rows = [];
      for (const e of events || []) {
        if (e.event !== "session_start" && e.event !== "session_stop") continue;
        const at = this.minutesIntoDay(e.ts, dayISO);
        if (at === null) continue;
        rows.push({ at: at, start: e.event === "session_start" });
      }
      // `/api/usage` is newest-first; spans need oldest-first. Stable, so two events sharing a
      // minute keep their relative order.
      rows.sort((a, b) => a.at - b.at);

      const spans = [];
      let open = null;
      for (const r of rows) {
        if (r.start) {
          // A second start with one already open: the enforcer went away in between and the end of
          // that span is unknowable. Never stretch it to here.
          if (open !== null) spans.push({ from: open, to: open, kind: "unknown" });
          open = r.at;
        } else if (open !== null) {
          spans.push({ from: open, to: r.at, kind: "use" });
          open = null;
        }
        // A stop with nothing open is discarded: it belongs to a session that began yesterday, and
        // this axis is one day wide.
      }
      if (open !== null) spans.push({ from: open, to: Math.max(open, nowMin), kind: "live" });
      return spans;
    },

    // The spans for today, ready to render.
    get todayTimeline() {
      const d = new Date();
      return this.dayTimeline(this.usage, this.localDayISO(d), d.getHours() * 60 + d.getMinutes());
    },

    // Percent-of-day geometry, so the markup needs no arithmetic (and no template literal, which
    // the CSP build cannot parse in an attribute).
    spanStyle(s) {
      const left = (s.from / 1440) * 100;
      // Every span gets a hairline floor: a single minute is 0.07% of the axis and an unknown-end
      // marker is 0%, and both would be invisible. `unknown` needs no branch of its own —
      // `dayTimeline` builds it with `from === to`, which this floor already covers.
      const width = Math.max(0.4, ((s.to - s.from) / 1440) * 100);
      return "left:" + left.toFixed(3) + "%;width:" + width.toFixed(3) + "%";
    },

    // Colour is **reinforcement here, never the carrier.** Measured in Chrome on the dark theme:
    // `bg-primary` (159,232,141) against `bg-success` (98,239,189) is a contrast ratio of **1.01**
    // — identical luminance differing only in hue, and green-against-teal is the textbook
    // deuteranopia confusion pair. `bg-warning` sits at 1.04 against primary, no better. A reader
    // who cannot separate those hues would have had nothing at all to go on.
    //
    // So each kind is distinguishable by **shape** before colour is considered:
    //   * `unknown` is a hairline, because its duration is unknown rather than short — see
    //     `spanStyle`, which gives it no width on purpose.
    //   * `live` carries a ring, the same device the screen-time chart already uses to mark a
    //     pinned day.
    //   * `use` is a plain filled bar.
    // Every span also carries a `title` and a line in the visually-hidden list beside the chart.
    spanClass(s) {
      if (s.kind === "unknown") return "bg-warning";
      if (s.kind === "live") return "bg-success ring-2 ring-inset ring-base-content";
      return "bg-primary";
    },

    // Read aloud, and on hover. An unknown end must say so rather than implying a duration.
    spanLabel(s) {
      const hhmm = (m) => String(Math.floor(m / 60)).padStart(2, "0") + ":" +
                          String(m % 60).padStart(2, "0");
      if (s.kind === "unknown") {
        return "In use from " + hhmm(s.from) + " — end unknown, the service stopped";
      }
      if (s.kind === "live") return "In use from " + hhmm(s.from) + ", still on";
      return "In use " + hhmm(s.from) + " to " + hhmm(s.to);
    },

    // Six labelled ticks — every four hours. More would not fit on a phone.
    get dayTicks() {
      const out = [];
      for (let h = 0; h < 24; h += 4) {
        out.push({ h: h, label: String(h).padStart(2, "0") + ":00", pct: (h / 24) * 100 });
      }
      return out;
    },

    // A key, because three encoded colours without one is a puzzle. Only the kinds actually
    // present are listed — a legend naming states the day does not contain is its own noise.
    get timelineKey() {
      const kinds = {};
      for (const s of this.todayTimeline) kinds[s.kind] = true;
      const all = [
        { kind: "use", label: "In use" },
        { kind: "live", label: "In use now" },
        { kind: "unknown", label: "End unknown — the service stopped" },
      ];
      return all.filter((k) => kinds[k.kind]);
    },

    tickStyle(t) {
      return "left:" + t.pct.toFixed(3) + "%";
    },

    // --- first-seen notice -------------------------------------------------
    //
    // Three states, and the UI must not merge them: `null` means the report could not tell (no
    // focus history, only one day of it, or a baseline too large to hold); an empty list means it
    // checked and nothing was new; a non-empty list is the notice.

    get firstSeen() {
      const st = this.screentime;
      if (!st) return null;
      return st.first_seen || null;
    },

    // Only worth showing when something actually turned up. "Nothing new" is the normal case and
    // does not deserve a panel — a notice that is present every day stops being read.
    get showFirstSeen() {
      const fs = this.firstSeen;
      return !!fs && fs.apps && fs.apps.length > 0;
    },

    // The claim's strength, stated rather than implied. "New, against 40 days" and "new, against
    // 1 day" are different statements and only the parent can weigh them.
    firstSeenNote() {
      const fs = this.firstSeen;
      if (!fs) return "";
      const days = fs.baseline_days === 1 ? "1 earlier day" : fs.baseline_days + " earlier days";
      return "First seen " + fs.date + ", against " + days + " of history";
    },

    // Named separately from the list because the count can exceed what is shown.
    firstSeenHeading() {
      const fs = this.firstSeen;
      if (!fs) return "";
      const n = fs.count;
      const noun = n === 1 ? "app" : "apps";
      if (n > fs.apps.length) {
        return n + " new " + noun + ", showing the " + fs.apps.length + " most used";
      }
      return n + " new " + noun;
    },

    // Which game portal a page title names, or "" if none is recognised.
    //
    // The product question is "an evening of Roblox or an evening of homework". Native Roblox is
    // already exact -- it has a process name. Browser portals had nothing, and they are what a
    // child reaches for when the native app is blocked. This costs no Win32 call, no COM, no
    // browser reconfiguration and no privacy escalation: the watcher already has the title.
    //
    // Two limits that must reach the reader, not just this comment: a renamed tab defeats it, and
    // so does any portal not in `GAME_PORTALS`. No match therefore means "nothing was recognised",
    // never "no game sites were visited" -- the same null-vs-zero rule as everywhere else here.
    // The card says so in as many words.
    gamePortal(title) {
      if (!title) return "";
      const t = String(title).toLowerCase();
      for (const [re, label] of GAME_PORTALS) {
        if (re.test(t)) return label;
      }
      return "";
    },

    // What to call an executable when showing it to a parent.
    //
    // The keys stay as process names everywhere they matter — enforcement matches on them, and
    // `apps` and `focused` are rendered side by side on the same key, so renaming the data would
    // split one app into two rows. Only the presentation changes.
    //
    // A curated set rather than asking Windows. Reading `FileDescription` out of the version
    // resource means file I/O per app and still misses Store-packaged programs, which is exactly
    // the Roblox case this product most cares about. Anything unknown falls back to the executable
    // without its extension, which is already an improvement on "RobloxPlayerBeta.exe".
    appLabel(name) {
      if (!name) return "";
      const known = {
        "chrome.exe": "Google Chrome",
        "msedge.exe": "Microsoft Edge",
        "firefox.exe": "Firefox",
        "brave.exe": "Brave",
        "opera.exe": "Opera",
        "robloxplayerbeta.exe": "Roblox",
        "windows10universal.exe": "Roblox",
        "minecraft.windows.exe": "Minecraft",
        "javaw.exe": "Minecraft (Java)",
        "steam.exe": "Steam",
        "steamwebhelper.exe": "Steam",
        "discord.exe": "Discord",
        "spotify.exe": "Spotify",
        "vlc.exe": "VLC",
        "obs64.exe": "OBS Studio",
        "explorer.exe": "Windows Explorer",
        "notepad.exe": "Notepad",
        "winword.exe": "Word",
        "excel.exe": "Excel",
        "powerpnt.exe": "PowerPoint",
        "onenote.exe": "OneNote",
        "teams.exe": "Teams",
        "ms-teams.exe": "Teams",
        "zoom.exe": "Zoom",
        "code.exe": "Visual Studio Code",
        "whatsapp.exe": "WhatsApp",
        "telegram.exe": "Telegram",
      };
      const hit = known[String(name).toLowerCase()];
      if (hit) return hit;
      return String(name).replace(/\.exe$/i, "");
    },

    // The heading over a breakdown, which has to say *why* it is showing the date it shows.
    //
    // Unpinned, each panel independently picks the newest day carrying its own kind of data, so the
    // date needs the qualifier — three panels can legitimately name three different days. Pinned,
    // they all show the chosen day and the qualifier would be a lie.
    stHeading(key) {
      const d = this.stDayFor(key);
      if (!d) return "";
      const what =
        key === "apps" ? "Apps running" : key === "focused" ? "Time in front" : "In the browser";
      if (this.stPinned !== null) return what + " — " + d.date;
      return what + " — most recent day with data (" + d.date + ")";
    },

    // Whether a breakdown has anything to show for the day in view.
    stDayHas(key) {
      const d = this.stDayFor(key);
      return !!(d && d[key] && d[key].length);
    },

    async grantExtra(mins) {
      this.grantingExtra = true;
      try {
        const r = await this.postJSON("/api/extra-time", { minutes: mins });
        if (r.ok) {
          this.toast(`Granted +${mins} min`, "success");
          this.loadToday();
          this.loadUsage();
        } else if (r.status === 400) {
          this.toast("Minutes out of range (1–240)", "error");
        } else {
          this.toast("Could not grant time", "error");
        }
      } catch {
        this.toast("Request failed", "error");
      } finally {
        this.grantingExtra = false;
      }
    },
    // Worth a message more than any other list here. Both surfaces for a pending request — the
    // header badge and the card itself — are hidden when the list is empty, so a failed load is
    // pixel-identical to "your child has asked for nothing". That is the one thing this screen
    // exists to tell a parent, and the failure to tell them is otherwise completely silent.
    loadTimeRequests() {
      const done = this.loadList("/api/time-requests", "timeRequests", null, "Failed to load time requests");
      return done.finally(() => {
        this.requestsAsked = true;
        // The one place a pending count changes on its own. A parent with the dashboard open in a
        // background tab now learns a request arrived without looking at the page — which is the
        // whole of what this product can offer without sending anything outside the house.
        this.syncTitle();
      });
    },
    loadCodes() {
      return this.loadList("/api/time-codes", "codes", "loadingCodes", "Failed to load one-time codes");
    },

    async issueCode() {
      this.issuingCode = true;
      try {
        const r = await this.postJSON("/api/time-codes", { minutes: this.newCodeMins });
        if (r.ok) {
          const j = await r.json().catch(() => ({}));
          this.toast(`Code ${j.code} = ${j.minutes} min`, "success");
          this.loadCodes();
        } else if (r.status === 400) {
          this.toast("Minutes 1–240, and at most 50 active codes", "error");
        } else {
          this.toast("Could not generate a code", "error");
        }
      } catch {
        this.toast("Request failed", "error");
      } finally {
        this.issuingCode = false;
      }
    },

    async copyCode(code) {
      try {
        await navigator.clipboard.writeText(code);
        this.toast("Code copied", "success");
      } catch {
        this.toast("Copy failed — select it manually", "error");
      }
    },

    // `decision` is the literal "approve" or "deny", not a boolean. It was a boolean, and the
    // wrong-argument case granted the child time: any truthy value — including the *string*
    // "deny" — took the approve branch. That is the wrong direction to fail in for a parental
    // control, and it is not hypothetical; it happened while exercising this page, and the log
    // recorded an approval for a request that had been denied. Anything not exactly "approve"
    // now denies.
    async resolveTimeRequest(id, decision) {
      const approve = decision === "approve";
      const verb = approve ? "approve" : "deny";
      try {
        const r = await fetch(`/api/time-requests/${id}/${verb}`, { method: "POST" });
        if (r.ok) {
          const j = await r.json().catch(() => ({}));
          this.toast(approve ? `Granted ${j.minutes ?? ""} min` : "Denied", "success");
          this.loadTimeRequests();
          if (approve) this.loadUsage();
        } else {
          this.toast("Could not update the request", "error");
        }
      } catch {
        this.toast("Request failed", "error");
      }
    },

    usageDetail(e) {
      if (e.event === "screentime_daily")
        return `${e.minutes_used ?? "?"}/${e.budget ?? "?"} min`;
      if (e.reason) return e.reason;
      if (e.minutes != null) return `${e.minutes} min`;
      return "—";
    },

    // Bar height as a percentage of the tallest measured day, floored so a small non-zero
    // day is still visible rather than rounding away to nothing.
    stBarPct(d) {
      const peak = Math.max(1, ...this.screentime.days.map(x => x.minutes_used ?? 0));
      if (d.minutes_used == null) return 100;      // the hatch fills the column
      // Deliberate 3% floor, not a true zero height: a measured-zero day is a real
      // observation (the service watched and saw nothing) and needs a visible, hoverable
      // mark of its own, distinct from both a tall measured bar and the not-measured hatch.
      // Do not "correct" this back to 0 — a height:0% rect renders no area and can't be
      // hovered, which would make the day's <title> unreachable.
      if (d.minutes_used === 0) return 3;
      return Math.max(4, Math.round((d.minutes_used / peak) * 100));
    },

    // How a day's figure is phrased, in one place. The chart tooltip and the day-by-day
    // table both show it, and when they each formatted it themselves they had already
    // drifted apart ("90 of 180 min" vs "90 min of 180 min budget") in the same commit.
    stDayLabel(d) {
      if (d.minutes_used == null) return "not measured";
      const budget = d.budget ? ` of ${d.budget}` : "";
      return `${d.minutes_used}${budget} min${d.over_budget ? " (over budget)" : ""}`;
    },

    // How a bar looks, in methods rather than in the attribute. Keeps the three states —
    // measured, over budget, not measured — named in one place, and keeps the markup to
    // plain method calls, which is the form Alpine's CSP build accepts (see O8): no
    // template literals and no globals in an attribute.
    // Over budget carries a **texture as well as a colour**. `bg-error` against `bg-primary` is a
    // 1.22 contrast ratio in this theme — near-identical luminance differing in hue, and
    // green-against-salmon is a red-green confusion pair — so colour alone left a sighted
    // colour-blind parent with no way to read the one thing this chart exists to show. The screen
    // reader was always told: `stBarTitle` and `stDayLabel` both say "over budget".
    //
    // `.st-over` stripes at 135deg, the mirror of `.st-nodata`'s 45deg, so the two encoded states
    // are also distinguishable from each other and not merely from the ordinary case.
    stBarClass(d) {
      if (d.minutes_used == null) return "st-nodata";
      return d.over_budget ? "bg-error st-over" : "bg-primary";
    },

    // The chart's key. Each entry is a representative day row passed through `stBarClass`, so a
    // swatch cannot drift from the bars it explains — which is what happened when the classes were
    // spelled out in the markup and `.st-over` restyled the bars without them. Full story in
    // `web::tests::the_chart_key_is_rendered_from_the_bar_classes_not_written_into_the_markup`.
    get stBarKey() {
      return [
        { label: "within budget", minutes_used: 1, over_budget: false },
        { label: "over budget", minutes_used: 1, over_budget: true },
        { label: "not measured", minutes_used: null },
      ];
    },

    stBarStyle(d) {
      return `height: ${this.stBarPct(d)}%`;
    },

    // A bar is a control now, so it needs a name a screen reader can use — the `title` attribute
    // it carried before sits on a non-focusable element and is not reliably announced.
    stBarLabel(d) {
      return this.stBarTitle(d) + (this.stPinned === d.date ? " (showing)" : "");
    },

    stBarTitle(d) {
      if (d.minutes_used == null) return `${d.date}: not measured — the service was not running`;
      return `${d.date}: ${this.stDayLabel(d)}`;
    },

    stChangeLabel() {
      const c = this.screentime.change_pct, prev = this.screentime.prev_total_mins;
      // A null change_pct does not always mean "no baseline": a previous period that was
      // measured at exactly zero has no defined percentage, but it is still a comparison
      // worth showing. Saying "no earlier period" there would be plainly untrue.
      if (c == null) {
        return prev == null ? "no earlier period to compare"
                            : `previous period: ${prev} min`;
      }
      const dir = c > 0 ? "▲" : c < 0 ? "▼" : "—";
      return `${dir} ${Math.abs(c)}% vs the previous period (${prev} min)`;
    },

    // "The newest day whose `key` list has anything in it", held in one place for `stDayFor`.
    //
    // Note that is not the same as the most recent *measured* day: a day can be measured (the
    // service was running) and still carry nothing under `key` — per-app data only exists for
    // apps with a limit set, and focus data only for days the watcher was alive. Returning null
    // rather than the newest measured day is what lets the heading name the date it is actually
    // showing, instead of silently substituting an older one.
    stRecentDayWith(key) {
      const days = this.screentime.days.filter(d => d[key] && d[key].length);
      return days.length ? days[days.length - 1] : null;
    },

    // Shared by the "Today" panel banner and the screen-time card, so the two can never
    // disagree about what counts as stale.
    //
    // `== null`, so an absent age counts as stale alongside an explicit null. This used to be
    // `=== null` to stop the warning flashing on every page load, because the initial `today`
    // literal carries no `enforcer_age_secs` key and `undefined` would have matched. That
    // suppressed the flash and the real signal together: `loadToday()` routes through
    // `loadList`, which swallows a failed fetch, so a load that never succeeds leaves `today`
    // at its initial value forever — and the dashboard reported healthy enforcement for a
    // service it could not reach at all. The flash is now prevented by `todayAsked` below,
    // which says whether an answer has been *attempted*, rather than by reading "no answer" as
    // "a good answer".
    isEnforcerStale(age) {
      return age == null || age > ENFORCER_STALE_SECS;
    },

    // The enforcer heartbeat, already served by /api/usage/today. A stale or absent value
    // means the figures below may be missing days rather than showing light use.
    //
    // Silent until the first attempt finishes, so the warning never appears on a page that
    // simply has not loaded yet. After that it reports honestly, including when the load failed.
    stEnforcementStale() {
      return this.todayAsked && this.isEnforcerStale(this.today?.enforcer_age_secs);
    },

    // The sentence after "Enforcement may not be running." Three states, not two, and they point
    // at different things: a service that answered and is late, a service that answered and has
    // never ticked, and a service that did not answer at all. The third only became reachable
    // when the staleness check stopped reading "no answer" as a good answer — and the markup then
    // rendered "No check-in for NaN min.", because it divided an absent age by 60.
    //
    // Returns the whole sentence so the markup is a bare method call: no template literal and no
    // `Math` in an attribute, which is also the form the CSP build needs (O8).
    enforcementDetail() {
      const age = this.today?.enforcer_age_secs;
      if (age === undefined) return "The dashboard could not reach the service to ask.";
      if (age === null) return "The background checks haven't reported yet.";
      return `No check-in for ${Math.round(age / 60)} min.`;
    },

    fmtTime(ts) {
      if (!ts) return "—";
      const d = new Date(ts);
      return isNaN(d.getTime()) ? ts : d.toLocaleString();
    },

    fmtBytes(b) {
      if (!b) return "—";
      const u = ["B", "KB", "MB", "GB"];
      let i = 0;
      while (b >= 1024 && i < u.length - 1) { b /= 1024; i++; }
      return `${b.toFixed(i ? 1 : 0)} ${u[i]}`;
    },
  };
}
