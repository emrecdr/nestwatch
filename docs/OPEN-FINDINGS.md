# Open findings

Known design problems that are **not** bugs and **not** scheduled. Each one was found by a review
pass, judged real, and deliberately left alone — with the reasoning, so it doesn't have to be
re-derived, and so nobody re-raises something already weighed.

`CHANGELOG.md` records what shipped. This records what didn't, and why: what is still **open**,
what proved worth **fixing** after all, and what was **considered and declined**.

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

### O5 · An unanswered question about enforcement coverage — tracked privately

There is an open question about whether enforcement holds under conditions this repository
deliberately does not enumerate. It is the highest-value unanswered item about this system.

The detail is deliberately **not** in this repository. It describes how enforcement can be
avoided rather than how it works, and — unlike every other finding here — it cannot be
re-derived by reading `src/`, because it is a property of the operating system's behaviour
rather than of this code. That asymmetry is the whole reason for the split: redacting what
the source already shows would buy nothing, so nothing else here is redacted. The repository
is public, and published, this one would be a how-to reachable from a name visible on the
managed PC. It lives in
`docs/private/OPERATIONAL-FINDINGS.md`, which is git-ignored, alongside the fix and the
condition for applying it.


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

### O8 · The dashboard's logic is the least-verified code that ships

**Two of three steps are done.** The scripts are now `assets/app.js` (744 lines) and
`assets/ask.js` (136), out of the markup, and `script-src` no longer admits `'unsafe-inline'` as a
result — an inline `<script>` can no longer run on either page, which is the directive that
matters most where injected content would land. `no_inline_script_on_any_served_page` holds that
shape, since the failure mode is silent.

**There are now 21 JavaScript tests**, on `node:test` — no framework installed, so the addition
costs the project nothing it was not already carrying. They cover the pure decision and formatting
methods: `compareVersions`, `isEnforcerStale`, `stBarPct`, `stDayLabel`, `stBarClass`,
`anyRulesSet`, `fmtBytes`. All five mutations tried against them fail at least one test.

Writing them found O10 on the first run — the staleness indicator reporting healthy enforcement
for a service the page could not reach. That is the argument for this entry, made concrete: the
first tests ever run against this file found a safety-relevant bug in it.

**What remains.** No linter over the two files, and the DOM-facing half is still untested — the
polling loop, the screenshot lifecycle, the error paths. Testing those needs a DOM (jsdom or a
headless browser), which is a materially larger dependency decision than `node:test` was, and is
the kind of thing to decide deliberately rather than adopt in passing. Note it would *not* have
caught O9 either: that was a namespace bug in the markup, which is why the guard for it is a
source scan.

**The point here is that it is the same fix twice.** Moving the script to `assets/app.js` is what
makes it both lintable/testable *and* CSP-tightenable; neither is clearly worth the migration alone,
and together they are.

**The cost, measured — and the first estimate in this entry was wrong.** It claimed
`@alpinejs/csp` "forbids inline expressions, so every `x-text`/`x-show` in 1,650 lines has to move
into the component object", sourced from a GitHub discussion rather than the documentation. The
documentation says otherwise, and counting the markup settles it. Of **264** Alpine directive
attributes in `index.html`, **14** are incompatible:

| | count | verdict |
|---|---|---|
| Template literals | 10 | must become string concatenation or a getter |
| Spread (`[...days].reverse()`) | 1 | must become a getter |
| `Math.round` (inside one of the 10) | 1 | globals are unreachable; move into the component |
| `??` / `?.` | 3 | **undocumented either way** — verify before relying on it |
| `x-model` | 23 | **works** — the discussion claiming otherwise is stale |
| Dotted paths (`today.used_mins`) | 20 | works |
| Comparisons, ternaries, arithmetic, `+` concatenation, method calls | — | all work |

So this is roughly 5% of the directives, not all of them (it was 17 of 268 before O9's fix
retired four template literals along with the SVG chart). That changes the conclusion: the blocker
was never the markup, it is that **233 Rust tests sit beside zero JavaScript tests**, so a runtime
swap under the parent's only interface has nothing to catch a regression.

**What is left, in order.** The relocation and the unit tests are done. Next is `@alpinejs/csp`,
which is what `'unsafe-eval'` is still paying for. It is now a bounded job — 14 of 264 directives
— and the tests above cover the methods those directives call, so a swap that broke the component
object would be caught. What would *not* be caught is a directive that stops evaluating, since
nothing tests the rendered DOM; a headless smoke test of both pages is the honest prerequisite,
and it is the same tooling decision the paragraph above defers.

**Trigger.** Do this before the dashboard grows another panel, or the migration cost grows with it.

---

## Fixed

Raised here, then resolved. Kept rather than deleted, so nobody re-derives a question already
answered — and because *how* a finding was proved fixed is worth more than the fact that it was.
Each was confirmed by mutation: break the fix, watch the named test fail, restore.

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

### ~~O10 · The dashboard reported healthy enforcement for a service it could not reach~~ — **fixed**

Found by the first JavaScript test ever run against `app.js`, on the first run.

