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
| **A coverage percentage gate in CI** (raised as `N13`) | Measured twice. **84.03%** of lines on 2026-08-27 at `7091c84`, the misses concentrated in the two `async` enforcement loops and in `helper.rs`, which shells out to Windows. **96.54%** on 2026-11-15 at `28bdae5`, once driver tests walked both loops through a day in order and `tests/cli.rs` ran the binary itself: `rules.rs` 98.5%, `curfew.rs` 97.0%, `helper.rs` 90.6%, with the largest misses left in `server.rs`'s TLS start-up and `api.rs`'s error branches. A *gate* is still declined on what the number is made of, and the second measurement sharpens the argument rather than weakening it. At 96.5% a threshold either sits below what the suite reaches — where it never fires and only slows CI — or above it, where the cheapest way to go green is a test that touches the remaining lines without asserting anything about them. And the signal a gate would add is already given better: the per-push mutation job blocks on a specific surviving mutant, a located claim that a line can change and nothing notices, which a percentage cannot make. The run itself is cheap and repeatable (`cargo llvm-cov --all-features --workspace --summary-only`) and paid for itself once already, by finding three child-facing strings that no test asserted. **Re-raised on 2026-11-15, when both of its conditions were met — the sequence-level driver tests landed and the figure was re-measured — and declined again.** Re-raise only with a defect the mutation job missed that a coverage gate would have caught. |
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
| Widening `is_lan` — **second look, still no** | The original row below stands, and the case for widening got weaker, not stronger: Tailscale run as a *subnet router* on another machine already reaches this service from `192.168.x.x`, because subnet routers masquerade routed traffic to their own LAN address by default. So a working Tailscale arrangement exists without touching the allowlist — and it is the better one anyway, since it keeps the tunnel daemon off the monitored PC. The README's claim that Tailscale simply does not work was corrected in `4e77f1d`, and the passage was later moved out of the README entirely by `fb4e3db` — the analysis now lives in [`REMOTE-ACCESS.md`](REMOTE-ACCESS.md) §3, which is where to read it. |
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
| **WDAC or AppLocker, to make the blocklist an actual wall** | The blocklist is kill-on-sight and loses to a rename, which the README states. WDAC would fix that — publisher/hash rules, all Windows 11 SKUs — and AppLocker would not (enforcement needs Enterprise/Education, wrong SKU for families, and Microsoft has it in maintenance). **Declined on the failure mode, not the capability.** A bad WDAC policy can leave a machine unbootable, and this tool's whole safety argument is that its worst outcome is a service that stops enforcing, never a PC that stops starting. The same reasoning declines `net user <name> /times:` for the curfew: it enforces logon hours natively and is a *partial* answer at best, because it does not apply to Microsoft accounts, which is what most families sign in with. |
| **A CSRF token or a required custom header, instead of the `Origin` fallback** | OWASP lists the custom-header defence as well suited to API endpoints, and for browsers it is strictly stronger than comparing `Origin` — a cross-origin form cannot set a header at all. **Ruled out by a client, not by the argument.** The Android app in `nestwatch-mobile` reaches this server through Dart's `HttpClient` setting only `Accept` and a cookie (`lib/src/api/nestwatch_api.dart`, verified by reading it), so requiring a header would 403 every call it makes, and the fix lives in a different repository from the break. `Origin` covers the same population — WebKit has sent it on POST since 2008 — and costs that client nothing, because it sends neither header and is admitted on the both-absent arm. |
| **Failing closed when neither `Sec-Fetch-Site` nor `Origin` is present** | OWASP recommends blocking here and this declines it, on a property of the product rather than of the web. The header-less caller on this LAN is a shipped phone app and the test suite's own `oneshot` requests; the browser population that omits `Origin` on an unsafe method is essentially pre-2008. Blocking would cost a real client to shut a window no current browser stands in. Re-raise if the Android client ever gains a header to check, which would make the trade different rather than merely closer. |
| **`Cross-Origin-Embedder-Policy`, alongside the COOP/CORP pair that was added** | The other two are cheap and pay for themselves on browsers without fetch metadata. COEP does not: it exists to enable cross-origin *isolation* for `SharedArrayBuffer` and high-resolution timers, which this page has no use for, and in exchange it constrains what the page may load later. Added headers should each answer a question something actually asks. |
| An `Enforcer` trait unifying the two background loops | The genuinely shared skeleton is ~6 lines. The blocks that *look* duplicated aren't: curfew calls `disarm()` when a shutdown fails so it retries with a fresh countdown; the rules enforcer deliberately doesn't, and returns as the uncancellable `ShutdownNow`. A shared helper would extract the boilerplate and leave the divergent part behind. |
| **Replacing `pairing::qr_svg` with the `qrcode` crate's own SVG renderer** | Raised by a reuse review with the dependency argument checked and correct: the crate's `render::svg::Color` builds the same viewBox, quiet zone and merged `<path>`, its default quiet zone is the same 4 modules `qr_svg` hardcodes, and the feature is declared `svg = []` — **zero** transitive crates, verified in `qrcode-0.14.1`'s own manifest. `Cargo.toml`'s note that `default-features = false` drops "its image/SVG renderers" conflates the two: only `image` pulls anything. **Declined on the output, not the graph.** The crate's canvas opens with `<?xml version="1.0" standalone="yes"?>`, and `tests/provider_pairing.rs` asserts the response `starts_with("<svg")` because the string is embedded in a `data:` URI — so adopting it means shipping a prolog into every data URI or splitting it back off, which is string surgery to undo what the crate just added. It also emits fixed `width`/`height` where this one is viewBox-only and scales to the `<img>` box. What the review was right about was the allocation: `push_str(&format!(...))` ran once per dark module and is now `write!`, matching the four other incremental string builds in `src/`. Re-raise only if the data URI stops being the delivery mechanism. |

---

