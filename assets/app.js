// Both enforcers stamp a heartbeat every 30s; past this many seconds since the last one,
// "enforcement is alive" can no longer be assumed. One shared constant so the "Today" panel
// banner and the screen-time card's staleness warning can never disagree about what counts
// as stale — they used to (150 vs 300), so a gap between them could show "no check-in" in
// one place and "all fine" in the other for the same age.
const ENFORCER_STALE_SECS = 150;

function app() {
  return {
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
    rules: { enabled: true, daily_budget_mins: 0, budget_by_weekday: null, blocklist: [], app_limits: {}, app_groups: [], warn_secs: 60, budget_action: "lock" },
    appLimitRows: [],
    groupRows: [],
    savingRules: false,
    routines: [],
    loadingRoutines: false,
    newRoutineName: "",
    savingRoutine: false,
    today: { day: null, enabled: true, budget_mins: 0, used_mins: 0, remaining_mins: null, extra_mins: 0, per_app: [], groups: [] },
    loadingToday: false,
    // Whether the first /api/usage/today attempt has finished, succeeded or not. Distinguishes
    // "nothing known yet" from "asked, and the answer is missing" — see isEnforcerStale.
    todayAsked: false,
    grantingExtra: false,
    _lastPerDay: null, // remembers the per-day array while single-limit mode is active
    audit: [],
    loadingAudit: false,
    usage: [],
    loadingUsage: false,
    screentime: { days: [], total_mins: 0, measured_days: 0, daily_avg_mins: null, prev_total_mins: null, change_pct: null },
    loadingScreentime: false,
    timeRequests: [],
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

    loadRoutines() { return this.loadList("/api/routines", "routines", "loadingRoutines"); },

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
      this.processes = [];
      if (this.shotUrl) { URL.revokeObjectURL(this.shotUrl); this.shotUrl = null; }
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
    async loadList(url, field, flag, errMsg) {
      if (flag) this[flag] = true;
      try {
        const r = await fetch(url);
        if (r.status === 401) { this.authed = false; return; }
        if (r.ok) this[field] = await r.json();
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
      // when the staleness warning needs to be reachable.
      this.todayAsked = true;
    },
    loadScreentime() { return this.loadList("/api/screentime", "screentime", "loadingScreentime", "Failed to load screen-time report"); },

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
    loadTimeRequests() { return this.loadList("/api/time-requests", "timeRequests"); },
    loadCodes() { return this.loadList("/api/time-codes", "codes", "loadingCodes"); },

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
    stRecentAppDay() {
      const days = this.screentime.days.filter(d => d.apps && d.apps.length);
      return days.length ? days[days.length - 1] : null;
    },

    // The most recent day carrying *focus* data, chosen independently of stRecentAppDay above.
    //
    // The two measure different things and a day can carry either, both, or neither: every day
    // recorded before the watcher existed has apps and no focus, and so does any day it was dead
    // for. Reusing the running-apps day would put an empty focus list under a heading naming a
    // date that does have focus data somewhere else in the window — which reads as "he looked at
    // nothing that day" rather than as "nothing was watching".
    stRecentFocusDay() {
      const days = this.screentime.days.filter(d => d.focused && d.focused.length);
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