`isEnforcerStale(age)` returned `age === null || age > ENFORCER_STALE_SECS`. The strict `===` was
deliberate and documented: the initial `today` literal carries no `enforcer_age_secs` key, so a
loose check would read `undefined` as stale and flash the warning on every page load until
`loadToday()` resolved. The comment said exactly that.

**The cost of that trade was not written down.** `loadToday()` routes through `loadList`, which
catches a failed fetch and — with no `errMsg` passed — does nothing with it. So a load that never
succeeds leaves `today` at its initial value permanently, `enforcer_age_secs` stays `undefined`,
and `undefined === null` is false while `undefined > 150` is also false. The function answered
**"enforcement is fine"** for a dashboard that could not reach the service at all. `heartbeat.rs`
calls a silently dead enforcer "the worst failure this product can have"; this is the browser
under-reporting exactly that, and the failing case is the one where the service is down — which
is when the warning is the whole point.

**Fix.** Split the two questions the `===` was conflating. `isEnforcerStale` now uses `== null`,
so an absent age counts as stale like an explicit one — matching how the rest of the file already
tests for absence. The flash is prevented instead by `todayAsked`, set once the first attempt
finishes whether it succeeded or not, and `stEnforcementStale()` stays quiet until then. The
"Today" banner was calling `isEnforcerStale` directly, bypassing the gate; both banners now go
through `stEnforcementStale()`, which is what the shared helper's own comment said it was for.

**Verified by mutation**: restoring `=== null` fails two tests, and removing the `todayAsked` gate
fails a third. Both properties — no flash before the first load, honest reporting after it — are
asserted, so neither can be traded away for the other again without a test going red.

### ~~O9 · The screen-time chart rendered no bars at all~~ — **fixed**

Not raised by a review — found by running the dashboard in a browser, which nothing else here had
done. The chart repeated its bars with `<template x-for>` **inside the `<svg>`**. A `<template>`
parsed inside `<svg>` belongs to the SVG namespace, is not an `HTMLTemplateElement`, and has no
`.content`; Alpine's `x-for` reads `template.content.children`, threw, and rendered nothing. It
shipped in 0.2.3.

**Why every existing gate missed it.** It is not a Rust bug, not a type error, and not a
formatting or lint issue: it is a DOM namespace rule that only exists once a browser parses the
file. 233 tests, clippy on two targets, and a cross-compile all passed over it. The failure was
silent in the UI too — the summary figures above the chart and the day-by-day table below it both
read from the same data and were correct, so the page looked sparse rather than broken, and the
only evidence was eight console errors nobody was looking at.

**Fix, as shipped.** The bars are HTML `<div>`s in a flex row, so no `<template>` sits inside an
`<svg>` anywhere. Per-bar hover text became a `title` attribute, which is more reliably surfaced
than SVG's `<title>` element; the hatch for unmeasured days moved from an SVG `<pattern>` to a
`.st-nodata` rule in `web/src/app.css`, written in theme variables so it still follows both themes.
`stBarClass`/`stBarStyle` keep the three states named in one place and keep the markup to method
calls — which also removes four template literals from the O8 migration's tally.

**Verified by running**, since that is the only thing that could have caught it: 30 bars where
there were 0, all three states present (10 hatched, 16 within budget, 4 over), the deliberate 3%
floor keeping a measured-zero day visible and hoverable at 3px, and the console going from 20
error lines to none. `web::tests::no_alpine_template_inside_svg` fails if the shape returns —
confirmed by putting it back.

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
wrong. Confirmed by mutation: dropping `v{}` from the header format string fails
`the_report_header_names_the_build` and nothing else.

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
| Adopting `clippy::unused_qualifications` | Clean everywhere except 9 sites, all of them in `curfew.rs` and `rules.rs` — the enforcement path, including the pure function a previous pass already declined to restructure during cleanup. A style lint is not a reason to touch it. The other lints adopted in `Cargo.toml`'s `[lints]` were each verified to produce zero warnings first, so none of them opened a cleanup. Same for `missing_docs` (84) and `clippy::str_to_string` (36). |
| Widening `is_lan` — **second look, still no** | The original row below stands, and the case for widening got weaker, not stronger: Tailscale run as a *subnet router* on another machine already reaches this service from `192.168.x.x`, because subnet routers masquerade routed traffic to their own LAN address by default. So a working Tailscale arrangement exists without touching the allowlist — and it is the better one anyway, since it keeps the tunnel daemon off the monitored PC. README corrected, which had claimed Tailscale simply does not work. |
| An `Enforcer` trait unifying the two background loops | The genuinely shared skeleton is ~6 lines. The blocks that *look* duplicated aren't: curfew calls `disarm()` when a shutdown fails so it retries with a fresh countdown; the rules enforcer deliberately doesn't, and returns as the uncancellable `ShutdownNow`. A shared helper would extract the boilerplate and leave the divergent part behind. |

---

## Not covered by any of this

**None of the above has run on the target machine.** Everything here was found by reading, tests,
and cross-compilation — the same three gates that were green when `install` failed on real
hardware, and again when `remove_file` turned out not to be exclusive. See
[WINDOWS-TESTING.md](WINDOWS-TESTING.md); it is the only method with a track record of finding what
matters here.
