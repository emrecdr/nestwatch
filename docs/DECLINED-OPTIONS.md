# Declined and withdrawn options

Ideas that were weighed and deliberately **not** taken, or that were raised, measured, and
**refuted**. This file exists for one reason: to stop the same idea being proposed a second time by
someone who cannot see why it was dropped.

It is the companion to [OPEN-FINDINGS.md](OPEN-FINDINGS.md), which tracks work that is still open.
Nothing here is a task. **Re-raise only with new evidence** — and if you do, the bar is a measurement,
not an argument.

Several rows record claims that were checked and found **false**. On this codebase a confident claim
has repeatedly survived review and died on contact with the code, so what was checked and refuted is
worth as much shelf space as what was found true.

---

## Declined

Weighed in review and deliberately not done. Re-raise only with new evidence.

| | Why not |
|---|---|
| **A coverage percentage gate in CI** (raised as `N13`) | The measurement was worth doing and is now recorded in `O70`: 84.03% of lines, with the misses concentrated in the two `async` enforcement loops and in `helper.rs`, which shells out to Windows. A *gate* is declined on what the number is made of. The uncovered mass is the I/O layer the architecture deliberately keeps thin and untestable, so a threshold either sits below what the suite already reaches — where it never fires and only slows CI — or above it, where the cheapest way to go green is a test that drives the loop badly rather than one that drives it well. Both outcomes are worse than the honest 84%. The run itself is cheap and repeatable (`cargo llvm-cov --all-features --workspace --summary-only`) and paid for itself once already, by finding three child-facing strings that no test asserted. Re-raise if a driver test for the enforcement loops lands, which would change what the number is made of. |
| **Moving the audit's file append off the async runtime** (was `O65`) | Real as described: `state.audit.record` is the only file-touching call in `api.rs` not on the blocking pool, and `jsonl::append_line` is ~7 syscalls against an ACL-hardened `ProgramData` directory Defender is scanning. Declined on the trade, not the diagnosis. All **sixteen** call sites are bounded by one human action — that is the property `SECURITY.md` argues for at length, and the live-view coalescer exists to keep the one exception true. So the cost is a few milliseconds of a reactor thread on an action a person just clicked. Buying that back means putting sixteen **security-log** writes behind a task boundary, where ordering and shutdown-time delivery become things to reason about rather than things that are obviously right. Losing an audit line is a worse failure than a slow one. Re-raise if an event ever becomes clock-driven — that changes the frequency argument, which is the load-bearing half. |
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
| **Back-dating the return from idle**, so the reconciliation poll could back off while nobody is there | Raised as an accuracy fix and **withdrawn after the counter-argument**. `GetLastInputInfo` reports when input last *happened*, not when the user *returned* — on resume those are the same instant, so any correction guesses in the over-credit direction. Understating is the direction this codebase chooses deliberately elsewhere (`countdown`'s floor division, `clamp`'s scaling). The drop is real: up to one poll interval of genuine use per idle episode. Revisit only if an idle-poll back-off is actually wanted, and then measure first. |
| **Replacing the enforcement process scan with `WTSEnumerateProcessesW`** | One call, one buffer, zero per-process handles, and it carries `SessionId` — which is the key O6's per-account half needs. The `Win32_System_RemoteDesktop` feature is already enabled, so it costs no new dependency. **Killed by the documentation:** a caller outside the Administrators group does not get an error, it gets a *partial list*. On the enforcement path a partial process list is a silent fail-open — apps that should have been killed are simply absent. Running as LocalSystem satisfies the requirement today, but that is a property of the install rather than of the code, and the failure is invisible when it breaks. |
| **Reusing one long-lived `sysinfo::System` across ticks** | Strictly the largest remaining win without new FFI — sysinfo would skip `ProcessInner::new` for processes it already knows. **Declined:** it holds a `PROCESS_QUERY_INFORMATION \| PROCESS_VM_READ` handle open for every live process for as long as the `System` lives. A SYSTEM service permanently holding a few hundred read handles to everything on the machine is a textbook EDR heuristic, and being quarantined by antivirus costs a family more than the syscalls do. Revisit only if a real measurement shows the narrowed refresh is insufficient. |
| **An embedded database — DuckDB, as used in a sibling project** | The precedent is real and defeats the obvious objection: that project builds `x86_64-pc-windows-msvc` natively on `windows-latest`, exactly as this one does. **What does not transfer is the shape.** A realistic rollup row measures 763 bytes typical / 1,891 worst case, so ten years is 3,650 rows and under 7 MiB — a `Vec` holds the entire history. The shipped binary is 3.79 MiB and DuckDB's bundled amalgamation alone sits at crates.io's 10 MB *source* ceiling. And `bundled` cross-compilation is best-effort and needs a C++ cross-compiler for the target, which would cost the `x86_64-pc-windows-gnu` check that is the only way to lint eight `#[cfg(windows)]` `unsafe` blocks from a Mac. **SQLite becomes defensible** if the model ever changes from one blob per day to a row per `(day, app)` — that is ~11,000 rows a year and makes aggregation a query rather than a fold. Not yet earned. |
| **Shortening the 30-second enforcement tick to improve resolution** | The reflex when someone asks for better tracking, and it buys nothing: focus changes are caught within 250 ms by the watcher's hook, and the tick only decides how often that is folded into the day's tally. Resolution is already an order of magnitude finer than the tick, and every cost in the loop would multiply. |
| **Five suspicions about the install and enforcement paths, all refuted by reading** | Recorded because each is plausible enough that someone will suspect it again. **Localised group names** — `doctor` queries the Administrators group by SID (`S-1-5-32-544`) precisely *because* the name is localised; "Administratoren"/"Beheerders" appear only in the comment saying why. **Secrets written before the lockdown** — `prepare_data_dir` runs `create_dir_all` then `harden_acl` *before* the config is constructed, under a comment stating the ordering is deliberate. **ACL hardening** — `icacls /inheritance:r` strips inherited entries first, grants are by SID, and a failure bails the install rather than continuing. **An arbitrary 825-day certificate** — it is Apple's hard limit, and both bounds are set because Apple measures `not_after − not_before`. **Curfew defeatable by `shutdown /a`** — still on past `deadline + slack` re-issues as the *uncancellable* `ShutdownNow`, so cancelling buys one interval, not an evening. |
| **`thumbnail()` instead of `Triangle` for the preview downscale** | Raised on strong numbers: `imageops::resize` runs `vertical_sample` first, which returns `Rgba32FImage` — always 4×f32 whatever the source — so a 4K preview allocates a **33.2 MiB** intermediate every frame, and `thumbnail()` is 16.8 ms → 9.1 ms and 35.4 MB → 2.2 MB allocated. **Declined on a re-measurement.** The reviewer's quality figure (4.2/255) came from a deliberately aliasing-heavy frame; on a desktop-like fixture the visual difference is ≤1/255 and the argument had to be made on bytes instead — where `thumbnail` produces **5–15% larger JPEGs** (4K +10.1%, 1080p +15.4%, 1440p +4.8%). Wire bytes are the one thing the preview tier exists to minimise, and the 33 MiB is transient in a ~50 ms helper process. Also measured so nobody re-proposes them: two-stage is *slower* (18.2 ms), `CatmullRom` 28.2 ms. |
| **`Vec::with_capacity` for the JPEG encode buffer, and for the helper pipe read** | The encoder buffer starts at `Vec::new()` and doubles ~20 times on a full-tier frame; the pipe read gets no size hint because `GetFileInformationByHandle` fails on an anonymous pipe, so it doubles from 32 B (~11 reallocations for a 23 KiB preview). Both declined for the same reason: a capacity large enough to help the **rare** full tier taxes every **preview** frame, which is the one on a timer, and the cumulative memcpy is microseconds against a ~50 ms capture. |
| **Memoizing `FakeControl`'s gradient, and `stBarPct`'s chart peak** | Both real: the fake rebuilds a 1280×720 image per call, and `stBarPct` recomputes the peak once per bar (8,100 element reads at a 90-day window, 0.177 ms). Declined — the first is dev/test-only and a cache adds global mutable state to a deliberately simple fake; the second predates the reviewed diff and is sub-millisecond once per report load. |
| **Collapsing `takeScreenshot(silent, tier)` to one parameter** | The two agree at every production call site, and a reviewer correctly showed the comment defending their independence described the *shipped* behaviour as the failure it prevents. Declined anyway: they answer different questions — how loudly a failure is reported, versus how many pixels are asked for — and the plan that introduced tiers chose the split deliberately and mutation-tested it. **The stale justification was corrected rather than the signature**, which is the part that was actually wrong. |
| **Deriving "today" in the timeline from `this.today.day` rather than the browser clock** | A genuine second definition of "today" on a page where every other today-figure comes from the server. Declined **from a cleanup pass**: it changes timezone semantics, needs a fallback for the pre-load `null`, and belongs in a correctness review rather than a tidy-up. |
| **Labelling the live frame with the app that has focus** | Proposed as the best available mitigation for a black frame: show *which game* rather than a blank rectangle meaning either "monitor off" or "capture defeated". **Declined because the premise was false**, and it was written before that was checked. The watcher's foreground data reaches the dashboard only as `focus_totals` — a per-day *aggregate* in the screen-time report. There is no "what has focus right now" anywhere on the wire, so the label would have named whatever the child used **most today**, pinned under a live picture of something else. Delivering it properly means a new field on `/api/usage/today` or the capture response, which is a feature, not a label. Recorded here because the idea is obviously attractive and will be proposed again. Its idle sibling — skipping capture while `GetLastInputInfo` reports idle — is refuted under O18. |
| **A web-app manifest and icon set, so the dashboard installs to the phone's home screen** | Proposed as finishing the substitute for the app `MOBILE-APP.md` declined: if the browser is the product on a phone, Add to Home Screen is how a parent keeps it, and today that yields a screenshot thumbnail because there is no `apple-touch-icon`, `theme-color` or manifest. **Refuted by this repository, twice over, before any of it was written.** `MOBILE-APP.md` §"Why not simply install the existing dashboard as a web app" records that an installed PWA runs in a separate storage and trust context, so *the certificate exception the parent granted in Safari does not travel into it* — the installed app cannot connect at all against a self-signed certificate. `app.js`'s `syncTitle` comment says the same thing from the other direction, ruling out the Badging API for the same reason. So the feature does not degrade gracefully; it produces an icon that opens a broken app. Two things survive and are separable from it: `ask.html` carries no `rel="icon"` at all, so the child's page really does request `/favicon.ico` and take a 404 on every load, and the low-profile naming (`Host Health`, the stethoscope) is a deliberate decision a manifest would have to respect rather than a gap to fill. Recorded because the idea is obviously attractive — it was proposed in a review of this repository — and the counter-argument is two files away from where anyone would start writing it. |
| An `Enforcer` trait unifying the two background loops | The genuinely shared skeleton is ~6 lines. The blocks that *look* duplicated aren't: curfew calls `disarm()` when a shutdown fails so it retries with a fresh countdown; the rules enforcer deliberately doesn't, and returns as the uncancellable `ShutdownNow`. A shared helper would extract the boilerplate and leave the divergent part behind. |

