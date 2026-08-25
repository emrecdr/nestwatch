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
    _shotTimer: null,
    _shotBusy: false,
    _pollTimer: null,
    _pollMs: 60000,
    _refreshMs: 3000,
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
      // place rather than an inventory to keep aligned with the state above.
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

    async takeScreenshot(silent = false) {
      // Never overlap captures: the service helper can take up to ~15s, but the Live timer
      // fires every few seconds — without this guard slow captures would pile up.
      if (this._shotBusy) return;
      this._shotBusy = true;
      if (!silent) this.loadingShot = true;
      try {
        const r = await fetch("/api/screenshot");
        if (r.status === 401) { this.authed = false; this.stopAutoRefresh(); return; }
        if (!r.ok) throw new Error();
        const blob = await r.blob();
        if (this.shotUrl) URL.revokeObjectURL(this.shotUrl);
        this.shotUrl = URL.createObjectURL(blob);
      } catch {
        if (!silent) this.toast("Screenshot failed", "error");
      } finally {
        if (!silent) this.loadingShot = false;
        this._shotBusy = false;
      }
    },

    toggleAutoRefresh() {
      if (this.autoRefresh) {
        this.takeScreenshot();
        // Skip while the tab is hidden, matching the data poll. Each tick spawns a helper
        // in the child's session to capture and PNG-encode their whole desktop — by far
        // the most expensive thing this tool does — and without the guard it kept doing it
        // on their laptop for as long as the parent left the tab open in a pocket.
        this._shotTimer = setInterval(() => {
          if (!document.hidden) this.takeScreenshot(true);
        }, this._refreshMs);
      } else {
        this.stopAutoRefresh();
      }
    },

    stopAutoRefresh() {
      if (this._shotTimer) { clearInterval(this._shotTimer); this._shotTimer = null; }
      this.autoRefresh = false;
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

    // The heading over a breakdown, which has to say *why* it is showing the date it shows.
    //
    // Unpinned, each panel independently picks the newest day carrying its own kind of data, so the
    // date needs the qualifier — three panels can legitimately name three different days. Pinned,
    // they all show the chosen day and the qualifier would be a lie.
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
    // Minutes as something a person reads. Over a month a heavy app reaches four digits, and
    // "1847 min" is a number you have to do arithmetic on before it means anything.
    fmtDuration(mins) {
      const m = Number(mins) || 0;
      if (m < 60) return m + " min";
      const h = Math.floor(m / 60);
      const rest = m % 60;
      return rest === 0 ? h + " h" : h + " h " + rest + " min";
    },

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
    stBarClass(d) {
      if (d.minutes_used == null) return "st-nodata";
      return d.over_budget ? "bg-error" : "bg-primary";
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

    // The most recent day carrying per-app data — not necessarily the most recent
    // *measured* day: a day can be measured (the service was running) and still have no
    // apps entry, because per-app tracking only exists for apps with a limit set (see the
    // footnote below the card). Returns null if no day in the window has any, so the
    // heading's date makes the substitution visible instead of silently showing an older day.
    // "The newest day whose `key` list has anything in it" — the rule behind the three helpers
    // below, held in one place. They differ only in which list they ask about, and each has its
    // own reason for existing, so they stay as named methods the markup and tests can call.
    stRecentDayWith(key) {
      const days = this.screentime.days.filter(d => d[key] && d[key].length);
      return days.length ? days[days.length - 1] : null;
    },

    stRecentAppDay() {
      return this.stDayFor('apps');
    },

    // The most recent day carrying *focus* data, chosen independently of stRecentAppDay above.
    //
    // The two measure different things and a day can carry either, both, or neither: every day
    // recorded before the watcher existed has apps and no focus, and so does any day it was dead
    // for. Reusing the running-apps day would put an empty focus list under a heading naming a
    // date that does have focus data somewhere else in the window — which reads as "he looked at
    // nothing that day" rather than as "nothing was watching".
    stRecentFocusDay() {
      return this.stDayFor('focused');
    },

    // The most recent day carrying browser page titles, chosen independently again — a day can
    // have focused apps and no browser time at all, which is a normal evening rather than a gap.
    stRecentPageDay() {
      return this.stDayFor('pages');
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
