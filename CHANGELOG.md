# Changelog

All notable changes to Nestwatch. Dates are the release-tag dates.

## [0.3.0] — 2026-07-12

First of the "differentiation" features: offline time codes.

### Added
- **Offline time codes.** The parent generates a single-use code worth N minutes (dashboard
  **Time codes** card) and hands it to the child, who redeems it on the `/ask` page — no parent
  action and no internet needed at redemption, so it works while the parent is away or the
  network is down. Codes are 8 unguessable characters, single-use, and capped at 50 outstanding;
  a valid redemption adds the minutes to today's budget. New parent endpoints `GET`·`POST
  /api/time-codes` and the unauthenticated, LAN-gated, rate-limited child endpoint
  `POST /redeem-code`.

### Security
- The `/redeem-code` surface grants time without a live parent action but is bounded: codes are
  drawn from the OS CSPRNG (~1.1 trillion combinations) so brute-forcing the rate-limited
  endpoint is infeasible; codes are single-use; plaintext codes live only in the ACL-hardened
  data dir (and are never written to the audit log); redemption reveals only `{ok, minutes}`.
  See `docs/SECURITY.md` for the full treatment.

## [0.2.5] — 2026-07-12

Validation pass over the 0.2.4 features — correctness and robustness fixes.

### Fixed
- **Pausing (or clearing the budget) no longer leaves a shutdown running.** With the Shutdown
  action, if the parent paused enforcement while the shutdown countdown was already in flight,
  the enforcer skipped its abort logic and the machine powered off anyway. The idle/paused path
  now cancels a shutdown it had scheduled (still gated on curfew being inactive).
- **The dashboard no longer shows a phantom budget on an unlimited day.** Granting extra time on
  a day with no base budget (reachable via the child-approve path) displayed a budget/remaining
  the enforcer never applied. `effective_budget_mins` now returns 0 when the day is unlimited, and
  the enforcer, its logging, and the card all read that one value.
- **A malformed `budget_by_weekday` can't brick startup.** It was a fixed 7-element array, so a
  wrong-length array in a hand-edited config failed to parse and stopped the service from starting
  (locking the parent out). It's now a length-tolerant list that falls back per day.
- **Per-day budget inputs no longer log errors while hidden**, and toggling per-day mode off then
  on no longer wipes a saved weekend configuration.

### Changed
- Usage history distinguishes a parent-initiated bonus (`source: parent`) from a child
  request that was approved (`source: request`).

## [0.2.4] — 2026-07-12

Parental-control parity pass: the dashboard now shows the day's usage and gives the parent
direct control over it, plus per-day budgets and a pause switch.

### Added
- **Today's-screen-time card.** Live minutes used / remaining against the effective budget, a
  colour-coded progress bar, and per-app usage bars — the data existed in the tally but was
  never surfaced. New read-only `GET /api/usage/today`.
- **Parent-initiated bonus time.** +15 / +30 / +60 buttons on the card grant extra minutes to
  today's budget directly, without waiting for the child to ask. New `POST /api/extra-time`,
  using the same day-scoped grant mechanism as approving a request (survives a reboot, resets
  tomorrow), audited and recorded in usage history.
- **Per-day-of-week budgets.** An optional "different limit each day" mode sets a separate
  budget for each weekday (e.g. less on school nights, more on weekends); `0` turns a day off.
  When unset, every day uses the single everyday limit, so existing configs are unchanged.
- **Master pause/resume toggle.** One switch pauses the whole rules enforcer (budget, blocked
  apps, per-app limits) for a "free evening" without clearing any settings; curfew is separate
  and still applies. Defaults to *enabled*, including for configs written before this release.

## [0.2.3] — 2026-07-12

Accuracy and robustness pass for the screen-time budget, plus child-facing warnings.

### Fixed
- **Screen time no longer accrues while the PC is idle, locked, or logged out.** The budget
  added 30s every tick unconditionally, so a machine left on overnight burned the whole day's
  budget before the child woke — and time kept counting while the budget lock itself held the
  screen, which also re-issued the Lock (re-spawning a session helper) every 30s against an
  already-locked desktop. A new `SystemControl::session_state()` — one
  `WTSQuerySessionInformationW(WTSSessionInfoEx)` call from the service, no user-session helper —
  gates accrual and enforcement on the console session being present and unlocked, and gives a
  fresh warning grace when the child returns.
- **Approving more time now cancels an in-flight budget shutdown.** With the Shutdown action,
  once `shutdown /s` was issued a grant cleared the enforcer's deadline but the machine powered
  off anyway. The enforcer now aborts a shutdown it scheduled once the child is back under
  budget — gated on curfew being inactive, so curfew stays the sole authority over its shutdowns.
- **`config.json` and the `usage_state.json` tally are written atomically** (temp file → fsync →
  rename), so a crash or power cut mid-write can't corrupt them. A truncated `config.json` would
  otherwise stop the service from starting and lock the parent out until reinstall.
- The daily `screentime_daily` rollup logged **today's** budget on the previous day's row; it
  now records the budget that was actually in force that day.

### Added
- **Child-visible warnings.** The pre-lock grace period and the Warn action are no longer
  silent: the child gets a brief "Screen time is up. This PC will lock in N seconds." desktop
  notification (`WTSSendMessageW`, non-blocking, auto-dismissing) before a Lock — re-shown each
  time it re-arms after they return — and on the rising edge of a Warn-mode limit.
- The `session_start` / `session_stop` usage events (previously documented but never emitted)
  are now recorded on active-use transitions, and `logout` is now written to the audit log.

## [0.2.2] — 2026-07-11

### Added
- **Service diagnostic logging.** The SYSTEM service (Session 0, no console) now writes its
  `tracing` diagnostics — startup, enforcer failures/retries, cert and control errors — to a
  daily-rotated `service.<date>.log` in the ACL-hardened data dir (retained ~2 weeks). Dev `run`
  still logs to the console; the screenshot `helper` path is untouched. Uses a blocking appender
  (no `WorkerGuard`) so logs aren't lost on a `panic = "abort"`.

## [0.2.1] — 2026-07-10

Quality/cleanup release — no new features. Two small user-visible fixes plus internal dedup.

### Fixed
- The dashboard loaded only four of six panels after a fresh login (Rules and pending
  time-requests were stale until a manual refresh); both `init()` and `login()` now share one
  `loadAll()`.
- The Usage-history "screen-time" row showed `N/? min` because the `screentime_daily` event
  didn't carry the budget; it now includes `budget`.

### Changed
- Internal simplification (no behavior change): a typed `DailyGrant` centralizes the daily
  extra-minutes reset rule; shared `default_warn_secs`; an `api::spawn` helper and frontend
  `postJSON`/`loadList`/`loadAll` helpers remove duplicated offload/fetch/loader code;
  `Rules::effective_budget_mins` and `config::today()` fold twice-open-coded logic.

## [0.2.0] — 2026-07-09

A large parental-control feature batch, plus a foundation refactor. All phases were shipped
behind green CI (fmt, clippy `-D warnings`, tests on Linux + Windows, Windows cross-compile).

### Added
- **Remote lock** — `POST /api/lock` and a dashboard button; under the SYSTEM service the lock
  is launched into the interactive session via the helper (`helper --lock`), since Session 0
  can't lock the desktop directly.
- **Password change** — `POST /api/password` (verify current, re-hash Argon2id, persist,
  rotate the session id) with a dashboard form; no reinstall needed.
- **Screenshot auto-refresh** — a "Live" toggle re-fetches the screenshot every few seconds.
- **Richer curfew** — multiple windows, each with per-day-of-week selection, backward-compatible
  with the previous single-window config.
- **Usage rules engine** — a daily **screen-time budget** (wall-clock, persisted across reboots),
  an **app blocklist** (kill-on-sight), and **per-app time limits**; the exhaustion action is
  configurable (Lock — default / Shutdown / Warn). New `GET`·`POST /api/rules`.
- **Local usage history** — `usage.jsonl` + `GET /api/usage` + a dashboard card (daily
  screen-time and enforcement events).
- **"Request more time"** — an unauthenticated, LAN-gated, rate-limited child page at `/ask`
  (`POST /time-request`); the parent approves/denies in the dashboard, and an approval adds
  minutes to today's screen-time budget.

### Changed
- **Config is now a single `Arc<RwLock<Config>>` source of truth**; all settings persist through
  one `update_config` helper (guard dropped before any `.await`, so the runtime never blocks).
- The append-only JSONL store was factored into one shared module (`jsonl`), with `audit` and
  `usage` as distinct newtypes over it.
- `Config` now derives `Default`; `install` preserves curfew + rules across reinstall.
- Docs (`README.md`, `docs/SECURITY.md`, `docs/WINDOWS-TESTING.md`) updated for the new surface,
  including the intentionally-unauthenticated child endpoint and the expanded capability set.

### Security
- The child `/ask` / `/time-request` surface is deliberately unauthenticated but bounded:
  LAN-gated, rate-limited (a separate per-IP `SubmitLimiter`, 5/min), input-capped, leaks no
  state, and grants nothing without parent approval.
- `cargo audit` advisories cleared (bumped `crossbeam-epoch`; documented-ignore for the
  unreachable `quick-xml` advisories via xcap's never-compiled Linux backend).

## [0.1.0] — earlier

Initial release: LAN-only web dashboard over self-signed HTTPS (single Argon2id password),
screenshot / process list / kill / shutdown, a warn-then-shutdown **curfew**, and a
tamper-resistant Windows **SYSTEM service** (ACL hardening, LAN-scoped firewall rule, session
helper for screenshots). Plus the LAN-security hardening pass: app-layer LAN allowlist, per-IP
login limiter, security headers, audit log, and an 825-day `serverAuth` cert.