---

| Extracting `focus_missing` from a day-long accumulator into something cheaper | A real observation — `focus_missing` in `rules::today_summary` is `total_secs >= FOCUS_EVIDENCE_SECS && foreground_secs.is_empty()`, so liveness is inferred from an accumulated total. But the test `absent_focus_data_is_only_called_missing_once_there_is_use_to_contradict_it` pins that threshold as **intended**: an unused machine must say nothing, and only sustained use *against* an empty focus map means "nothing is reporting". Changing how it is derived is a behaviour change, not a simplification — a redesign of a signal with a defended definition. |
| `PreRollover` → a newtype wrapper over `Usage` | The five-field clone in `RulesEnforcer::decide_after_snapshot` looks like duplication of `Usage`'s fields and is not reducible by wrapping. `PreRollover` is a **pre-mutation snapshot**: `decide` runs immediately after and mutates `self.usage`, so the values must be owned clones taken at that instant — which is the whole reason the type exists separately. Restructuring it means changing the enforcement path during a cleanup pass, the class this file already declines several times over. Revisit alongside O2. |
| Seven per-row `x-for` templates in `index.html` → one shared row driven by a descriptor array | The four `today.*` lists (`per_app`, `focused`, `pages`, `groups`) plus siblings look collapsible, but they vary on **two independent axes**: the name via `appLabel()` vs raw, and the value as a `<progress>` bar vs a plain "N min". A shared row needs conditional logic on both, inside an Alpine **CSP-build** template — where `?.`, `??`, spread and template literals are all forbidden (the syntax list and its parser errors are recorded in `app.js`) and a violation renders **nothing, silently**: no error, no failing test. The collapse is unverified against that constraint, and this is exactly the environment where "looks equivalent" and "is equivalent" diverge without warning. |
| Thirteen `resetSessionData` fields → a `sessionData()` factory | `resetSessionData` is not thirteen uniform assignments. The fields are interleaved with field-specific comments and their `loading*` partners, and the block ends in two side-effects a factory cannot hold — `stopShotClock()` and `syncTitle()`, each carrying a comment explaining why a stale value would otherwise outlive the component. Hoisting buys one inventory at the cost of detaching thirteen fields from their reasons — the exact defect the same pass had just fixed seven instances of. **The line is not "never extract":** `emptyScreentime()` *is* extracted, and is called from inside `resetSessionData` itself, precisely because it prevents a live bug — a field added to one of two copies had already leaked a previous session's arrays past sign-out. Pure shape-duplication is extracted; fields carrying individual reasons are left where their reasons are. |
| **Giving the full-size view its own frame buffer, written only by human captures** | The alternative considered when fixing the overlay's sharpness (2026-08-26), and the cheaper one: the live timer would keep updating the thumbnail at preview tier while the overlay held the last full frame, so live view would cost exactly what it costs today. **Declined because it keeps a *stale* sharp frame.** The overlay would show a picture that looks live and is not, while the Live toggle stays lit — which is the failure mode this codebase exists to avoid (`measured` vs absent, `focus_missing`, an unpaired session marker drawn with no width), not a fix for it. The tier now follows the visible surface instead, and the cost is bounded three ways: the overlay is a transient state a person opened, `_liveUntil` still caps an unattended session, and the cadence selector is on the card. If the cost ever proves unacceptable in practice, the honest answer is a slower cadence while the overlay is open — not a frozen picture presented as a live one. |
---

