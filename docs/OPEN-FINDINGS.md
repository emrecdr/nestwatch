# Open findings

Known design problems that are **not** bugs and **not** scheduled. Each one was found by a review
pass, judged real, and deliberately left alone — with the reasoning, so it doesn't have to be
re-derived, and so nobody re-raises something already weighed.

`CHANGELOG.md` records what shipped. This records what didn't, and why.

**Provenance.** Several independent review passes over this codebase (2026-08, twelve reviewers in
total), plus a security analysis and a verification round — across four angles: reuse, simplification, efficiency,
altitude. Every claim below was verified against the tree before being written down; the measured
ones say so.

That second pass is also why three entries in *Considered and declined* record findings that were
**refuted**: on this codebase a confident claim has repeatedly survived review and died on contact
with the code, so what was checked and found false is worth as much shelf space as what was found
true.

---

## Open

### O1 · Curfew's per-tick state has two owners

`curfew::Enforcer` owns `deadline`; a loose local in `run_enforcer` owns the `Countdown`. So no
single function answers "what should curfew do this tick" — the two machines are joined only in the
loop body.

**Cost, concretely.** Any rule coupling them has nowhere to live and no way to be tested: *don't
warn while a shutdown is already pending*, *re-arm the countdown when a shutdown is aborted*,
*suppress the warning on an abort tick*. Each would land in the loop as an ad-hoc `if`, invisible to
the test helpers. The rules enforcer has a test pinning exactly this kind of interaction
(`countdown_is_silent_once_the_budget_is_spent`) because both outcomes come out of one call; curfew
**structurally cannot** have the equivalent. A symptom you can see today: `active` is derived three
times per tick from the same `now` (`curfew.rs` — the loop, `bedtime_warning`, `mins_until_active`).

**Fix.** Move `Countdown` into `curfew::Enforcer` as a field and fold the warning into `tick()`,
taking the next-window state as an input so the enforcer stays config-free and clock-free, and
returning the warning alongside the `Action`. Tests then drive the real enforcer. This does *not*
require matching the rules enforcer's `Vec<RuleAction>` shape — a tuple is enough.

**Trigger.** Do this **before** anything else is added to the curfew loop. It is not urgent while
the loop is stable.

### O2 · `rules.rs` has a real seam between the pure machine and the loop

The file is ~1,950 lines holding config types, the tally and its persistence, `Targets`,
`today_summary`, the pure enforcer, the async loop, shutdown-abort coordination, notification
helpers, and the tests.

Size is not the finding. The finding is that **the loop could not be moved out**, because it read
the enforcer's private `budget_deadline`, and same-file privacy was what made that legal. Removing
that field-peek (`RuleAction::LockWarning`, shipped) unblocked the cut — so the seam is now
actually available, which it wasn't before.

**Fix.** Cut between the pure machine (`Tick` / `RuleAction` / `RulesEnforcer`) and the loop
(`run_rules_enforcer` + abort coordination + message helpers). A worthwhile follow-on is lifting the
tally (`Usage` / `Targets` / `today_summary`) out too — it has its own consumer in `api.rs` that
never touches the enforcer, and it would end the `rules::Usage` / `usage::UsageLog` name collision.

**Do not** split the config types out; `curfew.rs` holds its own the same way, and breaking that
symmetry buys nothing.

**Trigger.** Next time this file is opened for a feature rather than a fix.

**The trigger has fired, and the work is still deferred — deliberately.** The screen-time report
(the screen-time report) opened `rules.rs` for a feature: it added the rollup write and `decide_after_snapshot`.
That is exactly the moment this entry named. It is being held anyway, for the same reason O4 gives
for holding its own fix: this codebase has never been run on the target machine. Cutting the enforcer's pure machine away from its loop is a large change to the file that
decides when a child's PC locks — and stacking it on top of a release nobody has watched run means
that when something misbehaves on the device, there is no way to tell the feature from the
refactor.

