# Screen-time reporting — design

**Date:** 2026-08-17
**Status:** approved scope, not implemented
**Scope:** Tier 0 + Tier 1 + Tier 1.5, hardening H1–H4, and honesty items I1–I3 (below).
Roughly 75 lines, entirely additive. No enforcement path is touched.

---

## The finding that shaped this

Screen-time history is **already collected end-to-end**. It was not missing; it was unreachable.

| Stage | State | Evidence |
|---|---|---|
| Collection | Working. 9 event types | `rules.rs`, `curfew.rs` — 7 direct `record()` calls plus a shared helper taking a dynamic `event: &str` (`rules.rs:1053`) |
| Storage | Working. `usage.jsonl` | `usage.rs`, `jsonl.rs` |
| API | Working. `GET /api/usage` | `server.rs:70`, `api.rs:136` |
| UI | Working. Raw event table | `assets/index.html:624` |

What is missing is not a pipeline but a **report**: the dashboard shows raw rows
(`session_start`, `session_stop`) and cannot answer "how much last week, on what?".

The nine events, for reference — only `screentime_daily` is a daily rollup; the rest are
point-in-time and are the noise that crowds it out of the API window:

`budget_countdown`, `budget_lock`, `budget_shutdown`, `budget_shutdown_aborted`, `budget_warn`,
`curfew_countdown`, `screentime_daily`, `session_start`, `session_stop`

### The corrected premise

The initial instinct — including this author's first design — was that new storage was needed,
likely an embedded database. Measurement says otherwise:

| Event rate | Rotation (2 MiB cap) | `recent(200)` reach |
|---|---|---|
| 20/day | 1311 days (3.6 yr) | **10 days** |
| 50/day | 524 days (1.4 yr) | **4 days** |
| 150/day | 175 days | **1.3 days** |
| 400/day | 66 days | **0.5 days** |

Storage has 0.5–3.6 years of headroom. The binding constraint is `recent(200)`, a **read-side
parameter**, two orders of magnitude tighter. Sizing a new store to fix *capacity* would have
solved a problem that does not bite for years while leaving the actual bottleneck untouched.

A separate file **is** in scope — see H1 — but for an unrelated reason: resisting deliberate
eviction by a child generating noise. That is a threat-model argument, not a capacity one, and
conflating the two is how the first draft of this spec got the decision wrong in both directions.
Neither argument rescues a database, which was rejected for four further reasons, recorded here so
it is not re-proposed:

1. **This service powers the machine off deliberately.** Unclean shutdown is a designed, scheduled
   event. Append-only line logs lose at most one truncated line; a B-tree store has real recovery
   semantics to get right under exactly the failure this app causes on purpose.
2. **Volume is not database-shaped.** One rollup row per day, ~113 bytes. Under 1 MB/year.
3. **The workload is a scan, not a join.** Append-only, one row per day, never updated or deleted.
4. **Supply chain.** Dependencies here are visibly curated (see the `image` and `qrcode` entries in
   `Cargo.toml`, which exist to *strip* transitive crates), and CI has a `supply-chain` gate.
   `rusqlite` bundled needs a C toolchain, which would also hit the mingw cross-compile gate.

The codebase already solved this class of problem once: `usage.rs:5` records that the usage log was
split from the audit log "so verbose usage rows can't push security events out of the audit log's
rotation window." The same principle applies, but the cheap fix is on the read side first.

---

## Researched standards

Checked against current practice on 2026-08-17 rather than assumed. Three findings changed
decisions in this document.

**Retention default: 30 days, not 90.** Apple Screen Time retains roughly four weeks (its
`DeviceActivityReportExtension` exposes up to a month); Google Family Link documents 30 days for
activity insights. The ICO Age Appropriate Design Code is more pointed: data minimisation applies
*more strictly* to children, and data should be kept only as long as necessary for the purpose.
The 90 days floated earlier was an arbitrary pick, 3× the industry norm, and in the wrong direction
for a child's data. **Default 30, configurable, clamped at 365** (H3).

**Industry measures foreground focus; this design measures process-running.** Commercial trackers
count the active foreground window and exclude minimised or background apps. This design counts a
process as accruing while it runs — so a launcher or a minimised game reads far higher here than in
Apple or Google's reports. That is not a bug to fix cheaply (see below), but it *must* be labelled:
the card should say the figures are not comparable to phone Screen Time numbers, because a parent
will inevitably compare them.