## Withdrawn after measurement

### 94% of every tally write is data the enforcer never reads — and it does not matter

**Measured and withdrawn, 2026-08-25.** The premise holds and the conclusion does not.

The premise, re-verified: `decide` reads none of `foreground_secs` or `page_secs`. Every reference in
`rules.rs` is a struct definition, a rollover `clear()`, `today_summary`, the `PreRollover` snapshot,
`record_foreground`'s writes, `rollup_row`, or a test — none in the decision path, which is what
`foreground_time_cannot_trigger_a_per_app_limit` already pins. So the byte split is real: 195 B of
enforcement data against 3,451 B modelled with a watcher running.

**What is wrong is the significance, and the error is instructive: I costed logical bytes when the
hardware charges physical pages.** `write_atomic` does `File::create` → `write_all` → `sync_all()` →
`rename`. The payload is one component of four, and it is the free one — 338 B (measured on this
machine, no watcher) and 3,451 B (modelled, watcher running) both round to a **single 4 KiB page**.
The cost of a save is the fsync and the two directory updates, none of which scale with the payload.

Worked through at the modelled size — 1,920 fsync'd saves over a 16-hour day:

| | |
|---|---|
| Logical | 6.63 MB/day, 2.42 GB/year |
| Physical, at 4 KiB granularity | 7.9 MB/day, 2.87 GB/year |
| Against a conservative 10 TB endurance budget | **0.03% per year** |
| `sync_all()` latency against the tick that awaits it | 50 ms worst case against 30,000 ms — **0.17% of one tick** |