**Revised trigger:** the *next* feature to touch `rules.rs` **after** the current build has been verified
on-device against `docs/WINDOWS-TESTING.md`. If that verification finds nothing, do this first,
before the feature.

### ~~O3 · `today_summary` is documented pure but reads process globals~~ — **fixed**

Its doc said "Pure (no I/O) so it's unit-tested" while calling `crate::heartbeat::worst_age_secs()`,
which reads two process-global atomics **and** `SystemTime::now()`.

The cost was visible in the tests: four of them passed only because *none* asserted on
`enforcer_age_secs`. The impure field was precisely the untested one, because pinning it would have
coupled the test to whatever else in the binary had called `beat()`.

`today_summary` now takes `enforcer_age_secs: Option<i64>`, and `api::usage_today` reads the
heartbeat at the edge alongside its other I/O. The function is pure for real, and
`today_summary_passes_the_enforcer_heartbeat_through` asserts all three cases — fresh, stale, and
`None` (never reported, which after one tick's uptime means the loops never started). Confirmed by
mutation: hardcoding the field to `None` fails that test while the other four stay green.

### O4 · A wedged enforcer is reported but never recovered

`heartbeat.rs` calls a silently dead enforcer "the worst failure this product can have", and then
only *displays* the staleness — `api::usage_today` (which feeds the dashboard's Today card) and
`doctor` are its only two consumers. It was `rules::today_summary` until O3 moved the read out to
the edge; the mechanism is unchanged, only where it is called from. There is no `abort`, `exit`, or restart anywhere in `src/`. The signal is pull-based:
it helps only if the parent happens to look.

**What is and isn't already covered.** A *panic* is handled: `panic = "abort"` kills the process and
the SCM restarts it. The uncovered case is a **hang** — a tick that enters
`spawn_blocking` and never returns (a wedged `shutdown.exe`, a stuck WTS call). The loop stalls,
the heartbeat goes stale, and enforcement is off indefinitely with the dashboard still serving
normally.

**Two things checked, because both would have changed the answer:**
- The restart budget is *not* exhaustible. `configure_recovery` sets three `restart/5000` actions
  with `reset= 86400`, which reads like "three restarts then give up for a day". Per
  [`SERVICE_FAILURE_ACTIONS`](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/ns-winsvc-service_failure_actionsw),
  when the failure count exceeds the array length **the last action repeats**. So the SCM restarts
  indefinitely, and a watchdog that aborts cannot strand the machine unenforced for 24 hours.
- A hang is *plausible but undemonstrated*. Every blocking call in the loops has a bounded
  implementation; none has been observed to wedge.

**Fix, revised after validating it against current practice (2026-08-19).** A supervisor reading
`worst_age_secs()` that `process::abort()`s past a generous threshold (≥5× `CHECK_INTERVAL`), so
the SCM restarts a wedged service. Keep the decision a pure, unit-tested function and the abort at
the edge, matching how the enforcers are already split.

One correction to the original sketch: it said "a supervisor **task**", i.e. a `tokio::spawn`.
The established pattern for this — a heartbeat recorded on the runtime and *checked from a plain
`std::thread` outside it* — exists precisely because a supervisor living on the runtime cannot
detect the failure mode where the runtime itself is starved. That is not hypothetical here: this
service builds a multi-threaded runtime (`Runtime::new()`, `lib.rs:185`) and the tick loop has
five await points, two of them `spawn_blocking`. If blocking work ever saturated the pool, an
on-runtime supervisor would be queued behind exactly the stall it was meant to report.

The codebase already has this shape and can be followed rather than invented: `session.rs:314`
spawns a `std::thread` watchdog that kills the screenshot helper if it outruns its timeout. The
enforcer supervisor is the same idea applied to the tick loops.

Practitioner reports on this pattern note no false positives in production, and that an extra
thread waking once a second is negligible — but the caution below is unchanged, and is about this
machine rather than the pattern.

**Why it isn't done.** The abort→SCM-restart leg cannot be exercised by `cargo test`, by clippy, or
by the Windows cross-check — only on the target machine. Shipping a self-abort mechanism into a
service whose runtime behaviour hasn't been observed since an early build risks converting a hypothetical
hang into a real restart loop. **Trigger:** do this on-device, with `WINDOWS-TESTING.md` in hand.

### O5 · Boot-time and recovery-mode gaps — tracked privately

There is an open question about whether enforcement survives every way the machine can be
started, and it is the highest-value unanswered item about this system.

The detail is deliberately **not** in this repository. It describes how enforcement can be
avoided rather than how it works, and the repository is public: published, it is a how-to
reachable from a name visible on the managed PC. It lives in
`docs/private/OPERATIONAL-FINDINGS.md`, which is git-ignored, alongside the fix and the
condition for applying it.


### ~~O7 · The binary could not tell you which version it is~~ — **fixed**

Nothing in `src/` referenced `CARGO_PKG_VERSION`, and the release profile sets `strip = true`, so
the shipped `.exe` carried no version string at all — found by checking a published artifact, which
contained its own version number nowhere. There was no `--version` flag, and `doctor` did not print
one either.

**Why it mattered.** This is a tool installed by hand, from a downloaded file, onto a machine
visited rarely. The question you could not answer while standing at that PC was *which build is
actually running*, which is the first thing worth knowing when something behaves unexpectedly, and
the one that decides whether a given security fix reached the machine rather than just the
repository.

**Fix, as shipped.** `env!("CARGO_PKG_VERSION")` behind `crate::VERSION`, surfaced by a `version`
command (`--version` / `-V`), in `doctor`'s report header, and in the usage text. `strip = true`
does not affect `env!` — it is baked in at compile time as an ordinary string constant. Verified
where it counts: a **stripped release build** reports its version from both surfaces, and a test
pins the doctor header so it cannot silently drop out of the one report you read when something is
wrong.

### O6 · Screen-time figures are machine-wide and count running, not focused, time

The report added in the screen-time work counts any account at the console, and counts an app while
its process runs rather than while it has focus. Both are conservative for enforcement and
misleading for a report; both are labelled on the card rather than silently accepted.

**Per-account attribution is cheaper than foreground tracking, but it is not "free" — corrected
2026-08-19.** The username genuinely is already fetched and discarded: `session.rs:144` reads
`level1.UserName[0]` purely to detect the sign-in screen, from a `WTSINFOEXW` buffer it has
already validated. No new FFI call, no new `unsafe`, no extra syscall — that part holds.

What the earlier estimate missed is the blast radius above that line. `session_state()` is a
method on the `SystemControl` trait, so carrying a username out means changing the trait, the
`SessionState` enum, and all three implementations (`windows.rs`, `service_control.rs`, `fake.rs`)
plus `rules.rs` — six files, and the enum is matched on in the enforcement path. It is a small
change in the FFI and a trait-wide change everywhere else, which is a different proposition from
"one string per day".

That does not make it wrong, and the value is unchanged: without it the report cannot separate a
parent doing their taxes from a child's evening. But it is not the free win the first pass called
it, and it lands in the tier only on-device testing can verify at runtime — the same reason O4 is
held. Do it alongside the next change that already opens `SystemControl`, not on its own.


**Foreground accuracy is not cheap.** Microsoft disabled Interactive Service Detection in Windows 10
build 1803, so a session-0 service cannot reach user-session windows at all; it would need a helper
resident in the child's session, well beyond the existing on-demand screenshot helper.

---

## Considered and declined

Weighed in review and deliberately not done. Re-raise only with new evidence.

| | Why not |
|---|---|
| A general `control::call` wrapper over all seven `SystemControl` methods | Two reviewers disagreed. The failure messages are call-site-specific by design (`"budget lock FAILED — screen time is not being enforced right now"`), so wrapping all seven means seven wrappers each taking a message parameter — strictly worse. The one concrete cost, two dropped `JoinError` arms, was fixed directly. `control::notify` stays because it is a *policy* wrapper (failure is a debug-level non-event; delivery is a boolean both callers branch on), not merely an async shim. |
| Splitting `RulesEnforcer::decide` into app-rules and budget halves | The seam is genuine — the two halves share nothing but the accrual that already runs first. Declined as a restructure of the security-critical pure function during a cleanup pass, not because the analysis was wrong. Revisit alongside O2. |
| Hoisting `parse_hm` out of curfew's lookahead probe | **Measured**: worst realistic config (7 windows) is 7.2µs per 30s tick, 0.000024% duty cycle. Hoisting is 4.7–7.6× faster and buys 2.5–20ms *per day*. |
| Trimming `sysinfo`'s per-tick process refresh | **Measured** at 8% of a 4.9ms call already on the blocking pool. Caveat: measured on macOS; the Windows syscalls it would skip have a different cost profile and are unverified on target. |
| Lengthening the 30s tally-save interval | The child is the adversary and a reboot is their tool. At 30s a reboot forfeits ≤30s of tally and costs more than that in boot time; at five minutes it becomes "reboot, gain five minutes, repeat". Write-on-change was taken instead — it removes most writes with the guarantee fully intact. |
| Renaming `rules::Usage` → `Tally`, and `RuleAction::Warn` → `LimitReached` | Both would read better. Rename churn across `api.rs`, `doctor.rs` and the tests isn't worth it on its own; fold into O2 if that lands. |
| Sharing one `CHECK_INTERVAL` between the two enforcers | They are independent loops and nothing breaks if they diverge; the comment is descriptive, not a constraint. The real constraint — that a loop must tick faster than the smallest warning threshold — is now documented on `WARN_AT_MINS` instead. |
| **Separating the child's `/time-request` audit line, to stop log eviction** | Raised as a live hole; **refuted by reading the code**. The concern was that an unauthenticated child could append audit lines at 5/min until the 2 MB log and its single `.jsonl.1` backup rolled every login and kill off disk. `api.rs` already audits **only submissions that joined the queue**, and the queue caps at `MAX_PENDING` — so further growth requires a *parent* action to resolve one. The comment there records it as the fourth site of that defect class, after `login`, `pair` and `logout`. Nothing to do. |
| **Moving `heartbeat::beat()` to the end of the enforcer loops** | The doc said it was called at the end "so it proves the tick finished"; the code calls it at the top. The tempting fix is to move the code. **Don't** — `run_rules_enforcer` has two early `continue` paths, and one of them is the parent pressing **Pause**, so beating at the end would report enforcement as dead every time the feature is used. The doc was corrected instead; see `heartbeat.rs`. |
| **Widening `is_lan` to admit CGNAT (`100.64.0.0/10`)** | Confirmed by running: `Ipv4Addr::is_private()` excludes that range, so a parent tunnelling in over Tailscale is rejected by the app-layer gate. Declined anyway — the tool is LAN-only by design, and admitting a range no home network uses would extend the trust boundary for every install to fix an explicitly unsupported setup. Documented in the README instead, so it reads as a boundary rather than a bug. |
| An `Enforcer` trait unifying the two background loops | The genuinely shared skeleton is ~6 lines. The blocks that *look* duplicated aren't: curfew calls `disarm()` when a shutdown fails so it retries with a fresh countdown; the rules enforcer deliberately doesn't, and returns as the uncancellable `ShutdownNow`. A shared helper would extract the boilerplate and leave the divergent part behind. |

---

## Not covered by any of this

**None of the above has run on the target machine.** Everything here was found by reading, tests,
and cross-compilation — the same three gates that were green when `install` failed on real
hardware, and again when `remove_file` turned out not to be exclusive. See
[WINDOWS-TESTING.md](WINDOWS-TESTING.md); it is the only method with a track record of finding what
matters here.