**Session 0 isolation confirms foreground tracking is expensive.** Microsoft fully disabled
Interactive Service Detection in Windows 10 build 1803; a session-0 service cannot reach user
session windows, so `GetForegroundWindow` is unreachable from the service and would require a
helper resident in the user's session — a much larger change than the on-demand screenshot helper.
The deferral stands, now on an externally verified basis.

**But per-user attribution is nearly free, contrary to the first draft.** `session.rs` already
fetches `WTSINFOEXW` Level-1 and inspects `level1.UserName[0]` to detect the sign-in screen — the
username is already in a validated buffer and simply discarded. No new FFI call, no new `unsafe`.
Only the return type has to carry it. This is a small additive change, not tier-3 work, and the
first draft was wrong to bundle it with foreground accuracy.

**On rejecting a database — stated honestly.** `redb` is mature: pure Rust, ACID, copy-on-write
B+trees, a stable file format, performance comparable to lmdb. It would work. It is rejected here
on *fit*, not capability — one ~300-byte row per day, a scan-not-join workload, and repeated
deliberate power-off. (Turso's Rust SQLite rewrite remains beta as of mid-2026, so the SQLite route
still means `rusqlite` and a C toolchain, which the mingw cross-compile gate and `supply-chain` job
both have to absorb.)

**Missing-data handling (I1) is supported, with a caveat.** Guidance splits on gaps: zero-filling
preserves visual continuity for *randomly* missing samples, but systematic gaps — a failed
sensor, or here a stopped service — call for domain-specific handling and explicit display. These
gaps are systematic by definition, so `null` plus a visible not-measured treatment is the correct
reading, not merely the cautious one.

## Scope

### Tier 0 — read-only report (~30 lines)

`GET /api/screentime?days=N` reads `screentime_daily` rows and returns an aggregated series plus a
summary. A card in the dashboard renders it.

Reads via `recent(usize::MAX)` — the established pattern, already used by `timecode.rs:82` and
`timereq.rs:100`. A third caller, not a new idea.

Aggregation is a **pure function** taking the loaded rows and the window as parameters, mirroring
the `today_summary` refactor of 2026-08-17 (commit `efae1ad`) and for the same reason: the logic
becomes unit-testable without touching disk or process globals.

**Touches:** `api.rs` (handler), `server.rs` (route), `rules.rs` (pure fn + tests),
`assets/index.html` (card).

**Does not touch:** enforcement or kill decisions, `accrue()`, the persisted `Usage` format,
`ProcessInfo` or the `SystemControl` trait, storage format, rotation, retention.

**Works on existing data.** Installs have been writing `screentime_daily` since v0.3.x. This
surfaces history already on disk rather than beginning to collect it.

### Tier 1 — per-app breakdown (~4 lines)

Snapshot `per_app_secs` before `decide()` and include it in the `screentime_daily` payload,
following the existing `prev_day` / `prev_total` pattern at `rules.rs:759-760` — same function,
same shape.

It writes to a **log payload only**; nothing reads it back into the control path, so it cannot
alter enforcement.

Covers apps that have limits. It does **not** discover unlimited apps — see *Deferred*.

### Tier 1.5 — reach the rotated backup (~5 lines)

`recent()` reads only `usage.jsonl`, never `usage.jsonl.1` (`jsonl.rs:69`). After the first
rotation, up to 2 MiB of history exists on disk but is unreachable through the API. Read the backup
first, then the live file, so history survives a rotation boundary.

---

## Threat model

The adversary is a motivated child with a keyboard and time, not a remote attacker.

**Direct file access: closed, and verified.** `prepare_data_dir` (`install.rs:331`) creates
`C:\ProgramData\HostHealth` and hardens it *before secrets land in it*; `harden_acl` grants only
SYSTEM + Administrators with `/inheritance:r` and `(OI)(CI)`, so files created later — including
every log — inherit the lock. `run_icacls` **fails the install** if that doesn't stick rather than
continuing quietly. A standard user cannot read or write these records.

Three places that protection ends, stated plainly because the report must not overclaim:

1. **A local-admin child voids all of it** — ownership, ACLs, service control. The whole tamper
   model rests on `WINDOWS-TESTING.md:16` (his account is not in `Administrators`), which is
   currently unchecked on the real machine.
2. **Physical access voids it.** NTFS ACLs are not encryption; a USB boot or pulled disk reads and
   rewrites the data dir unless BitLocker is on. Same root cause as O5.
3. **He can distort the record without touching the file**, by generating events — which is what
   H1 below exists to stop.

This makes the record **tamper-resistant against a standard user, not forensically tamper-proof.**
The UI must not imply otherwise.