Splitting the file would therefore save **nothing measurable**, and would *add* a second
create/fsync/rename whenever the report half is written. The one figure that sounded alarming —
2.42 GB/year — is a true byte count with no consequence attached, and quoting it without the
endurance denominator is how it came to head a recommendation.

**The adversarial case does not rescue it either.** `page_secs` is capped at
`foreground::MAX_PAGES` = 40 entries, but a key is a window title of up to 512 UTF-16 units, so a
child deliberately generating long titles could reach ~40 KiB — ten pages a tick rather than one.
That is still 0.1–0.3% of an endurance budget per year, bought with effort, to achieve nothing they
would notice.

**Disposition: do not implement for performance.** The only remaining argument is architectural —
one file conflates enforcement-critical state with report-only state — and that is not a reason to
restructure the persistence of the tally that locks a child's PC on a release nobody has watched run
on Windows. It is the same stacking O2 and O4 decline, and the same reasoning that kept O42's job
object out of the spawn path. Revisit only alongside O2, and only after the on-device pass.

**The general lesson is worth more than the finding.** The declined row on lengthening the 30-second
interval was right for a reason this entry missed: **the cost is the save, not the size.** Only
saving less often would change anything, and that is exactly what must not change, because a reboot
is the child's tool. There was never a cheap win here to find.

---

## The cadence selector's residual contrast

The live-view cadence buttons once shipped as `:class="o.ms === _refreshMs ? 'btn-active' : ''"`,
against a page whose established pattern is `? 'btn-active' : 'btn-ghost'`. **That defect is fixed**
(`assets/index.html`), and the entry that tracked it is gone from the open list. Two things about it
are kept here because they are not recorded anywhere else:

- **The mechanism, which is not about the selected element.** `btn-active` in this dark theme is a
  *subtle* fill. It reads as selected only when its neighbours have no button chrome at all, which is
  what `btn-ghost` removes. Against default `btn` neighbours it disappears — measured in Chrome at
  **1.04:1**, with the selected button marginally *darker* than the unselected ones. Reviewing the
  selected branch alone finds nothing wrong with it, because the affordance lives in the contrast
  *between* states.
- **Every automated gate passed the broken version.** `aria-pressed` was present and correct, so the
  screen-reader test passed; `btn-active` had a rule, so the every-class-has-a-rule test passed; the
  JS test asserting the buttons exist passed. The control was correct to a screen reader and
  invisible to everyone else — the same shape as the curfew toggle that had an `aria-label` and no
  visible text.

**Declined:** restyling the residual weak fill contrast, which is a property of the shared
`btn-active`/`btn-ghost` pattern and applies equally to the theme switch and the report range
selector. Both have already been reviewed and accepted by the parent. If it is ever revisited, note
that `btn-primary` is **not** the answer — that was tried, and it made a settled choice look like a
pending action, which is why that colour is reserved for *Save* and *Take screenshot*.