| Extracting `focus_missing` from a day-long accumulator into something cheaper | A real observation — `focus_missing` in `rules::today_summary` is `total_secs >= FOCUS_EVIDENCE_SECS && foreground_secs.is_empty()`, so liveness is inferred from an accumulated total. But the test `absent_focus_data_is_only_called_missing_once_there_is_use_to_contradict_it` pins that threshold as **intended**: an unused machine must say nothing, and only sustained use *against* an empty focus map means "nothing is reporting". Changing how it is derived is a behaviour change, not a simplification — a redesign of a signal with a defended definition. |
| `PreRollover` → a newtype wrapper over `Usage` | The five-field clone in `RulesEnforcer::decide_after_snapshot` looks like duplication of `Usage`'s fields and is not reducible by wrapping. `PreRollover` is a **pre-mutation snapshot**: `decide` runs immediately after and mutates `self.usage`, so the values must be owned clones taken at that instant — which is the whole reason the type exists separately. Restructuring it means changing the enforcement path during a cleanup pass, the class this file already declines several times over. Revisit alongside O2. |
| Seven per-row `x-for` templates in `index.html` → one shared row driven by a descriptor array | The four `today.*` lists (`per_app`, `focused`, `pages`, `groups`) plus siblings look collapsible, but they vary on **two independent axes**: the name via `appLabel()` vs raw, and the value as a `<progress>` bar vs a plain "N min". A shared row needs conditional logic on both, inside an Alpine **CSP-build** template — where `?.`, `??`, spread and template literals are all forbidden (the syntax list and its parser errors are recorded in `app.js`) and a violation renders **nothing, silently**: no error, no failing test. The collapse is unverified against that constraint, and this is exactly the environment where "looks equivalent" and "is equivalent" diverge without warning. |
| Thirteen `resetSessionData` fields → a `sessionData()` factory | `resetSessionData` is not thirteen uniform assignments. The fields are interleaved with field-specific comments and their `loading*` partners, and the block ends in two side-effects a factory cannot hold — `stopShotClock()` and `syncTitle()`, each carrying a comment explaining why a stale value would otherwise outlive the component. Hoisting buys one inventory at the cost of detaching thirteen fields from their reasons — the exact defect the same pass had just fixed seven instances of. **The line is not "never extract":** `emptyScreentime()` *is* extracted, and is called from inside `resetSessionData` itself, precisely because it prevents a live bug — a field added to one of two copies had already leaked a previous session's arrays past sign-out. Pure shape-duplication is extracted; fields carrying individual reasons are left where their reasons are. |
| **Giving the full-size view its own frame buffer, written only by human captures** | The alternative considered when fixing the overlay's sharpness (2026-08-26), and the cheaper one: the live timer would keep updating the thumbnail at preview tier while the overlay held the last full frame, so live view would cost exactly what it costs today. **Declined because it keeps a *stale* sharp frame.** The overlay would show a picture that looks live and is not, while the Live toggle stays lit — which is the failure mode this codebase exists to avoid (`measured` vs absent, `focus_missing`, an unpaired session marker drawn with no width), not a fix for it. The tier now follows the visible surface instead, and the cost is bounded three ways: the overlay is a transient state a person opened, `_liveUntil` still caps an unattended session, and the cadence selector is on the card. If the cost ever proves unacceptable in practice, the honest answer is a slower cadence while the overlay is open — not a frozen picture presented as a live one. |
| **A retention policy beyond rotation** (raised as `O67`) | Rotation is the retention policy: each log keeps two 2 MiB generations, and `jsonl.rs` clobbers the older at rotation, with no prune and no setting. How much of a child's history a parental tool keeps is a household's question, and keeping less is defensible on its own terms. What made it worth tracking was the silence, and that is gone: the report card shows *History from …* from what survived, and a rotation that destroys a backup writes a `rotated` row with `discarded_bytes` and `discarded_through`, carried into `GET /api/export`. The horizon is a model, not a fact — decades of light use, roughly two to three years for a child at `foreground::MAX_PAGES` every day — so quote it as a range or not at all. `screentime.jsonl` holds the irreplaceable rows, the daily rollups, and rotates slowest. The two options costed at nothing stay available: a rollup-only prune keeping N days, or a larger `MAX_BYTES` for `screentime.jsonl` alone. Declined by the owner on 2026-11-16. Re-raise with a decision to advertise a retention period, or the first parent who watches *History from* move forward and asks why. |
| **A redeem and login ceiling spanning addresses** (raised as `O95`) | The throttles count per address, keyed on `security::throttle_key`, which folds all of loopback into one key so a program on the child's own PC gets one quota rather than sixteen million. What stays open is a second device on the home network binding several RFC1918 addresses: 32^6 codes at 5 a minute against 50 active codes is about 8 years at one address and about a month at a hundred. A shared ceiling that refuses also decides what a legitimately issued code does while its budget is spent, and a cross-address delay on login either waits inside `login_lock` behind every queued guess — the parent's lockout by another name — or refuses whoever arrives during it. Every refused code and wrong password reaches *Refused today*, so sustained guessing is visible rather than silent; a counter is bounded by construction, a lockout is not. One premise, that Windows lets an ordinary account choose loopback addresses, is unconfirmed on the PC (checklist §H11). Declined by the owner on 2026-11-16. Re-raise with a *Refused today* figure that shows guessing, or a household network with devices the parent does not control. |
| **A full live frame sized to the viewer** (raised as `O66`) | `liveTier()` sends the native capture whenever the overlay is open, so a phone's box of roughly 1170×658 receives about a tenth of the pixels a 4K PC captures and encodes, and a desktop browser about 2.4 times what it shows. Sending the overlay's own box (`?tier=full&w=…&h=…`, clamped to native) would fix that with machinery that exists, but `ShotTier` is deliberately one variant and one code path so the full path cannot rot, and a third size axis is what it was written against. Declined by the owner on 2026-11-16. Re-raise if live view is slow on the phone actually in use. |
| **Screen time per Windows account on one PC** (raised as `O6`) | Nestwatch counts the machine, not the person: two children share one budget, and a parent's own use draws down the child's. The README states the boundary, one managed child per PC. Enforcing per account would be a config schema change with migration across both enforcers, the API and the whole dashboard, and would multiply rollup rows, which is the trigger this file gives for SQLite. Declined by the owner on 2026-11-16. Per-account *reporting*, the smaller half, was parked until the next change that opens `SystemControl`, and declined with it by the owner on 2026-11-18. Re-raise either half then. |
| **A weekly mutation run on a Windows runner** (raised in `O109`) | 138 of the 178 lines in `.cargo/mutants-baseline.txt` are in functions only Windows compiles, which no Linux run can reach. A Windows runner would judge them, and minutes are free for a public repository, but Windows builds are slow and the weekly Linux run already fills the concurrent-job limit for hours, which starves release builds. The baseline does its job meanwhile: a new survivor fails the weekly run. Declined for now by the owner on 2026-11-16. Re-raise when on-device testing of the Windows build starts, which exercises that code anyway. |
| **Deferring the dashboard's collapsed cards, or staggering its twelve requests on load** (raised as `O88`) | `loadAll` fires twelve requests at sign-in, six of them for cards that start collapsed. Measured on 2026-11-16 with an optimised build on an Apple-silicon Mac, against logs filled to what the service keeps (1,670 days of rollups in 4 MiB, and a full 2 MiB usage log): ten of the twelve answer in under 0.3 ms of server time, `/api/usage` in 10 ms and `/api/screentime` in 31–36 ms, and all twelve fired at once finish in 75–93 ms, twelve TLS handshakes included. Deferring the six collapsed cards saves about 11 ms, nearly all of it `/api/usage`, and needs a DOM test the page does not have. The cost sits in the always-open *Screen time* card and is the parse of the whole retained history, since 7, 30 and 90 days cost the same. Off Windows `run` lists processes from the fake control, so the `sysinfo` enumeration behind *Running apps* is not in these numbers. Declined by the owner on 2026-11-17. A card nothing else reads can still load on first open, as *Signed-in devices* does. Re-raise if the dashboard is slow to load on a real PC, timed there. |
| **Telling the parent a request has arrived, beyond the tab title** (raised as `O78`) | Web Push needs an external push service, which "nothing leaves the house" forbids, and the Badging API needs an installed app, which does not inherit the browser's certificate exception (`MOBILE-APP.md`). The Notifications API is ruled out by the phone rather than the certificate: a clicked-through `https` origin is a secure context, but `new Notification()` throws in mobile browsers by design, the phone path needs a service worker, and Chromium refuses to register one on an origin whose certificate error was bypassed (crbug 40423989), so it works in a desktop tab only. An audio cue on the event-stream arrival needs none of that, but a phone runs a hidden page's events no better: iOS Safari suspends a hidden page and its `AudioContext` (WebKit bugs 173332 and 261554), and Chrome on Android freezes a background tab's task queues after five minutes. The cue would sound only with the dashboard on screen, where the request card already shows, or in a desktop background tab, where the title already reads *(1) Nestwatch*, and it would need a mute setting and a sound path no test reaches. Declined by the owner on 2026-11-17. Re-raise if parents keep the dashboard open in a desktop browser, where a desktop notification would reach them. |
| **Re-running a weekly mutation shard that lost its runner** (raised as `O103`) | `mutants-weekly.yml` runs 32 shards, and twice in its first four runs one lost its runner partway (shard 25 on 2026-10-05, shard 15 on 2026-10-22), leaving its mutants untested that week. The silence is gone: the `whole-run` job fails the run when a shard's `outcomes.json` has no `end_time`, so a lost shard is a red Monday that names it, and re-running that one job recovers it. An automatic retry would be a `workflow_run` workflow holding `actions: write` that re-runs a run's cancelled jobs once and leaves a shard with survivors alone. It cannot be exercised until a runner is lost, and whether a lost runner always reports `cancelled` is unverified; a click on a rare red run costs less than an unverified workflow with write permission. Declined by the owner on 2026-11-17. Re-raise if lost runners become frequent enough that the click is a chore. |
| **Splitting `rules.rs` into the pure enforcer and its loop** (raised as `O2`) | The file holds the config types, the tally and its persistence, `Targets`, `today_summary`, the pure enforcer, the async loop and the tests, 7,283 lines measured 2026-11-18, and the line between the pure machine and the loop is a real seam. Splitting the most safety-critical file in the codebase is a large change with no feature waiting on it, and the finding's own trigger put it after an on-device run of the checklist that has not happened. Declined by the owner on 2026-11-18. Re-raise when its size blocks a feature, or once the checklist has run on the PC. |
| **Reading the browser's address bar for matching** (raised as `O20`) | The DNS route stays declined (`FOREGROUND-TRACKING.md`): Chrome's own resolver bypasses the Windows DNS client. The address-bar route is cheaper than that document assumed and is specified as `WORDS-IN-GROUPS.md` Phase 0, a per-browser measurement that has to be taken on the PC before anything is built. Declined by the owner on 2026-11-18. Re-raise with Phase 0's measurement. |
| **Sampling page titles more often than every five seconds** (raised as `O22`) | Focus time is exact, from timestamp deltas, but the title attached to it is whatever the last five-second sample saw, so a tab visited for less than one poll can be credited to its neighbour or missed. The finding said itself not to do this speculatively. Declined by the owner on 2026-11-18. Re-raise if the page figures look wrong on the PC. |
| **Taking the watcher's resource numbers in a finding of their own** (raised as `O23`) | No figure in `FOREGROUND-TRACKING.md`'s resource table is measured. The numbers belong to one evening on the PC, and §D2's *Measure the cost* item in `WINDOWS-TESTING.md` already asks for them, so the checklist carries the task. The finding's notes on measuring without confounds are in git: `git show 057b417:docs/OPEN-FINDINGS.md`, `O23`. Declined by the owner on 2026-11-18. Re-raise if that checklist item is ever dropped. |
| **One long-lived helper for live view instead of a process per frame** (raised as `O28`) | Every live-view tick creates a process in the child's session: token, environment block, pipe and `CreateProcessAsUserW` of the whole binary, to deliver one image. Frames are now about 30 KB, so the spawn may be the whole remaining cost or noise; the finding said to measure first and do it last. Declined by the owner on 2026-11-18. Re-raise if live view is slow or costly on the PC. |
| **Skipping an unchanged live frame with an `ETag`** (raised as `O33`) | A screen that does not change is still captured, encoded and sent in full each tick, and a hash of the raw frame would let it answer `304`. Whether the capture backend delivers only on change, which would make this cheap, is unverified. Declined by the owner on 2026-11-18. Re-raise if live-view bandwidth matters in practice. |
| **A usage timeline longer than the last few days** (raised as `O36`) | The *When the PC was in use today* strip reads the last 200 session events, which answers "was he on at two in the morning?" for today, the question it was built for. Declined by the owner on 2026-11-18. Re-raise when a longer timeline is wanted. |
| **Keeping where a program ran from** (raised as `O39`) | The watcher has each program's full path and keeps only its file name, although installed and downloaded are different facts. A coarse directory class would keep that signal, but it changes the Windows watcher and its wire format, and enforcement must keep keying on names. Declined by the owner on 2026-11-18. Re-raise when a parent wants to see it. |
| **Removing `run_helper`'s exemption from the flag scanner** (raised as `O54`) | `run_helper` parses `--tier` by hand, which costs a standing skip in the scanner that polices every other flag and a second test that slices its body out of `lib.rs`. A shared flag list would make the property true by construction; the finding put it after the checklist runs on the PC. Declined by the owner on 2026-11-18. Re-raise when `run_helper`'s flags change. |
| **A lexer for the comment stripper** (raised as `O63`) | `stripJs` tracks string delimiters without knowing it is inside a regex, so a regex literal holding a quote, `/["']/`, would swallow what follows. Nothing in `assets/*.js` needs one, and writing the class another way is the documented workaround. Declined by the owner on 2026-11-18. Re-raise the first time a quote inside a regex is needed there. |
| **A guard for doc comments that drift onto the wrong item** (raised as `O69`) | It happened seven times: inserting an item above another silently hands it the other's doc. `doc_claims::every_module_level_pub_fn_has_its_own_doc_comment` now catches it for module-level public functions. Nothing short of a parser that knows which comment describes which item can see private functions or methods. Declined by the owner on 2026-11-18. Re-raise if such a mechanism appears. |
| **Splitting the dashboard's single Alpine component** (raised as `O71`) | `assets/app.js` registers one component for every card. The cost is coupling rather than reach, the untested half is the DOM half, which a split does not reach either, and the mechanism is cheap if it is ever wanted: more `Alpine.data()` registrations, seams along the cards. Declined by the owner on 2026-11-18. Re-raise when a change needs a card of its own. |
| **Waking the enforcers only for saves that can change enforcement** (raised as `O76`) | Every config save wakes both enforcers, a language or password change included. The choke point is deliberate, and a wake is cheap while the tick is. Declined by the owner on 2026-11-18. Re-raise when a wake becomes expensive, or a config-writing endpoint stops being paced by a person. |
| **A cap on open connections** (raised as `O81`) | The stalled-connection half is fixed: the server speaks HTTP/1.1 only and arms its header timeout on the first poll. Nothing bounds how many connections stay open, on a home LAN behind the app-layer gate, and enforcement does not depend on the dashboard answering. Declined by the owner on 2026-11-18. Re-raise on a report of the dashboard becoming slow or unreachable while the PC is otherwise healthy, or any work that puts the service beyond the home LAN. |
| **Making an idempotent grant replay aware of the day** (raised as `O85`) | `idempotency::RETENTION` is 48 hours, so a grant at 23:59 retried at 00:01 is handed the stored `{"ok": true}` for a day that got nothing. Correct as idempotency and wrong as an answer about today, and no client retries across midnight that way. Declined by the owner on 2026-11-18. Re-raise with a second pushing client that does not read its grant back, or a report of bonus time that was never added. |
| **A consumer manifest for the golden files** (raised as `O86`) | Decided on 2026-11-16: `tests/golden/` becomes shapes any client parses, with a per-file consumer manifest `check_golden.sh` reads and a matching change in `nestwatch-mobile`. It was left to wait on its trigger rather than be done on its own. Declined by the owner on 2026-11-18. Re-raise with a third consumer, or the next deliberate change to `/api/extra-time`'s response body. |
| **A firewall rule for a VPN endpoint on the PC** (raised as `O91`) | Decided on 2026-11-16: remote access, when it is set up, goes through a Tailscale subnet router on a separate box at home (`REMOTE-ACCESS.md`, option 3), which reaches the service from an address on the local subnet that both the firewall rule and the app-layer gate admit. An endpoint on the PC itself would need the two gates widened together. Declined by the owner on 2026-11-18. Re-raise if WireGuard or any tunnel endpoint is put on the PC. |
| **Translating the server's refusal messages** (raised as `O94`) | About 40 messages the server writes, 25 refusal sites in `api.rs` and the `validate` functions, reach a Dutch or Turkish dashboard in English, one sentence at the moment something was refused. The translations in the tables are unreviewed, and adding more without a reviewer is the thing this project avoids. Declined by the owner on 2026-11-18. Re-raise when someone can review Dutch or Turkish, or a parent reports a message they cannot read. |
| **Translating the dashboard's built sentences and remaining markup** (raised as `O96`) | 22 methods build English and 93 strings are written into `index.html` in English. `web/test/app.test.js` pins both lists exactly, so neither can grow or go stale unnoticed, and each entry is one work item. Waiting on the same reviewer as the server's messages. Declined by the owner on 2026-11-18. Re-raise when someone can review Dutch or Turkish. |
| **Keeping unknown settings inside nested config types** (raised as `O97`) | A setting a newer build writes at the top of the config survives an older one, but one inside `rules`, `curfew`, a routine, a provider or a daily grant is dropped. The fix is three lines per type and makes the config's shape harder to read. Declined by the owner on 2026-11-18. Re-raise when a nested field is first added. |
| **Moving from chrono to jiff** (raised as `O110`) | RUSTSEC-2026-0543, issued 2026-11-17, marks chrono unmaintained: its maintainer is winding it down and recommends `jiff` (chronotope/chrono#1768). It is informational, not a vulnerability, and no release fixes it, so `deny.toml` ignores it with that assessment beside the entry. This crate uses chrono's `clock` and `serde` features only, and not chrono-tz, whose time-zone data is the part that stopped being updated; local time comes from the OS offset, anchored by `clock.rs`. Moving is 29 files, the anchored clock, the curfew windows, the day rollover and the stored dates in every log among them, with serde formats that must read back what older builds wrote. Declined by the owner on 2026-11-18. Re-raise with a chrono advisory that is a vulnerability, a Rust release chrono stops building on, or the next change that reworks time handling anyway. |
---

## Withdrawn after measurement

### The child's page should show him his rules — bedtime, the routine in force, his gate's bar

**Raised in the 2026-10-03 audit and withdrawn the same day, before a line was written.** The claim
rested on `api::child_status` returning four numbers (`budget_mins`, `limited`, `remaining_mins`,
`used_mins`) and on the research the audit cited — that transparency and autonomy serve older
children better than bare restriction. Both are true. What the audit had not read was the
endpoint's own doc comment and the test beside it, `child_status_is_unauthenticated_and_leaks_no_rules`
(`tests/api.rs`): `/status` needs no sign-in, so **anyone on the home network** can open the page it
feeds, and the endpoint therefore carries "no curfew window … and nothing about *why* a limit
exists" — a schedule handed out that way is a map for planning around, and not only the child's.
The test forbids the word `curfew` in the body and pins each day to a date and a number, so the
proposed block would have failed it first.

**Why the research does not override the test.** The transparency it argues for is already
delivered, where only the child can see it: the practice notices and the gate-opened sentence land
in *his* session (`probe::tell_child`), the request flow is his to start, and the page tells him
how much of his day is left. None of that publishes the household's rules to the LAN. The one gap
the audit found — the moment the gate opens was silent — was closed by `ecde32d` through that same
channel, not through the page.

**The method lesson, which is the part worth keeping:** a surface's *fields* say what it does; its
test says what it must not do. The audit ranked from the fields. Before proposing that an endpoint
grow, read the test that pins its shape — this repository writes one for every boundary it means.

### The dashboard needs navigation — fourteen cards in one flat scroll

**Raised and withdrawn the same day, 2026-08-31. The claim was false and the method that produced
it is the part worth keeping.**

The claim: `index.html` is 94,682 bytes and holds fourteen top-level cards with one `id`, one
in-page anchor, no tab bar and no drawer — so a parent on a phone scrolls past three configuration
cards to reach *More-time requests*, seventh in source order. Three fixes were proposed: reorder by
urgency, add a sticky glance bar, and collapse the settings tail.

**All three were already done, at the exact commit that was measured.** `f637fde` ("Let the
dashboard fit a phone, and give it a way in"):

- **Seven of the fourteen cards are `<details>`** — Routines, Time codes, Recent access, Usage
  history, Trusted clock, Language and Change password. That is the settings tail, already
  collapsed, already last.
- **An "At a glance" row is the first child of the grid**, full width, carrying enforcement state,
  today's minutes and the pending-request count. Its comment says why: the three questions the page
  is opened with "lived in three non-adjacent cards, one below the fold on a phone — and a phone is
  what the onboarding QR hands you."
- **The requests card is `x-show="showRequests()"`** — it is not seventh-and-buried, it is absent
  when there is nothing waiting and present when there is, with a documented `null`-vs-`[]`
  distinction so a failed fetch cannot look like a quiet evening.

So the ordering complaint was wrong twice: everything above the requests card is operational
(Screen, Running apps, Curfew, Today, Limits), not configuration, and the count the parent needed
was already at the top of the page.

**The method error, which is the reusable part.** The measurement counted `card-title`, `id=`,
`href="#"`, `tabs` and `drawer` — a list of the navigation idioms the reviewer *expected* — found
them absent, and reported "no navigation" as a measured fact. It never grepped for `<details>`,
which is the idiom actually in use. Counting proxies for a property and reporting the conclusion
with the confidence of a direct measurement is how a false claim acquires a number and survives
review. The byte count in the claim was correct; everything inferred from it was not.



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

### Have the scanner sweep read whole files instead of cutting at the test module

**Proposed** 2026-08-31 (this session), as the third item of `O79` (since closed):
`tests/scanner_guards.rs` composes `statements(production_source(&text))`, so it cannot see below
`#[cfg(test)] mod tests` — which is where source-scanning guards actually live. `src/server.rs`'s
route scanner, the canonical broken instance of the class, sits there and is invisible to the sweep
meant to catch its kind.

**Tried and backed out** by the other session, same day. Scanning whole files produces false
positives the rule cannot adjudicate at this resolution: a test *fixture* holding a needle string,
and a whole-text `split_once` in a file that happens to use `.lines()` somewhere unrelated, are
indistinguishable from a real line-oriented scanner. A guard that cries wolf is one somebody
switches off, which costs more than the coverage gained.

**Do not re-propose without** a way to tell a fixture from a scanner — the needle being *compared
against* file contents rather than merely present. Reopening on coverage grounds alone repeats work
already done.

## The mutation baseline's survivors (raised as `O109`)

*Moved here from `OPEN-FINDINGS.md` on 2026-11-18, where it was titled "A full sweep leaves 178 mutants alive, recorded in a baseline rather than fixed". Every file has been worked through and the one decision it left, a weekly mutation run on a Windows runner, is declined in the table above, so it is a record rather than a task. It stays whole because `.cargo/mutants-baseline.txt` and the tests it names point here for why each survivor is accepted.*

**Measured 2026-10-05, the first weekly run, at `260f4d8`.** Of 2,242 mutants planned: 1,287 caught,
338 unviable, 555 survived and 2 timed out — and about 130 were never tested, because shard 25
uploaded nothing and four shards were killed before finishing (`O103`). The 555 split three ways,
and the split is the finding:

- **135 in the six modules that only compile on Windows.** Not test gaps at all: nothing on Linux
  compiles them, so nothing can kill them. Now excluded.
- **99 in `#[cfg(windows)]` functions inside shared files** — `preflight.rs`, `doctor.rs`,
  `clock.rs`, `install.rs` and a few more. The same story one level down, kept in the baseline
  because excluding their files would hide the tested code beside them.
- **321 in code the Linux tests run.** By file: `install.rs` 53, `rules.rs` 48, `doctor.rs` 32,
  `lib.rs` 23, `api.rs` 22, `cert.rs` 18, `auth.rs` 17, `pairing.rs` 17, and twenty files with ten
  or fewer.

**What was done instead of fixing all of them at once.** `.cargo/mutants-baseline.txt` lists every
survivor by file, mutation and function — no line numbers, which move with every edit — and
`mutants-weekly.yml` now fails only on a survivor that is not on it. Before this, the job could only
ever be red; a check that is always red reads exactly like one that is never run. Each line is debt
rather than an exemption: a test gap, an untestable Windows-only line, or a mutant equivalent to the
original. Killing one deletes its line in the same commit.

**Order of work.** The enforcement and the parent's API first — `rules.rs`, then `api.rs` — because
a gap there is a limit that may not hold (done 2026-10-06, below); next `auth.rs`, `pairing.rs` and
`cert.rs`. `install.rs`, `doctor.rs` and `lib.rs` are console and setup paths, much of which only
means anything on Windows even where it compiles elsewhere, and come last. The 99 Windows-only
functions need a mutation run on a Windows runner to mean anything, which is a separate decision
about cost.

**Triaged 2026-10-06: `rules.rs` and `api.rs`, 70 survivors, 65 gone.** Each killing test was
watched failing under its mutant, and the kills were confirmed by cargo-mutants itself on the
functions involved. Seven of the 65 left the list by refactoring rather than by a test: code moved
into a function that a test now covers (`Usage::read_on`, `chargeable`, `Usage::used_mins` — which
the enforcer loop's own minute counts now go through too), or a duplicate of `Rules::spent_today`
replaced by a call to it. Most of the rest were bounds tested only
far past the limit and never at it (`Rules::validate` alone held sixteen), deadlines checked only
after they had passed, and endpoints whose answer no test read back. Five remain, and they are of
two kinds. **Equivalent, two:** `t > now` turned into `>=` in `extend_curfew` (it needs a stored
extension equal to the trusted clock to the nanosecond), and `prev_wanted && !now_wanted` turned
into `||` in `maybe_abort_budget_shutdown`, which only changes how often the curfew setting is
*read* — `should_abort_budget_shutdown` decides the same either way. **Real, three, all in
`run_rules_enforcer`'s records and notices:** the Warn-mode notice's rising edge, the rollover row
written once a day, and the session start and end records. Each needs the loop driven across
several ticks with a real usage log, which `tests/enforcer_session.rs` now shows how to do. The
baseline holds 357 lines after this.

**The untested remainder, run 2026-10-22 at `555286a`.** The weekly run reached the ~130 mutants
the first sweep never tested and found 47 survivors not in the baseline. The 39 in `#[cfg(windows)]`
functions (`install.rs` 34, `doctor.rs` 5) are now in it; the 8 in code the Linux tests run
(`session_handle`, `ParsedRow::detail`, `build_report`) were killed instead. Shard 15 lost its
runner after 59 of its 71 mutants (`O103`), and the other 12 were run locally with the full suite:
one more `#[cfg(windows)]` survivor, now in the baseline too (397 lines), and one mutant in
`window_ending_yesterday` that made the suite hang rather than fail, which a bound on the window now
turns into a failure. Replaying every shard's own check against this baseline left only those 8,
which the tests now kill.

**The first full pass, 2026-10-22 at `ce4a51f`, and what the shards could not see.** All 32 shards
passed, each having tested every mutant it planned: 2,271, of which 394 survived and 2 timed out.
Counted over the whole run instead of shard by shard, it held two survivors more than the baseline:
`replace == with != in install` five times against four lines, and `remove_service -> Ok(())` twice
against one (the `#[cfg(windows)]` body, and the non-Windows stub, which already returns `Ok(())`).
Neither was a new gap, since `install` checks for elevation before anything a Linux test can reach.
They passed because a shard compares only its own survivors with the whole baseline, so a name
that survives once in each of five shards passes all five against four lines. Three lines had
nothing surviving under them: `FakeControl::abort_shutdown -> Ok(())` and `Curfew::is_active_now ->
false`, both now caught, and one of the five `&& → ||` lines in `doctor::run`, where four survive.
The baseline was set to that run's 396 survivors line for line, and a `whole-run` job adds the 32
shards up. It fails on a survivor the baseline does not hold and on a shard without finished
results, and warns on a line nothing survived. Before it went in, it was replayed against that
run's artifacts: it failed with exactly those two on the old baseline, warned on exactly those
three, and passed on the new one.

**Its first run on GitHub, 2026-10-23 at `cc3c9fc`, found flaky tests rather than new gaps.** The
source was identical to `ce4a51f`, yet five mutants changed outcome, each in the same shard both
times, and every "caught" among them came from a test unrelated to the mutated code failing at
random. Two causes. `tests/password_change.rs` holds two tests that each point the process-wide
`NESTWATCH_DATA_DIR` at their own directory and run in parallel, so they shared one, and the
first to finish could delete it between `Config::save` creating it and writing into it: a 500,
about once in two thousand runs. Both halves were shown in a throwaway clone, the sharing with a
forced start order and the failed save with the directory removed underneath it; a lock now makes
the two tests take turns. The probe tests in `control` write a script and run it, and on Linux a
sibling test's fork can hold the script open for writing long enough for `exec` to fail with
`Text file busy` (rust-lang/rust#114554). macOS does not enforce that, so it was not reproduced
here; those tests now retry on exactly that error, through a helper with tests of its own. The
third deletion above was one of the five, a catch by the password test, and its line is back: the
baseline is each name's highest count across the two runs, 397.

**`auth.rs`, 2026-10-24: 17 survivors, 15 gone.** cargo-mutants on the functions involved: 30
caught, 2 missed, and the 2 are equivalent. What the gaps were: the password message's "add N more"
was never read; a lockout was only ever tested while running, so a guard that kept refusing after
it ended passed, and nothing watched the limiter forget a device; *last seen* on the
signed-in-devices card was never checked against the expiry it is derived from; the absolute session
cap was tested only from the refused side; and nothing tested the sliding refresh at all, so its
comparison could be inverted or its five days made zero. The two left are `<` turned into `<=` in
`LoginLimiter::check` and `prune`, which differ only when the clock reads the lockout's end to the
nanosecond. The baseline holds 382 lines after this.

**`pairing.rs`, 2026-10-24: 17 survivors, all gone.** cargo-mutants on the functions involved: 27
caught, none missed. Eleven were the dashboard QR's drawing — the quiet zone, each square's position
and which modules count as dark — which only "is there a QR" ever checked, so a picture that looks
like a QR and scans for nobody passed; it is now read back and compared with the `qrcode` library's
own matrix. The console QR is read back the same way, which also pins the inversion that makes it
scan on a dark terminal. The rest: a minted token's fifteen-minute life (`now + TTL` as a product,
or a clock stuck at one, made a token that never expires), `clear` doing nothing on `uninstall`,
and the advertised host's `Display`, which `install` prints into the parent's address. The baseline
holds 365 lines after this.

**`cert.rs`, 2026-10-24: 18 survivors, all gone.** Nothing ran `ensure_cert`, so a startup that kept
a certificate whose key was lost (HTTPS then cannot start) passed; nothing read the validity window,
so a certificate valid from an hour in the future passed; nothing backdated a certificate, so the
age and days-left `doctor` reports, and the service log's expiry warning, could be constants or
silent; and the machine name came from environment variables no test set, which
`tests/cert_hosts.rs` now does in a binary of its own. The loopback filter on the LAN address could
not be reached through a machine that is online, so it moved into `reachable_address`, which is
tested directly. The log is read through `testutil::LogCapture`, which `request_span.rs`'s tests now
share. The baseline holds 347 lines after this.

**`clock.rs` and `curfew.rs`, 2026-10-24: 22 survivors, 17 gone.** cargo-mutants on the functions
involved: 49 caught, 13 unviable, 5 missed, and the 5 are the `#[cfg(windows)]` zone reading, which
a Linux build never compiles. In `curfew.rs` a default warning of one second, a Sunday-only bedtime
applying all week, an extension ending this instant treated as running, the dashboard's own
ten-minute maximum refused, a countdown not re-armed after a window closed, and a re-issued shutdown
re-issued on every tick all passed, because each was tested only away from its edge; and the loop's
choice of what to count down to was driven by nothing, which `tests/curfew_countdown.rs` now does.
In `clock.rs`: a mark at the furthest real offset (fourteen hours), the recorded zone, the
non-Windows "no identity", the once-per-reading tamper warning, and the anchor offset's unit, which
is zero on a UTC machine whatever the arithmetic and is now read under India's half-hour offset in
`tests/clock_offset.rs`. The baseline holds 330 lines after this.

**The storage files, 2026-10-24: 24 survivors, 19 gone.** `config.rs`, `sessionstore.rs`,
`jsonl.rs`, `idempotency.rs`, `usage.rs` and `audit.rs`; cargo-mutants on the functions involved: 55
caught, 3 unviable, 5 missed, and the 5 are equivalent. Three are the session store comparing
`expiry_date > now`, where `>=` differs only for a session expiring at that nanosecond; two are
`EarnedDay`'s field count, a length hint `serde_json` ignores, and JSON is the only format a config
is written in. What the gaps were: an imported policy's routine limits checked from neither side; a
revocation that a restart undid, and a disk write for revoking nothing; the log's 2 MiB cap and its
rotation at exactly 2 MiB; the 8 KiB window that finds the last timestamp, which a kilobyte would
not, behind a day with forty page titles; a "could this hold the needle" that always said yes; a
stored response replaying for under an hour instead of two days; and the view count on the child's
own page, read through the log by nothing. `UsageLog::recent_including_rotated` had two survivors
and no caller anywhere, so it was deleted rather than tested. The baseline holds 311 lines after
this.

**The report and tracking files, 2026-10-25: 12 survivors, 8 gone.** `screentime.rs`,
`foreground.rs`, `token.rs`, `timereq.rs` and `probe.rs`. A run against the full suite named the 12
exactly — 11 missed and one that hung — and a second, after the tests, caught the 8. The 4 left are
equivalent: `bound` scaling by `elapsed / claimed` when the two are equal, which multiplies by one;
`Tracker::bank` moving its marker to the value it already holds, and banking zero milliseconds; and
`probe::run_once` reading the tally when the threshold is zero, which decides the same way. What the
gaps were: a child who used exactly the limit reported as over it; a first-seen baseline at the cap
of 2,000 apps abandoned as if past it; a row with page titles but no focus losing to a wider legacy
row; a request exactly one window old still counted; tokens drawn from eight characters instead of
thirty-two, which cuts a pairing token from 80 bits to 48 while every token still looks right; a
drained key kept at zero for the life of the watcher; and a torn last line on the helper's pipe,
where the mutant hung the suite rather than failing it. That test now reads on a thread with a
deadline, so a hang there is a failure in five seconds rather than a timeout. The baseline holds 303
lines after this.

**The test-support files, 2026-10-25: 21 survivors, 8 gone.** `control/fake.rs`, `control/mod.rs`,
`srcscan.rs` and `server.rs`. A run against the full suite named the 21 exactly, and a second, after
the tests, caught the 8. Nine of the 13 left are the fake controller's: eight in the arithmetic of
the gradient its screenshot draws, a picture whose colours nothing reads, and its
`lock_workstation`, a no-op whose mutant only drops a log line. Three are equivalent: `encode_shot`
fitting a frame exactly the preview's width or height into the preview box, which resizes it to the
size it already has (two); and `in_a_comment_line` starting its line at the newline rather than
after it, which `trim_start` then strips. The last is `serve` returning at once, the console's entry
point, which binds the real port and returns only when stopped. What the gaps were: a frame too
large in one dimension only, sent to the parent at full size, because every test frame was too large
in both or in neither; the cap on a child's request body cut from 8 KiB to 1,032 bytes, which
refuses an honest 200-character reason once it is escaped; the source scanner's bracket depth, which
no fixture needed because each closed its brackets on a line that could not end a statement; a char
literal holding only `(`, which ran its statement on, because the one fixture held it inside a call
whose line also had `')`; and `line_of` answering 1 for every match, which a guard prints only when
it fires. The baseline holds 295 lines after this.

**The command-line entry points, 2026-10-25: 33 survivors, 32 gone.** `lib.rs`, `helper.rs` and
`main.rs`. Every other test called the library and none ran the program, so a `main` that did
nothing passed, as did a `helper` answering `--lock` with a usage error and a `remote-setup`
printing the teardown script whichever was asked for. `tests/cli.rs` now runs the built binary with
its data folder in a scratch directory and kills it at a deadline. A run against the full suite
named the 33 exactly, and a second, after the tests, caught 32. What the gaps were: a preview tier
the helper never found on its command line, which sent every preview at full size; an empty or
one-byte image passed down the pipe as a success; a setup script with no machine name in it; `pair`
naming a token after `pair` or `--integration`, both valid, so the refusal of `parent` never came;
`run` serving in silence, its log pointed at the service's file or set up not at all; and
`check_flags` doubling its index after an option that was not the first, which waved through
whatever followed, or not advancing after a bare one, which never returned. That last was a timeout
rather than a catch, so the flag tests now run on a thread with a deadline and a hang fails them.
The one left is `helper::lock`, which off Windows is the fake's silent no-op, so returning `Ok` at
once is the same thing. The baseline holds 263 lines after this.

**`doctor.rs`, 2026-10-25: 50 survivors, 29 gone.** 18 are in functions only Windows compiles, the
network-profile and listening checks and the service, firewall and disk checks, and stay until a
mutation run on Windows. Of the 32 in code the Linux tests run, three remain: two differ only when
the console is not elevated, which off Windows it never is, and the third lets a fingerprint of any
length through where a SHA-256 one always has 32 groups. No test had ever run `doctor`, so nothing
read its report for a bare machine, a settings file that does not parse, an install record without
settings or a healthy install, nor the exit code that tells them apart; `tests/cli.rs` now runs all
four, which took 14. The Enforcement section lived inline in `run`, as the earned-time check once
did, and is now `enforcement_checks`, a reading of the config with tests of its own, which took 9.
Six were not test gaps. Three were the liveness line's "checked in" and "stopped" arms, which read
an in-memory heartbeat that no enforcer ever stamps in `doctor`'s own process, so they could never
print; they are deleted, and `heartbeat.rs` and `O4` no longer say `doctor` reports it. The other
three, in `clock_check`, pointed at a defect: a settings file with a zone and no offset, which only
editing it by hand produces, runs on plain local time, while `doctor` called it anchored or called a
zone change held off by "the recorded zone". It now asks for the anchor first. The baseline holds
234 lines after this.

**`preflight.rs`, 2026-10-25: 35 survivors, 9 gone.** 26 are in functions only Windows compiles: the
system-tool, existing-service, downloaded-file, network-profile and Windows-build checks, and the
remedies and service lookups behind them. All 9 in code the Linux tests run are gone. No test called
`gather`, only `check_port` directly, so a `gather` that checked nothing passed, and with it an
install over a held port. Nothing tested the off-Windows stand-ins for `apply` and
`running_service_port` either: an `apply` answering `Ok` prints `done:` for a change that never
happened, and a stand-in answering with a port waves that port through as an upgrade over ourselves.
The other four were the refused-port branch of `check_port`, which a test can reach only by binding
below 1024 without root on Linux, and never on macOS. The bind's result is now judged apart from the
bind, by `bind_finding`, and each way it fails is tested. The baseline holds 225 lines after this.

**`install.rs`, 2026-11-15: 143 survivors, 47 gone.** 89 are in functions only Windows compiles, the
service, firewall, ACL and process work behind `deploy` and `remove_service`. Of the 54 in code the
Linux tests run, seven remain, and none of them can die off Windows: `offer_fixes`, and the two
lines of `install` that call it, have nothing to do where no finding carries a remedy;
`ensure_elevated` always passes where every console counts as elevated; `prepare_data_dir` does
nothing that saving the settings does not do anyway; and `settle_sessions` clearing a store with
nothing in it is the same as not clearing it. Two more, the answers of `prompt_for_password`, come
from a terminal no test has, so `.cargo/mutants.toml` excludes that one function rather than
carrying it as debt. No test had ever run `install` or `uninstall`. `tests/cli.rs` now installs
headless into a scratch folder and reads back what it wrote, the settings, the password hash, the
clock anchor, the certificate and the install record, and covers a reinstall keeping all of them and
the certificate until `--new-cert`, a changed address or a missing key; an unreadable settings file
stopping the install until `--reset-config`; a held port stopping it before anything is written;
devices staying signed in unless `--revoke-sessions`; and `uninstall` keeping the settings unless
`--purge`. The `NESTWATCH_PASSWORD` decision and the prompt's retries moved into `password_from_env`
and `prompt_with`, so neither needs a terminal to be tested. The baseline holds 178 lines after
this.

**Every file has now been worked through.** What is left is 178 lines: 138 in functions only Windows
compiles, which no Linux test can reach and only a mutation run on Windows could judge, and 40
labelled in the paragraphs above as equivalent, or as reachable only on Windows or in the dev-only
fake controller. That leaves one decision in this finding: whether a mutation run on a Windows
runner is worth what it costs (see *Order of work*). Until then the baseline does its job: a new
survivor fails the weekly run, and a line comes off only with the test that kills it.