### H1 — dedicated `screentime.jsonl` *(the substantive one)*

Daily rollups get their own file, so point-in-time events cannot evict them.

**This was deferred in the first draft on the wrong grounds.** Judged on *capacity*, a second store
is unnecessary — there is 0.5–3.6 years of headroom. Judged on *adversarial behaviour* it is
necessary: a child scripting lock/unlock cycles emits `session_start`/`session_stop` pairs, and
roughly 14,000 of them — about ten days of AutoHotkey at 30-second tick granularity — rotate the
history out. Tier 1's larger rows make that marginally cheaper for him. Capacity was the wrong
lens; eviction resistance is the right one.

The codebase already made this call once for exactly this reason (`usage.rs:5`: keep verbose rows
from pushing important ones out of a rotation window). This is the same decision, one level down.

Cost: one more `JsonlLog` instance in `state.rs` and one write call. The machinery is proven and
shared; nothing new is invented.

### H2 — treat `date` as untrusted

The `date` field originates from a clock the child may be able to move. The aggregation function
must be **total**: deduplicate by date, ignore future-dated rows, tolerate out-of-order input. A
clock jump must not be able to forge or duplicate a day. Pure logic, unit-tested.

### H3 — clamp `days`

`?days=N` defaults to **30** (see *Researched standards*) and is clamped at 365, so one request
cannot ask for unbounded work and the default errs toward retaining less of a child's data.

### H4 — read legacy rows

Existing installs hold history only in `usage.jsonl`. The report reads the new file *and* the
legacy rows (plus the `.1` backup, per Tier 1.5) so the chart does not start empty after upgrade.

### Rejected as over-complex

Recorded so they are weighed rather than re-proposed: HMAC-signing the log (the key would live on
the machine an attacker must already control), tamper-evident hash chaining, and ACL-level
append-only enforcement (`icacls` cannot express it cleanly, and it would break rotation).

None of these change the conclusion that item 1 above — his account not being an administrator —
is worth more than every hardening measure in this document combined.

---

## Data shapes

Rollup row written at day rollover (Tier 1 adds `apps`):

```json
{"ts":"…","event":"screentime_daily","date":"2026-08-16",
 "minutes_used":200,"budget":180,
 "apps":{"roblox.exe":105,"chrome.exe":62}}
```

`GET /api/screentime?days=30`:

```json
{"days":[{"date":"2026-08-16","minutes_used":200,"budget":180,
          "over_budget":true,"apps":[{"name":"roblox.exe","minutes":105}]},
         {"date":"2026-08-15","minutes_used":null,"measured":false},
         {"date":"2026-08-14","minutes_used":0,"budget":180,
          "over_budget":false,"apps":[]}],
 "total_mins":1120,"daily_avg_mins":37,
 "prev_total_mins":918,"change_pct":22}
```

The three rows above are the distinction I1 exists to preserve: a **measured heavy day**, an
**unmeasured day** (`null` — the service never ticked), and a **measured zero day** (a real row
saying he didn't use it). The middle and last must never render alike.

`daily_avg_mins` and the totals are computed over **measured days only**; averaging unmeasured days
as zero would understate usage by exactly the amount that is unknown.

`change_pct` is `null` when the previous window has no data, rather than 0 — an absent comparison
must not render as "no change".

---

## Edge cases and honesty requirements

- **A missing day is "not measured", never "0 minutes" (I1).** A day with no row means the service
  never ticked that day, which has two very different causes: the PC was off (genuinely zero), or
  the enforcer was stopped or wedged while the machine was in use (unknown, possibly heavy). These
  are indistinguishable from the rollup log alone, so the API returns `null` — not `0` — and the
  chart draws the day hatched and labelled *not measured*.

  Collapsing them to `0` would make **a dead enforcer look exactly like a well-behaved child**,
  which is O4 in a new costume and considerably more persuasive as a chart than as a log. Do not
  interpolate across gaps.
- **Service down across several days** produces one row for the last active day and none for the
  gap; those days render as not-measured, per above.
- **Enforcement liveness is shown alongside the chart (I2).** `/api/usage/today` already returns
  `enforcer_age_secs`. When it is stale or `null`, the card carries a banner — *"enforcement may
  not have been running; figures may be incomplete"* — so the report cannot quietly contradict the
  health signal rendered next to it. No new data or endpoint is required.
- **Figures are machine-wide, not per-account (I3).** `list_processes` uses
  `ProcessesToUpdate::All` and `active_session_state` resolves the *active console* session
  (`WTSGetActiveConsoleSessionId`), so any account at the keyboard accrues to the same tally — a
  parent doing their taxes lands in the child's report. Conservative for enforcement, misleading
  for a report used to make decisions, so the card must say so.

  **The fix is cheaper than first stated.** `session.rs` already fetches the console session's
  `WTSINFOEXW` Level-1 payload and reads `level1.UserName[0]` to spot the sign-in screen; the
  username is already there and discarded. Recording it on the daily rollup — one string per day —
  lets the report flag when another account contributed, with no new FFI, no new `unsafe`, and no
  extra syscall. Left out of the initial cut only to keep the first change read-only-ish, not
  because it is expensive; promote it as soon as the report is in use.
- **The figures are not comparable to phone Screen Time.** Commercial trackers count foreground
  focus; this counts process-running (see *Researched standards*). A parent will compare the two,
  so the card must say which it is rather than leave the difference to be discovered.
- **DST needs no handling — already correct.** `MIN_RESET_GAP` is 12 hours and its comment
  (`rules.rs:510`) states it "leaves room for DST and a mid-day restart while still making a reset
  loop useless." A 23- or 25-hour day neither double-rolls nor drops a day. Recorded so it is not
  re-derived.
- **Clock tampering** is a live threat here (`clock.rs`, `MIN_RESET_GAP`). History writes ride on
  the existing rollover, so they inherit those defences; the report must not add a second, weaker
  path to the same data.
- **Malformed or missing file** yields an empty report and never blocks the control path, matching
  `jsonl.rs` best-effort semantics.
- **What the number means.** The card must state it measures *time the PC was unlocked with this
  app running*. `active` is WTS session state (`rules.rs:743`) — there is no idle detection and no
  foreground tracking, so a minimised game accrues full time. Labelling this is mandatory, not
  optional.

---

## Testing

| Unit | Test |
|---|---|
| Aggregation (pure) | Window slicing, empty input, `change_pct` null on absent baseline, over-budget flag |
| Per-app rollup | Rollover writes one row carrying the previous day's `per_app_secs`, not today's |
| `.1` backup read | Events spanning a rotation are returned in order, newest first |
| Endpoint | Behind `require_auth`; `days` clamped (H3) |
| **H1 eviction resistance** | Flooding the usage log with session events leaves `screentime.jsonl` rows intact — the property the separate file exists for |
| **H2 untrusted dates** | Duplicate dates collapse deterministically; future-dated rows ignored; out-of-order input aggregates identically to sorted |
| **H4 legacy read** | Rows from `usage.jsonl` and `screentime.jsonl` merge without duplication when both contain the same date |
| **I1 not-measured ≠ zero** | A gap in the series yields `null`, never `0`; a genuine zero-usage day with a row still yields `0`. The two must not collapse — this is the test that keeps a dead enforcer from reading as a well-behaved child |

Each verified by mutation — break it, watch the test fail, restore — per the discipline used
throughout the 2026-08-16/17 work.

**On-device (`WINDOWS-TESTING.md`):** confirm rows appear after a real midnight rollover, and that
per-app minutes are plausible against observed use.

---

## Deferred, with reasons

- **Compaction / age-based pruning of `screentime.jsonl`.** The file itself is now in scope (H1),
  but pruning is not: one ~300-byte row per day fills the 2 MiB cap in roughly 19 years. Revisit
  never, most likely.
- **Track all processes (discover unlimited apps).** Requires an owner filter via
  `sysinfo::Process::user_id()` (confirmed present, `sysinfo-0.39.5/src/common/system.rs:2008`),
  which means changing `ProcessInfo` and the `SystemControl` trait — the Windows tier CI cannot
  verify — plus `accrue()` and the persisted `Usage` format. Three high-value targets at once, for
  a reporting feature. Revisit once the report has proved useful.
- **Foreground-window + idle accuracy.** The honest version of "screen time", and what commercial
  trackers actually measure. Deferred on an externally verified basis: Microsoft disabled
  Interactive Service Detection in Windows 10 build 1803, so a session-0 service cannot reach user
  session windows at all. It needs a helper *resident* in the user's session — materially more than
  the existing on-demand screenshot helper, and in the tier only on-device testing can verify.

**Near-term, and explicitly not in this bucket:** per-account attribution. The username is already
fetched and discarded (`session.rs`), so it is a small additive change rather than tier-3 work.
Promote it once the report is in use; it is out of the first cut for sequencing, not cost.
- **Hourly timeline, CSV export.** Not asked for.
