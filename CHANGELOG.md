# Changelog

All notable changes to Nestwatch. Dates are the release-tag dates.

## [Unreleased]

### Added

- **Another app can now grant earned bonus time — once a day, honestly labelled.** "Add bonus
  time today" grants exactly as it always did, and pressing it twice still means it twice. What is
  new is that a *named* caller — a companion app pushing "today's practice is done" over the same
  authenticated LAN API the dashboard uses — is treated as what it is: a robot whose reason for
  granting is true all day once it is true at all. Its grant lands **once per source per day**,
  judged by this machine's own tamper-anchored clock rather than any day the pushing device
  claims, and the latch survives a service restart. The audit line names the source instead of
  recording a robot as `"parent"`, so your log stays a record of who actually did what. A retried
  push whose reply was lost (phone schedulers are killed mid-flight routinely) can carry an
  `Idempotency-Key` and receive its original answer instead of a second grant — the standard
  header, doing the standard thing. Nothing here knows what "practice" is: the next source (a
  chores app, a reading log) is a name, not a server change. Nothing new listens, nothing dials
  out, and a household that never points an app at this endpoint sees no change at all.
- **Your child is now told which rule closed their app, and you can see that it fired.** A blocked
  app, one over its own daily limit, and one in a group whose shared pool ran out were all closed
  the same way: the window vanished on the next thirty-second check, with nothing said to the child
  and nothing written to your usage history. From their side that is indistinguishable from a
  crash — so the limit shaped no habit, and unsaved work went with it, while the *screen-time*
  budget in the same install warned three times and gave a minute's grace. The notice names the app,
  names the limit that fired (or the group, when it was a shared pool, since that is where the
  setting actually lives), is translated like every other message they see, and carries the address
  to ask for more time. Once per app per day: repeating "this app is blocked" says nothing new, and
  announcing per launch would let a relaunch loop raise dialogs as fast as processes can start.
  Your Usage history gains one `app_stopped` row per app per day, recording whether the notice
  actually reached them — so "did my Roblox limit work?" is now answerable from the dashboard
  instead of being invisible either way.
- **The dashboard warns you before the certificate expires.** Nothing renews it, and after 825 days
  browsers reject it outright — which means the dashboard is the thing that stops working, so a
  warning that arrives afterwards has nowhere to appear. The only alarms were a line in a log file
  you never open and `nestwatch doctor`, which needs an elevated console on the PC. The "at a
  glance" row now says how many days are left for the final thirty of them, and what to do about it
  (re-run `install`). The threshold stays on the server, so the dashboard holds no copy of it.
- **You can now see what this tool refused.** Nestwatch detects and declines several things a day
  and gets every one of them right — a clock moved to shift the day boundary, a second midnight
  rollover that would have wiped the day's tally, a shutdown cancelled with `shutdown /a`. Every one
  of those refusals went to a `tracing::warn!` in a daily-rotated file inside the ACL-hardened data
  folder, which needs an Administrator console on the child's PC to read. So the record existed
  precisely where a parent checking from their phone could not reach it: the dashboard could answer
  "what did *I* do?" from the access log in detail, and "has anything been pushed back against?"
  not at all.
  <br>A **Refused today** card now appears on the dashboard when — and only when — there is
  something to show, which is not most days. It counts rather than lists, deliberately: all three
  of these are things the child can repeat on a timer, and a row per occurrence would hand the
  person being limited a way to rotate the history out. A count cannot grow the file, so hammering
  any of them produces a bigger number and not a bigger store. The counts ride in the daily tally
  that is already rewritten in place, so they **survive a reboot** — which matters, because
  rebooting is the cheapest thing a child can do and an in-memory count would be cleared by the
  very person it describes.
  <br>**It says what the tool did, never what anyone meant by it.** A family that genuinely crossed
  a time zone produces exactly the same count as a clock moved on purpose, so the card does not
  claim to tell them apart: it reads "clock change ignored — screen time and bedtime kept using the
  trusted time", and adds that nothing needs fixing. That is a fact you can check. It is also what
  makes the card safe to show the child as well as the parent, which is the arrangement research on
  monitoring finds survives; an accusation is the one that does not. A test asserts the wording
  never uses "tamper", "caught", "cheat" or "suspicious".
  <br>Deliberately **not** included: signals whose meaning is ambiguous. An enforcer that stopped
  ticking might be tampering or might be a Windows update, and mixing "we blocked this" with "this
  looked odd" is how a warning becomes one nobody reads.
- **What to do if you forget the control password.** There is no reset link and no recovery email —
  there is no account and no vendor, which is the point — and until now there was nowhere that said
  what to do instead. The README has a section, and `install` says it before asking you to choose:
  the way back in is to run `install` again from an elevated console on the child's PC, which keeps
  your curfew, rules, app limits, routines and granted extra time, and reuses the certificate so
  **devices you have already paired do not warn again**. Only the password changes. The reason this
  needed saying is a consequence of something otherwise good: signed-in devices stay signed in for
  30 days of inactivity, so you will hardly ever type this password — which is exactly what makes
  it easy to forget, and it is not a thing you can fix from a hotel.

### Fixed

- **A bedtime set for one weekday covered the wrong night.** If you set a curfew window on
  specific days and it ran past midnight — "Friday, 22:00 to 07:00", which is how a bedtime is
  normally written — the tool applied your day choice to whatever day it happened to be checking.
  So a Friday window shut the PC down at **3am on Friday**, a school morning nobody asked about,
  and left the small hours of **Saturday** alone, which is the night you were actually setting.
  Both halves were wrong and neither was visible: the log showed a curfew firing, and one had,
  just not the one you set. A window now belongs to the night it opens on, and the line beside it
  says so as you edit — "Applies: Fri night, ending Sat morning".
  <br>**This changes behaviour if you use per-day windows that cross midnight**, which is the only
  configuration that was affected: a window on every day, or one that starts and ends on the same
  day, behaves exactly as before. Worth re-reading your curfew card once after upgrading.
- **A forged report could have wiped out your screen-time history in a few days.** The figures the
  focused-time columns are built from arrive from a helper running as your child, and the tool
  bounds them carefully — how many apps may be listed, how many page titles, how many seconds each
  may claim. What nothing bounded was how *long* a single app name or page title could be. The
  512-character limit that exists lives inside the helper itself, so it constrained an honest one
  and nothing else. Measured with the real code: a single forged report left 29 page titles
  totalling **919,913 bytes**, which is then written to disk every thirty seconds and folded into
  that day's stored row — enough to push your whole retained history out in about four days, where
  it is normally years. Titles and app names are now cut to the same 512 characters an honest
  helper produces, so real data is untouched, and two that become identical after cutting have
  their minutes added together rather than one replacing the other.
- **A forged report from the child's own machine could claim more time than had actually passed.**
  Focused-time figures arrive from a helper that runs as your child — the design assumes it may be
  hostile, and scales any report that claims more seconds than the interval it covers. The scaling
  divided by a total that was itself capped, so once the claimed figures were large enough that
  total stopped growing and every entry was scaled by too much: two forged entries could report two
  seconds of a one-second interval, and forty could report forty. Only the *reported* focus columns
  were affected — screen time, bedtime and every limit are measured separately and were never
  involved — so the visible effect was a number in the report that was simply false. Found by a new
  property-based test rather than by review, and it had survived every hand-written test because
  reaching it needs two extreme values at once, which is exactly the shape a chosen example does
  not have.
- **`nestwatch doctor` and the service log disagreed about the certificate by exactly one day.**
  The log warned at 30 days remaining and `doctor` — which a parent runs *because* of that line —
  said everything was fine until 29. The constant was shared; the comparison against it was written
  out twice, once with `<=` and once with `<`, which is the contradiction the constant was made
  public to prevent. There is now one function both ask, and a test that sweeps every day of the
  certificate's life against the original formula rather than against itself. (The first version of
  that test compared the two new callers with each other, which was a tautology that stayed green
  under the very change it was written to catch; found by mutating it.)
- **The README said there was no phone app. There is one.** Under "Not included" it read "A phone
  app — not built", while `docs/MOBILE-APP.md` — linked from the sentence immediately after —
  records that an Android client exists and pins this install's certificate. It is not only a stale
  sentence: `security::require_same_origin` deliberately declines OWASP's advice to fail closed
  when a request carries neither `Origin` nor fetch metadata, **because that client sends neither**,
  and read against "there is no app" that exemption looks like it protects nothing but `curl`. The
  section now describes what exists and calibrates it honestly — a completed walking skeleton for
  Android and iOS with an installable APK and no tagged release, so buildable rather than shipped —
  and says plainly that notifications reaching you away from home remain impossible here, because
  they need a server outside the house.
- **`install --new-cert` worked but was documented nowhere a person looks.** It reissues the TLS
  certificate, which is what you need after the PC's addresses change; `install` mentioned it only
  in passing, on one branch, and it appeared in neither `nestwatch --help` nor the README. For a
  command-line tool the usage message is the last hop outward, so a flag missing from it does not
  exist as far as anyone is concerned. It is now in both, with the cost stated (every paired device
  warns once more). A new test ties the option table to the printed usage so the next flag cannot
  repeat this — two sides of that triangle were already guarded and this was the unguarded one.

### Security

- **A connection that stalls part-way through a request is now closed.** `hyper` documents a
  30-second timeout for exactly this and it was doing nothing here: applying it requires a timer
  that `axum-server` never installs, after which the documented default silently resolves to
  "no timeout". So any device on your home network could open connections, send half a request, and
  hold them — and the resources were never reclaimed. Both protocols are covered, which matters
  because the server offers HTTP/2 as well: fixing only HTTP/1 is a fix an attacker steps around by
  choosing the other one, and it would have *looked* closed. Measured against the real binary
  before and after rather than reasoned about — a half-sent request that stayed open past 65 seconds
  now closes within 31, and an idle HTTP/2 connection within about 60.
  <br>**One case remains open and is recorded as `O81` rather than left to be discovered:** a
  connection that sends *nothing at all* is still held, because the code that decides which protocol
  is in use runs before either timeout exists. It is a leak rather than a lock-out — measured at
  about 42 KB per connection, with the dashboard still responding normally under 300 of them — and
  screen-time and bedtime enforcement are unaffected either way, since neither goes near the web
  server.

- **A guard against a Windows privilege-escalation route could not see part of what it guards.**
  Rust resolves a bare program name by searching the running executable's *own directory* before
  `System32`, and this installer runs elevated from wherever you left the `.exe` — so a planted file
  next to it could be run instead of the real Windows tool. A test scans the whole codebase to make
  sure no program is ever launched by bare name. It matched the text `Command::new("`, so any such
  call the formatter had broken across two lines was invisible to it, and the test reported success.
  Demonstrated by inserting exactly that call and watching it pass. Now scans whole text rather than
  single lines, with its own fixture test so the tolerance cannot be quietly undone. The failure also
  names the program now: it used to print the line the call starts on, which for a call broken across
  lines is `Command::new(` and nothing else — so the one report that existed to identify the offending
  binary did not contain it, in exactly the case the scan was rewritten to catch.
  <br>This is the third guard found with the same blindness in one day, after the route guard in
  0.5.1 below and a sibling in the other session's work, and a sweep found no fourth. What makes the
  *next* such guard safe by default — one shared reflow-tolerant reader, plus a build-time check
  that new guards use it — landed separately in the other session's work. Both scanners in that file now read
  tokens through the shared helper rather than carrying private copies of the reflow rule, which is
  what let three guards drift into failing open on the same day. What is still open is recorded as
  `O79`, and is about how the meta-guard picks files to check rather than about any scan.
- **A connection that connects and then says nothing is no longer held forever.** Both network gates
  in front of this service act per *request*, so neither ever saw a client that completed the TLS
  handshake and then sent no bytes at all. Those connections were held indefinitely — measured at
  ~42 KB and one handle each, so not a lock-out, but an unbounded leak that nothing reclaimed and
  that anyone on the LAN could grow at will. The cause sat above every timeout setting: the server
  sniffed each connection to decide HTTP/1.1 versus HTTP/2, and until it knew, it had built neither
  protocol's timeout machinery.
  <br>**The service now speaks HTTP/1.1 only**, which removes the sniffing step and lets the
  30-second header timeout arm immediately. Measured over a socket against the real binary: a
  connection that sends nothing is now **closed after 30.0 seconds**, where it stayed open past 66;
  a half-sent request is closed the same way.
  <br>What HTTP/2 was doing here was multiplexing the live-update stream, which the dashboard treats
  as optional — it already falls back to its 60-second refresh, and that is what happens now if you
  keep a great many tabs open at once. Nothing else changes: the dashboard, the child's page and the
  phone client all speak HTTP/1.1 and always could.
  <br>**Recorded because it nearly went wrong:** the obvious one-line version of this fix is
  actively dangerous. Restricting the server without also narrowing what the TLS handshake
  advertises leaves it offering HTTP/2, which every current browser then chooses — and the browser's
  HTTP/2 opening bytes are gibberish to an HTTP/1.1 parser, so the dashboard would have gone blank
  for everyone. Both halves ship together and a test fails if either is removed alone.
- **The password hasher moved to one generation of its cryptography, and the secrets on the managed
  PC now name their own random source.** `argon2` was pinned at 0.5, which held an older RustCrypto
  stack in the binary alongside the current one — two copies each of `digest`, `block-buffer` and
  `crypto-common`, because `sha2` had already moved on. Upgrading collapses them: duplicate crates
  drop from 20 to 16 and the graph from 398 to 395.
  <br>**Verified before it was made, because getting it wrong locks every parent out of their own
  dashboard** — the hash is written once at `install`, there is no rehash-on-login, and there is no
  remote reset. A PHC string produced by the previously shipped `argon2 0.5.3` was checked against
  the new build: it still verifies the right password and still rejects the wrong one, and the
  defaults are unchanged at `m=19456, t=2, p=1`, OWASP's current minimum. That check now ships as a
  test carrying the real 0.5.3 hash as a constant, so a future bump that breaks compatibility fails
  in CI rather than on a locked-out household.
  <br>Pairing tokens and redeemable time codes previously drew their randomness through
  `argon2::password_hash::rand_core` — a re-export of a re-export of a password-hashing crate. They
  now call `getrandom` directly, which on Windows 10 and later means **`ProcessPrng`**, Microsoft's
  documented primary interface to the per-processor PRNGs. The old path reached the deprecated
  `RtlGenRandom` through `advapi32.dll`, which is itself a thin wrapper around `ProcessPrng` — so
  this is the same bytes from the same generator, with one fewer DLL and one fewer deprecated entry
  point between the child's PC and its secrets.

### Changed

- **You can now keep a copy of your settings, and put them back.** *Settings backup*, at the bottom
  of the dashboard, downloads your curfew, daily limits, app rules, groups, routines and your
  child's language as one file, and restores it. There was previously no way to do this at all:
  the settings live in a locked folder reachable only from an elevated console on the PC, and
  `uninstall --purge` deletes them for good — so rebuilding the machine, or setting up a second
  one, meant re-entering everything by hand, routines included. The file deliberately contains no
  password and nothing about the machine it came from: not the port, not the certificate, not
  today's granted minutes, and **not the trusted-clock anchor**, because restoring another
  machine's clock would quietly weaken curfew enforcement on this one. Restoring never resumes
  enforcement you had paused, never revokes a bedtime extension you granted tonight, and is refused
  outright if the file is incomplete rather than being applied in half.
- **The tool now records what it deletes when the history file rolls over.** History is kept in two
  generations and the older is overwritten; the report card already told you the oldest day it
  could still show, but that describes what survived and cannot tell a new install from one that
  has silently dropped a year. A line is now written at each rollover saying how much went and the
  date it went up to, and it appears in your history download.
- **The screen-time report is now checked against generated histories, not only chosen ones.** The
  numbers on that card have to agree with each other — the total with the columns it is drawn from,
  the average with the days it averaged, the comparison with its baseline — and a disagreement
  between any two of them is invisible to a test that checks either alone. Eight properties now
  hold across randomly generated stores including duplicated days, days in the future, malformed
  rows and an empty history. **They found nothing**, which is the result worth reporting: the
  arithmetic was already right, including the case where the previous period was measured and
  totalled zero, where the percentage is genuinely undefined rather than infinite.
- **Notifications to your child can now be tested.** Nothing in the test suite could observe a
  single thing this product says to a child: every countdown, every warning, and the notice above
  went to a log line the tests could not read. Two real defects had already been found on the one
  message that *was* observable — it was English on every install, and it was the only one that
  never told the child where to ask — and the same exposure sat unwatched on all the others. The
  test double now records them, and the first end-to-end test of a child-facing message that is not
  a shutdown ships with it.
- **The weekly build now actually checks the code against today's compiler.** Its comment claimed
  it already did — that `fmt` and `clippy` ran on a moving `stable`, so a new release's lints would
  surface on a quiet Monday. They never have. `rust-toolchain.toml` pins 1.96.0, and rustup ranks a
  toolchain *file* above `rustup default`, which is the only thing the toolchain action sets; the
  repo's own `build.rs` already said as much ("CI uses 1.96.0"), so two files disagreed and the
  comment was the wrong one. A separate `stable-lints` job now runs `cargo +stable clippy` — the
  one override that beats the pin — on the schedule and on demand, blocking, and nowhere else, so a
  lint shipped this morning cannot fail an unrelated pull request. Measured before adding it:
  current stable (1.98.0, two releases past the pin) reports zero warnings on this tree, so the
  gate starts green and earns its keep on the *next* release.
- **Dependency updates are now grouped by what cargo thinks is breaking, not by what the version
  number says.** Dependabot classifies `0.5.3 → 0.6.0` as a *minor* update; in cargo it is a
  breaking one, and **24 of this crate's ~28 direct dependencies are `0.x`**. So the single
  "minor-and-patch" group — whose entire purpose was to be safe to skim — was swallowing exactly
  the upgrades worth reading, including rustls, axum, argon2 and tower-sessions. The three crates
  the old rule named as needing care were two-thirds unprotected by it. Patch updates keep their
  grouped, skimmable pull request; minor ones now get their own, so the breaking-for-`0.x` class
  arrives labelled. CI catches what fails to compile; it does not catch a cookie default or a
  cipher-suite list that changes quietly, which is the shape these take.

## [0.5.1] — 2026-08-31

### Security
- **The dashboard is no longer open to a page on another port when you view it from an older
  iPad or iPhone.** The check that stops a page your child serves from a different port on the same
  PC from operating your controls relied on a browser header — `Sec-Fetch-Site` — that Safari only
  began sending in **16.4, March 2023**. An iPad Air 2, iPad 5 or mini 4 can never reach that
  version, and that is exactly the kind of device a household ends up using as "the dashboard
  device". On one of those, the check was doing nothing at all: your browser sent the login cookie
  and no header, and the request was let through. Requests that carry no such header are now judged
  on `Origin` instead, which every browser has sent on form submissions since 2008. Nothing that
  worked before stops working — `curl`, health probes and the Android app send neither header and
  are admitted exactly as they were.
  <br>The comment in the source explaining why the header-less case was safe said those callers
  "carry no ambient cookie authority to abuse". That is true of `curl` and false of an old browser,
  which has a full cookie jar — it was the wrong half of the sentence doing the work. `SECURITY.md`
  repeated it, and additionally dated it to "pre-2020 browsers", which was wrong by three years.
  Both are corrected.
- **A guard protecting the login boundary could not see part of what it was guarding.** A test
  scans this project's own source for routes reachable without a password, so one added to the
  wrong router fails the build. It matched the text `.route("` — so any route the formatter had
  broken across lines was invisible to it, and a new unauthenticated route in that shape would be
  absent from both sides of its comparison and pass silently. That formatting is what happens
  automatically as soon as a route gains any per-route setting. Demonstrated against the shipped
  0.5.0 code, which accepted a deliberately unguarded route without complaint. The scan now
  tolerates the line break.
- **The two pages your child can reach without logging in now cap how much data they can send.**
  Both were on the framework's 2 MB default. The existing per-IP limits cap how *often* they can be
  called and said nothing about size, so between them they allowed megabytes of parsing a minute
  from someone who has not signed in. The cap is now 8 KiB — over five times the largest request
  the child's own page can produce.
- Added `Cross-Origin-Opener-Policy` and `Cross-Origin-Resource-Policy`, both `same-origin`. These
  are enforced by the browser rather than asked of it, so they still apply on the older browsers
  the first item above is about.

- **A second guard, this one over the child's own notices, could not see part of what it guarded.**
  Every message the child reads is meant to be built by a translation function, and a test scans the
  codebase to catch one written in place instead. It read the source a line at a time, so a call the
  formatter had broken across lines — which happens automatically once the arguments grow, exactly
  when a message is being composed — was invisible to it. A hardcoded English shutdown notice in
  that shape passed. Demonstrated by inserting one and watching the test report success. It now
  reads whole statements, so a call and the text handed to it stay together however they are laid
  out.

### Documentation
- **The README now says that one install manages one child.** There is a single budget, curfew and
  set of rules per PC, because nothing stored has a person in it — so two children sharing a
  machine share one budget, and a parent using that PC spends their child's screen time. This was
  always true and was never written down, which made it something you discovered by watching the
  numbers behave oddly.

## [0.5.0] — 2026-08-31

### Fixed
- **Changes you make now take effect at once, instead of up to 30 seconds later.** Both enforcers
  woke on a 30-second timer, so anything you did to call off a shutdown — extending bedtime,
  granting time, pausing the rules, switching curfew off — reached them at their next tick. Against
  a 60-second warning countdown that meant acting in the last half-minute could still lose: the PC
  powered off after you had cancelled it. The enforcers now re-check the moment a setting changes,
  measured at well under a second where it was previously up to 30.
- **Approving more time during bedtime no longer looks like it worked.** Screen time and bedtime are
  two independent limits: granting minutes moves the daily budget and has never moved the curfew. So
  a request approved after bedtime had started was applied correctly and the PC shut down anyway,
  with the dashboard reporting nothing but success — the tool made you look like you had broken a
  promise. The grant still goes through (banking minutes is a fair thing to do on purpose), but the
  confirmation now says what will actually happen: *"Bedtime is in force now, so the PC will still
  shut down."* It also warns when a grant is long enough to run into tonight's window — *"Bedtime
  starts in 30 min, so only about that much of this is usable tonight."* Covers both the bonus-time
  buttons and approving a child's request.
- **The dashboard no longer scrolls sideways on a phone, and "Log out" is back on screen.** The top
  bar could not wrap and its buttons were forbidden to shrink, so it needed 461 pixels — wider than
  every phone held upright, including the one the pairing QR is meant to be scanned with. The page
  was pushed 79 pixels wider than the screen: "Shut down" was clipped in half, "Log out" sat off the
  edge entirely, and reaching either meant scrolling the whole page sideways first. A second, less
  visible cause sat underneath it — a card whose contents set a 442-pixel floor for the column every
  card shares — so both are fixed. The page now fits exactly, with nothing cut off and no card
  overflowing inside.
- **The day-of-the-week boxes in the curfew are big enough to hit.** They were 17 pixels wide with
  21 pixels between centres, below the 24 the accessibility guidelines ask for on both counts, and
  they are the fiddliest thing on the page — fourteen of them, aimed at with a thumb.
- **A day in the screen-time report can be opened from the list as well as the chart.** Picking a
  day previously meant hitting its column, which is nine pixels wide and narrower still over a
  90-day window. The dates in *Day by day* are now buttons that do the same thing, so the chart is
  no longer the only way in.
- **Password managers can save the sign-in.** The form had no user-name field, which is what a
  manager keys a saved password on, so it stored and filled the dashboard unreliably — on a page the
  guidance tells you to use a long passphrase with, entered on a phone.
- **`doctor` no longer blames the install's age for something else.** Run anywhere other than
  Windows it reported the clock as anchored by an install that "predates the zone check" — including
  for an install made seconds earlier by the current build — and told you to re-install, which
  cannot record a time zone the platform never reports. It now says which of the two it is.
- The child's page no longer requests a missing icon and takes a 404 on every load.
- **The link your child is given no longer breaks when the router hands out a new address.** The
  install output printed the PC's LAN IP as the child's request-more-time link. That address is
  whatever DHCP happened to assign at install time, so a reboot onto a new lease left the child with
  a link that no longer loaded — the same change that sends a parent hunting for the dashboard, aimed
  at the person least equipped to work out why. It now prints `https://localhost:<port>/ask`, which
  needs no lease, no name resolution, and no knowledge of what the PC is called. The two addresses
  printed above it are unchanged: those are for reaching the dashboard from your phone, which is a
  different problem with a different answer.
- **The shutdown notice is now in your child's language, and stops calling bedtime a "curfew".** The
  two notices Windows shows as it powers the machine off — one when screen time runs out, one at
  bedtime — were written in English on every install. So a Dutch household got a Dutch countdown, a
  Dutch lock warning, and then an English explanation of why the computer was going off, at the one
  moment a child most needs to understand what is happening. Both are now translated. The bedtime
  one also no longer says *"Curfew"*: that is the word for the setting on your dashboard, it appears
  nowhere the child can see, and the countdown they had been reading a minute earlier says
  *"Bedtime"*. Naming the same thing two ways across two messages a minute apart is a small cruelty
  at bedtime.
- **If you have screen time set to shut down rather than lock, your child is now told where to ask
  for more.** The countdown warnings carry the address of their own page; the shutdown notice was
  the one child-facing message that never did. That made it a coin-flip on a setting you probably
  chose for unrelated reasons: an install set to **Lock** told them where to ask, and an otherwise
  identical one set to **Shut down** never did — on the harsher of the two endings, where the
  Windows dialog is the only thing on screen and there is no notification beside it to carry the
  address.
- **Pushing bedtime back now warns you when screen time will stop them anyway.** Bedtime and screen
  time are two independent limits, and the warning only ran one way: granting minutes during a
  curfew window explained that the PC would still shut down, but moving bedtime while the daily
  budget was already spent reported nothing but success — and the machine locked regardless. That is
  the same silent broken promise, on the button added to fix it, which is the one you would reach
  for next. **Later bedtime tonight** now says so — *"Screen time is already used up, so the PC will
  still lock"* — and points at **Add bonus time today**. It also warns when only part of the
  extension is usable: *"Only 20 min of screen time is left."*

### Added
- **You can give a later bedtime for one night.** Screen time had three ways to grant more; bedtime
  had none, so allowing a late finish meant editing the curfew window and remembering to put it
  back. **Later bedtime tonight** (+15/+30/+60 on the Curfew card) pushes tonight's window back and
  then returns it automatically — press it twice for an hour. It survives a restart, it is not
  undone by saving the curfew form, and it cannot switch a disabled curfew on. Stored as a moment
  in time rather than a countdown, so a late-evening extension does not quietly expire at midnight
  and shut the PC down mid-way through the time you granted.
- **The countdown warnings tell the child where to ask.** *"5 minutes of screen time left — good
  time to save."* now carries the address of their own page underneath, so asking does not depend on
  having been told the URL once, months ago. Uses `localhost` and the port you installed on, so it
  works from the machine they are sitting at without depending on the PC's name or its current IP.
  **Not shown during a curfew window** — extra time cannot move bedtime, so inviting them to ask
  there would promise something that cannot be delivered, and you would be the one left saying no.
- **The child's own page now shows their last seven days, and how often you looked.** Two facts
  about the child, told to the child. The week is totals only — a small bar per day, with a day
  nothing was recorded hatched rather than drawn as a zero, the same way your own chart marks it.
  Underneath it, a count: *"Your screen was looked at 3 times today."* Counts, never times: a list
  of times would be a timetable to plan around, and nothing is lost by aggregating, because Windows
  already draws its yellow border while a screen is actually being captured — the child could
  always see a look happening, just never that it had happened. Both in Dutch too.
  <br>Deliberately no app names and no page titles: `/ask` needs no sign-in, so anyone on your home
  network can open it, and a per-app breakdown there would publish your child's browsing to the
  household. The rules stay off that page as they always have — no blocklist, no limits, no bedtime.
- **The screen-time report says how far back it can see.** *History from 2026-07-01* now sits beside
  *Measured days*. Recorded history is not kept forever — the oldest days are deleted as the log
  rotates, with no setting and, until now, no notice — so a 90-day report could quietly be all there
  would ever be. This does not change what is kept; it makes the limit visible rather than something
  you discover. The date is the oldest day held on disk, so it does not move when you switch between
  7, 30 and 90 days.
- **Releases now carry a signed list of everything compiled into the binary.** `nestwatch.sbom.json`
  is published beside the `.exe` and attested to it, so the components of a binary you already
  installed can be checked against a newly disclosed vulnerability without rebuilding it. Verify with
  `gh attestation verify nestwatch.exe --repo emrecdr/nestwatch --predicate-type https://cyclonedx.org/bom`.
- The dashboard has a skip link and a top-level heading, so a keyboard or screen-reader user can
  reach the last card without passing the roughly 120 controls in front of it.

## [0.4.0] — 2026-08-27

### Security
- **A time-zone change could push bedtime back by up to two hours, every night.** Changing the time
  zone needs no administrator rights and raises no prompt, so it was already defended against — but
  the defence compared the *offset* the machine reported, and allowed an hour of slack so that real
  daylight-saving changes still worked. That slack was measured from the install-time offset, and
  true local time also drifts an hour from it every summer, so the two added up: an install set up
  in winter could be moved **two hours** in summer by selecting UTC. A 21:00 curfew became 23:00.
  <br>The fix is to compare the time zone *itself* rather than the offset it produces. Two different
  zones share an offset for half the year — Amsterdam in winter and London in summer are both the
  same number — which is exactly the ambiguity that was being exploited. Daylight saving now works
  precisely rather than by tolerance, because the machine's own zone is believed outright.
  <br>**Re-run `install` once to get this**, or use the new re-anchor button. An install upgraded in
  place keeps the older, weaker check, and `doctor` now says so plainly rather than reporting the
  clock as fine.
- **The release build can no longer be tampered with by a third-party action.** Three of the actions
  used to build the released `.exe` were referenced by a name that their authors can move at any
  time, and they run *before* the compiler in the same job that signs the result. Anyone able to
  move one of those names could have changed the source between checkout and build, and the
  signature would then have vouched for the altered binary perfectly truthfully — `gh attestation
  verify` would have passed on your machine. All of them are now pinned to exact, unchangeable
  versions. The rule is no longer "pin the ones that hold a key" but "pin anything that runs in the
  job that builds or signs".
  <br>Building and signing have also been separated into two stages, so the stage that compiles the
  program no longer holds the signing key at all. Pinning stops a name being moved; this stops it
  mattering. Nothing about the released file changes — the same `gh attestation verify` command
  works exactly as before — but the signature now vouches for something built where nothing had the
  means to sign.

### Added
- **Take your own history off the machine.** A **Download** button on the screen-time report saves
  every day this install still holds as a single file. Until now the history lived in a folder
  locked to Administrators — which is what stops your child reading it, and also stopped you — and
  `uninstall --purge` deleted all of it with no way to keep a copy first.
  <br>The file is the rows exactly as stored: nothing merged, nothing filtered, so you can check it
  against what the dashboard shows rather than take the dashboard's word for it. If the same day
  appears twice, that is what is on disk and the file says so.
  <br>**Worth knowing while you are looking at it:** the history has a ceiling. Each log keeps two
  generations and the older is deleted when it rolls over, so the oldest days do eventually fall
  off, and nothing warns you when they do. How long that takes depends on your child rather than on
  this program — it is set by how many apps and pages they use in a day. For light use it is
  decades; for a child hitting the 40-page cap every day it is closer to two or three years.
- **Your child's page can be in Dutch.** Their own page and the countdown warnings that appear on
  their screen; this dashboard stays in English. Choose it under **The child's language**.
  <br>Set by you rather than picked up from their browser, and that is deliberate: the most
  important sentence on their page is the one telling them what this tool can see, and the person
  being watched should not be the one choosing the language it is written in.
- **Re-anchor the clock without reinstalling.** If the PC genuinely moves to another time zone, the
  curfew used to stay on the old one until you re-ran `install` from an elevated console, standing
  at the machine. There is now a button. It asks first, because doing it straight after your child
  has changed the zone would accept their change as correct — you are the only one who knows which
  happened.
- **`doctor` now reports the clock.** It says which time zone the enforcement is anchored to, and
  **fails loudly** when the machine is set to a different one — which means either the PC moved, or
  somebody changed it and the limits held.
- **A request reaches you the moment it is made.** The dashboard used to notice a new request
  within a minute; now it is a second or two. It still checks every minute as well, so a phone that
  slept through the notification is never left showing a stale page.
- **Today's answer sits at the top of the dashboard** and stays there while you scroll: minutes
  left, requests waiting, and whether enforcement looks stopped. Eleven cards on one page meant the
  thing you open it for several times a day was in the same queue as the things you set once.

- **The pairing QR now carries the certificate's fingerprint, so an app can check it instead of
  asking you to.** Groundwork rather than a feature you can see today: nothing in the browser flow
  changes, because the fingerprint rides in the part of a web address that is never sent to a
  server. Scan the QR with a phone camera and it behaves exactly as it always has.
  <br>What it is for is the difference between *trusting* the first connection and *checking* it.
  Any app that pins this PC's certificate has to learn what the right one looks like from
  somewhere, and until now the only honest answer was to show you 95 characters and ask you to
  compare them against `nestwatch fingerprint` by eye. People do not really do that, and the
  research on it is unkind — attacks on inattentive comparers succeed somewhere between 6% and 72%
  of the time depending purely on how the characters are laid out. A fingerprint that arrives in a
  photograph of your own console is one nobody on your network was in a position to substitute, so
  the very first connection is verified rather than assumed.
  <br>The one visible consequence is that **the QR is denser** — it holds about three times as
  much, which takes it from 41 to 53 characters wide in the console. That was measured against the
  longest computer name the printed address can use, not just against a short IP, because a QR that
  wrapped would be unscannable and the whole thing would be worse than not doing it. If your
  certificate file cannot be read for any reason, the QR is printed exactly as it was before rather
  than the install failing over a decoration.
- **The report now flags game portals by name.** A page called "Poki - Free Online Games" now
  carries a small **Poki** badge beside it, on today's card and on every day of the screen-time
  report. Roblox played through the native app was always measured exactly, by program name; played
  through a browser it was just another page title in a list, which is the route a child takes when
  the app itself is blocked. The badge costs nothing to produce — the title was already being
  recorded — and it changes no measurement.
  <br>**It is a label, not a claim of coverage.** A renamed tab defeats it, and so does any site not
  on the list. The card says so: no badge means nothing was recognised, *not* that nothing was
  played. The badge carries the site's name rather than only a colour, so it does not depend on
  being able to tell two shades apart.
- **`doctor` now tells you when the machine cannot take screenshots at all.** On Windows 10 older
  than version 1903 the capture cannot work, and `doctor` used to report everything green while
  every screenshot failed forever — sending you looking in the wrong place. It now names the build
  and says plainly that only the picture of the screen is affected. On a supported machine it says
  so too, rather than staying silent: a check that says nothing looks exactly like a check that
  never ran.

- **The screen-time card now tells you when it checked and found nothing.** A day that introduced
  no new program used to look exactly like a day the check could not run — both were blank space.
  It now says so in a line: "Checked — nothing new today, against 40 earlier days of history".
  Plain text rather than the warning panel, because most days are quiet ones and a warning that
  appears every day stops being read.
  <br>**And it now says when the check has stopped.** If thousands of different program names have
  been used, there is nothing dependable left to compare against and the check gives up rather than
  calling familiar programs new. That used to be silent and permanent, and looked identical to a
  freshly installed machine — so the one behaviour the limit exists to catch was the one that left
  no trace. It now shows a warning naming the cause.

### Changed
- **The settings card is now called "Limits & rules".** It sat directly above a card called
  "Screen time" while itself being called "Screen-time & app limits" — one sets the limits, the
  other reports what was used, and on a phone, where you see one card at a time, the name was all
  you had to tell them apart.
- **The dashboard is about four times smaller to load, and a repeat visit sends almost nothing.**
  It was re-downloading all 324 KB of itself on every single visit, because it told your browser
  not to keep any of it. It now sends roughly 85 KB the first time and, on later visits, only asks
  whether anything changed — which on a phone at the far end of the house is the difference between
  a wait and no wait. Nothing is ever served stale: the browser still checks every time, it just
  stops re-sending what has not moved.
- **A rejected setting now tells you what the limit actually is.** "Minutes out of range" became
  "minutes must be between 1 and 240". Four places did this, including the one that let your child
  ask for more time.
  <br>The dashboard now shows you those sentences. It had been throwing them away and printing its
  own copy of the number instead — so the two could disagree, and raising a limit meant editing it
  in two places or being told the old one. Granting bonus time, issuing a code and applying a
  routine all now repeat what the server actually said. Refusing a code is the clearest gain: it
  used to say "Minutes 1–240, and at most 50 active codes" because the page could not tell which of
  the two had stopped it, and now it tells you which.
- **Your child is told what happened to their request.** They could ask, and the page could never
  answer: a denial reached them through no channel at all — it looked exactly like being ignored —
  and an approval showed up only as a number that changed by itself. It now says which, as soon as
  you decide.

- **Time codes are now six characters instead of eight.** The code you leave for your child to type
  in — worth a set number of extra minutes — is shorter to read off a note and shorter to retype
  without a mistake. The alphabet already leaves out `I`, `L`, `O` and `U` so there is no character
  that can be mistyped into a different working code.
  <br>**This does not meaningfully weaken it.** Six characters is still over a billion
  combinations, and what makes a code unguessable here was never its length: redemption is limited
  to five attempts a minute from any one device, which puts guessing one at roughly four hundred
  years. Codes remain single-use, worth 1–240 minutes, capped at fifty outstanding, and are never
  written to the audit log.

### Fixed
- **Buttons that looked enabled and quietly did nothing.** While the live view was fetching a
  frame, **Take screenshot**, **Expand** and the overlay's **Refresh** accepted your click and
  discarded it — no message, no spinner, nothing. Because a capture can take up to 15 seconds
  while the live view refreshes every 5 seconds by default, and as often as every 2, that was the
  usual case rather than a rare one. Your click now takes priority and cancels the frame already
  in flight.
- **Expand now stays sharp while the live view is running.** Opening the full-size view fetched a
  full-resolution frame, and the live view replaced it with a stretched preview within one
  refresh — and the live view being on is exactly the state you are in when you press Expand.
  Frames now follow whichever view is on screen: full while the big picture is open, preview for
  the thumbnail.
  <br>That is the most expensive thing this tool does, so it is bounded: it lasts only as long as
  you keep the overlay open, the unattended-session cap still applies, and the refresh-rate buttons
  are on the card if you would rather trade sharpness for cost.

## [0.3.0] — 2026-08-26

### Before you update
**Screenshots and the live view now need Windows 10 version 1903 or newer.** This is the one
thing here that can take away something you already had: the capture backend was replaced to
fix a child being able to defeat screenshots from a game's own display settings, and the new
one has no fallback on older builds. Everything else — screen-time limits, curfew, blocked
apps, the whole enforcement half — works normally on an older Windows, and `install` tells you
rather than refusing. Check with `winver`; anything still getting Windows updates is well past
it.

**Windows also draws a yellow border around the screen while it is being captured.** That is
the operating system's own disclosure and this app neither can nor tries to suppress it. Your
child will see when you are looking.

### Added
- **The report now shows *when* the PC was used today, not just how much.** A 24-hour strip above the
  totals, so "was he on at two in the morning?" is a glance rather than a guess — the one question
  every other number on that card cannot answer. It marks a session that is still running, and it
  marks one whose end it does not know, which happens when the service stopped while your child was
  still using the machine. Those are drawn as a marker with no width on purpose: pretending to know
  when it ended would be worse than admitting it doesn't.
- **The report now tells you when an app turns up for the first time.** A total tells you how much;
  this tells you what *changed* — a game that appeared yesterday and never before, a chat client you
  have not heard of. It sits above the totals because it is the one thing on that card you might
  need to act on, and it names how much history is behind the claim: "against 32 earlier days" and
  "against 1 earlier day" are very different statements, and only you can weigh them.
  <br>It notices an app when it is **used**, not when it is installed — which is also how the
  commercial tools do it, and it is the only signal available to a tool that deliberately watches no
  registry. An app installed and never opened is not a fact about your child's day. On the first day
  of history it says nothing at all rather than declaring everything new, and a quiet day shows no
  panel: a notice that appears every day stops being read.
- **The live view is dramatically cheaper, and you can choose how often it refreshes.** It used to
  send a full, lossless picture of the whole screen every three seconds — on a 4K monitor showing a
  game that is **20 MB per frame**, over your home network, from a laptop that is often your child's,
  to fill a panel a few hundred pixels tall. Live frames are now sized for the panel they go in:
  around **30 KB**, and near enough the same on any monitor. Clicking the picture, or **Expand**,
  still fetches a full-resolution one, because that is the moment you actually want to read
  something. A **2s / 5s / 15s** choice sits beside the Live toggle, and the live view now stops
  itself after fifteen minutes rather than running all day in a tab you left open.
- **A light/dark switch, in the top bar.** ☀️ and 🌙, with **Auto** beside them and selected by
  default — Auto follows whatever your phone or laptop is set to, which is what the dashboard has
  been doing since the theme was unpinned. The switch is for when that setting is wrong for the
  moment: checking at eleven at night on a phone still in daylight mode, or the reverse. Your choice
  is remembered on that device, and choosing Auto forgets it rather than freezing today's answer.
- **The three things you open the dashboard for are answered before anything else.** Is enforcement
  running, how much time is left today, is anything waiting for you — they used to live in three
  cards that were not next to each other, one of them below the fold on a phone, which is the device
  the setup QR hands you. Each says *unknown* when it is unknown, rather than guessing the
  reassuring answer.
- **The tab title tells you when your child is waiting.** It reads `(1) Nestwatch` while a request is
  pending, so a dashboard left open in a background tab is enough — you no longer have to look at the
  page to find out. It never shows `(0)`: a count it could not fetch is not a zero.
  This is the whole of what is possible without sending anything outside your house. Push
  notifications need a company's servers in the middle; a home-screen app cannot inherit the
  certificate you approved. Both are declined deliberately, and the cost is that a tab has to be open
  somewhere.
- **The report can now show time by category.** If you have grouped apps together — Games, School —
  those totals were visible for today and thrown away overnight. They are now kept, and the report
  leads with them, because "Games: 14 h" is a sentence and twenty file names is a puzzle.
- **Today's screen time now shows which apps your child has actually been in front of** — not just
  a total, and not a day late. Those minutes were already being measured every thirty seconds and
  written to disk; they only ever reached you in the next morning's summary, describing an evening
  that had already happened. The Today card now lists the ten apps in front longest so far, and the
  browser pages beside them.
  It also tells you when it *cannot* answer. An empty list on a busy afternoon used to look
  identical to a quiet one; if the helper that measures this is not running, the card now says so
  rather than showing nothing and letting you assume the best. It stays quiet on a genuinely quiet
  morning — the warning only appears once there is enough use on the clock to contradict it.
- **The screen-time report can answer "how much Roblox this month?"** Every breakdown on that card
  used to show exactly one day, so you could see last Tuesday and never a total. There are now
  most-used lists across the whole window, for time running, time in front, and browser pages.
- **You can choose how far back the report looks — 7, 30 or 90 days.** The setting existed and
  worked from the first release; nothing in the dashboard ever asked for it, so everyone saw thirty
  days forever.
- **Clicking a day on the chart shows that day.** The chart invited the question and could not
  answer it: the lists underneath were pinned to whichever day the code picked. Choose a column and
  all three follow it, including saying "nothing recorded" for a day that has none. Choose it again
  to go back. The columns are proper buttons now, so they work from the keyboard and announce
  themselves.
- **Apps are named the way you would name them.** `RobloxPlayerBeta.exe` reads as Roblox,
  `javaw.exe` as Minecraft (Java). The real file name is still there if you hover, and nothing
  underneath changed — limits still match on the file name, because that is what Windows gives us.
  Long totals read as `2 h 30 min` rather than `150 min`.
- **Per-app screen time can now measure what was actually in front of your child — but this has
  never run on a real machine.** ⚠️ The code is written and its arithmetic is tested; the half that
  talks to Windows has not executed once, here or anywhere. **Work through §D2 of
  [WINDOWS-TESTING.md](docs/WINDOWS-TESTING.md) before relying on it**, because the failure is
  quiet: if it does not start on your PC the new columns are simply empty, which looks like a child
  who used nothing rather than a feature that did not run. Nothing else is affected — totals,
  curfew and every limit behave exactly as before; this only measures.
  What it does when it works: until now a minimised game and a game being played looked identical,
  because an app counted while its process ran. A small helper runs inside your child's session,
  notices which window has focus, and reports back every 30 seconds; the daily report carries those
  minutes beside the old ones, so you can see that Roblox was *open* for two hours and *played* for
  forty minutes.
  Time spent away from the keyboard doesn't count, and the away-detection is exact rather than
  approximate: Windows reports how long ago the last key or click was, so the moment your child
  stopped being at the PC is known precisely instead of being guessed at when a timer trips.
  Three deliberate limits, written down in
  **[docs/FOREGROUND-TRACKING.md](docs/FOREGROUND-TRACKING.md)**: it **reports, it never enforces**
  (per-app limits still count running time, and a test exists to keep focus figures out of the code
  that decides when the PC locks — the helper runs as your child, so its numbers are your child's
  to choose); it identifies web use from **window titles only**, because the alternative was
  reconfiguring your children's browsers to harvest every domain they visit; and **an unmeasured
  stretch stays unmeasured** — if the helper is killed, that time reads as unknown, never as a
  confident zero.
- **The report can say what the browser was showing, not just that a browser was open** — same
  helper, so the same ⚠️ applies: **never run on a real machine.** Roblox in its own app was already
  named; Roblox streamed through a cloud-gaming site in a tab looked exactly like homework, because
  both were "chrome.exe". Browser time is broken out by **page title** — what the tab was called.
  Titles only, deliberately: **not addresses, and not a browsing history.** Reading the title of a
  window that is already in front costs nothing, while collecting the domains your children visit
  would have meant changing their browsers' DNS settings behind their backs. The list is capped at
  the heaviest few dozen titles a day, so a page that retitles itself in a loop cannot bloat the
  stored history.
  **Do not assume private browsing is covered.** Reading a window title *should* work there, since
  private modes hide history rather than window titles — but that has not been confirmed on a
  running machine, and until it is, treat Incognito as a gap rather than as watched.
- **[docs/MOBILE-APP.md](docs/MOBILE-APP.md) — what a phone app would and wouldn't buy.** Researched,
  not built. It would remove the certificate warning; it could not tell you about a time request
  while you're away from home, because that needs a cloud service this design refuses; and it would
  be a second interface to keep in step with the first, forever. Includes the two findings that
  cost the most to establish: an installable web page **cannot** work here (an iOS home-screen app
  doesn't inherit Safari's certificate exception), and the certificate-pinning recipe in the most
  popular Dart HTTP client's own documentation sends your password before it checks the certificate.
- **[docs/REMOTE-ACCESS.md](docs/REMOTE-ACCESS.md) — reaching the dashboard from outside the
  house.** Off-LAN access is still not a feature and still unsupported; what the guide adds is an
  answer to the question people ask anyway. Which arrangements work (a VPN that puts you on the
  home subnet), which quietly don't (port-forwarding, and tunnels that terminate at somebody
  else's server), why the monitored PC must never be the one running the tunnel, and what each
  choice costs — including what it does to the source addresses in your access log.

### Improved
- **The picture now tells you how old it is.** Under a live view you get "updated 4s ago", counting
  up on its own — and if the frames stop arriving it says so, in red, naming the time of the last
  one. Previously a live view that had stopped working looked *exactly* like a child sitting
  perfectly still: the last good picture stayed on screen, the toggle stayed lit, and nothing
  anywhere said the service had stopped, the child had signed out, or the capture had failed. For a
  feature you open at the moments you are most worried, that was the worst thing it could do.
- **Turning the live view off now stops it at once.** A capture already in flight is cancelled
  instead of being allowed to land afterwards — a picture that appears seconds after you said stop.
- **Five panels you rarely open are now folded away.** Routines, Time codes, Recent access, Usage
  history and Change password sit behind a heading you tap to open. On a phone — the device the
  setup QR hands you — you passed all five in full before reaching anything else, in the order they
  happened to be written rather than the order you need them. They stay one tap away, remember
  nothing, and work from the keyboard.
- **The dashboard now follows your device's light or dark setting.** ⚠️ *This changes how it looks.*
  A light theme was being built into every release and then made unreachable — both pages pinned
  themselves to the dark one, so roughly half the stylesheet shipped to every install and could
  never be shown. The pages no longer pin a theme, so a phone or laptop set to light mode gets the
  light dashboard and one set to dark gets the dark one. If you preferred it always dark, that is a
  one-line change in `web/src/app.css` — set `themes: dim --default, light;` and rebuild.
- **Eight more controls can be used with a screen reader.** The curfew on/off switch announced only
  as "checkbox, not checked", with nothing tying it to the heading beside it, and seven boxes in the
  rules editor — blocked apps, per-app limits, app groups, the routine name — had nothing but a
  greyed-out example inside them, which is a hint and not a name, and which disappears as soon as
  you type. Continues the pass that gave the tables their headers; the ✕ buttons beside those same
  rows were already labelled, which is what gave the omission away.
- **The dashboard is readable with a screen reader.** Its six tables had no column headers as far
  as assistive software was concerned — the header row was styled, not labelled, so a figure was
  read out without the column it belonged to. And the two panels that refresh by themselves every
  minute, today's usage and the more-time requests, changed silently: you were told the numbers
  once, on load, and never again. Both fixed, and a test now refuses a seventh table that repeats
  the first mistake.

### Fixed
- **Days over budget are now marked by a pattern, not only by colour.** The screen-time chart showed
  them in red against green, and in this theme those two are almost exactly as bright as each other
  — so to a red-green colour-blind parent the chart's whole point was invisible. Over-budget bars
  now carry diagonal stripes as well, at the opposite angle to the ones already used for days that
  weren't measured, so those two can't be confused either. The wording screen readers get was always
  correct and is unchanged.
- **The key beneath that chart now matches the bars it explains.** Adding the stripes above fixed the
  bars and left the little key underneath showing the old flat colours — so "over budget" was a plain
  red square sitting next to striped red bars, and "not measured" a plain grey one next to hatched
  ones. Two of the three were wrong, and wrong in exactly the way the stripes were added to fix: a
  parent who can't separate those two colours was handed a key written in the pair they can't read,
  explaining bars that had already been corrected. The key is now drawn by the same code that draws
  the bars, so it can't drift from them again.
- **The "updated 4 seconds ago" line under the live view no longer stops telling the truth.** The
  live view now switches itself off after fifteen minutes, and when it did, that line simply froze —
  still reading "updated 4 seconds ago", in the same ordinary grey, above a picture that could by
  then be hours old. That is worse than having no line at all: instead of leaving you unsure how
  fresh the picture is, it answered confidently and wrongly, which is the one thing the line exists
  to prevent. It now keeps counting for as long as a picture is on screen, however the live view
  stopped — whether it timed out, you switched it off, or it failed.
- **Pausing the rules now records that it happened, and says which kind of pause it was.** The usage
  history logged when a session of active use *began* but, on the pause path, never that it ended —
  so a history read back later showed sessions starting and never finishing. It also called two
  different things "paused": the toggle you pressed, and simply having no rules set up. Those now
  read as **paused** and **no rules** respectively, because only one of them is something you did.
- **The screen capture no longer comes back black for fullscreen games or for Netflix.** This is
  the most serious thing in this release. The capture was reading the composited desktop, which
  covers ordinary windows and ordinary browsing, and misses anything drawn straight to the screen —
  a game in **exclusive fullscreen**, and DRM-protected video. It was not a rare edge case: it is a
  radio button in nearly every game's own display settings, so a child who noticed could switch it
  on and permanently defeat the screenshot with no prompt, no password and no administrator right.
  You would have seen a black rectangle, which looks identical to a monitor that is switched off.
  <br>Windows now draws a **yellow border** around the screen while you are watching. That is the
  operating system's doing and this app neither can nor tries to suppress it — and on reflection it
  is the right trade for a tool that already tells your child, on their own page, that you can see
  their screen. The border makes that promise something they can verify rather than something they
  have to take on trust.
  <br>This raises the Windows requirement to **version 1903 or newer** for screenshots specifically.
  Everything else — screen-time limits, curfew, blocked apps, per-app limits — still runs on older
  builds, and the installer says so rather than refusing.
- **Screenshots on a scaled display should no longer lose part of the screen.** On the 125% and 150%
  scaling Windows picks by default for most laptops, the capture asked the system for a picture
  larger than the screen it was reading from — predicted to lose about a third of the frame at 125%
  and over half at 150%. Marked *should* deliberately: this one is reasoned from the two APIs
  involved and has not yet been confirmed on real hardware.
- **The screenshot is now of the primary monitor, as the app always claimed.** With two screens it
  took whichever Windows happened to list first, which is not necessarily the main one — so it could
  have been quietly watching the wrong screen, indefinitely, with nothing on the page to say so.
- **Updating over an existing install could silently do nothing.** If anyone was signed in to the PC,
  the update failed to replace the program file and quietly restarted the old version instead — so you
  would run the installer, see it finish, and still be on the previous build. The cause was a
  background process left over from the running version, which keeps the program file open; the
  installer now closes it first. It never showed up in testing because it only happens when someone
  is actually logged in, which is most evenings and no test machine.
- **Uninstall could report success while leaving parts of itself behind.** Same cause: a file it could
  not delete produced a note in passing and an otherwise cheerful ending. It now stops and tells you
  exactly what is still there and how to finish the job. Getting this wrong in the reassuring
  direction is the worst option available — you would walk away believing the controls were gone.
  It also now checks the end state rather than each step: the firewall rule is confirmed gone
  (removing it was previously fire-and-forget, so a failure was silent), and a service that Windows
  has only *marked* for removal — which happens whenever the Services window is open — is reported
  while you are still looking at the uninstall, instead of surfacing later as an unexplained error
  on your next install.
- **`doctor` now tells you if you are running a different build from the one installed.** Copying a
  new version onto the PC does not update the service, so it was possible to download a fix, run the
  check from the download, and be told everything was fine about a service still running the old
  code. It now says which build is installed and which one you just ran.
- **The switch beside "Curfew" now says what it is doing.** It was a bare toggle: you could see that
  it did *something* and had to guess what. It now reads **Off**, **On**, or **On — no hours set**,
  matching the switch on the screen-time card, which has always shown its state. The third case is
  real — a window whose start and end are the same time never fires, so a curfew can be switched on
  and still do nothing.
- **The bedtime time boxes were clipping their own clock icon.** The field was about 39 pixels wide
  inside its padding; "22:00" needs 33 of them, which left no room for the picker button and it
  landed on top of the digits. Both pairs are now wide enough.
- **The selected item in a group of buttons no longer wears the "do this" colour.** The chosen theme
  and the chosen report window were painted the same green as **Save** and **Take screenshot**,
  which made a settled choice look like something you still had to press. They now use a quieter
  selected style. The green means "this is the action to take" and is worth keeping for that.
- **Your child is now told the screen can be seen.** One line on the page they use: *"A parent set
  this up and can see this screen, which apps you use, and how long for."* Nestwatch already goes out
  of its way not to know things — it records what a tab was called and never its address, and refuses
  to read browser history at all. Being silent about the screen was the one place that did not match.
- **A pending request from your child could be completely invisible.** If the dashboard could not
  reach the service, the "More-time requests" card and its counter were both hidden — they only
  appeared when the count was above zero, and a failed check left the count at zero. So a child who
  had asked for twenty more minutes and was waiting looked exactly like a child who had asked for
  nothing. The card now stays on screen and says the answer is unknown, and the counter reads `?`
  rather than claiming a number it does not have.
- **Signing out left the previous session's figures on screen.** Only three things were cleared;
  the access log, usage history, screen-time report, time codes, pending requests and saved routines
  all survived, so the next sign-in showed stale numbers as current until each was re-fetched. A tab
  left open overnight showed yesterday as today.
- **Clearing every day on a bedtime window made it apply every day.** Unticking all seven boxes is
  the natural way to say "not this week", and it does the opposite — an empty selection means daily.
  That was true before and is still true, because it is what an unset schedule has to mean; what
  changed is that the window now says *"Applies: every day"* beside itself, in a warning colour when
  it happens by accident. Previously the only hint was the smallest text on the card, below the
  whole set of windows, and it never reacted when you cleared the last box.
- **The bedtime day boxes could not be told apart.** They were labelled with one letter each —
  `M T W T F S S` — so Tuesday and Thursday looked the same, as did Saturday and Sunday, and a
  screen reader announced nothing more than "T, checkbox". They now read `Mo Tu We Th Fr Sa Su` and
  announce the full day name.
- **Forms across the dashboard had been rendering unstyled since the interface library was
  upgraded.** Four class names were removed by that upgrade and left behind in the markup — 69
  references across both pages. Nothing reported it, because a class that no longer exists still
  looks like styling to anyone reading the source; the browser simply has no rule to apply. Two of
  the four were doing real work: one set the size of every field label, the other stacked labels
  above their inputs. Both are restored, and a test now compares every class in the markup against
  the stylesheet that actually ships, so the next upgrade's leftovers fail the build instead of
  quietly changing how the product looks.
- **"0 min used today" was shown before anything had been loaded.** The card started from a
  placeholder of zeroes and read them out as measurement, so a dashboard that had not yet reached
  the service — or could not reach it at all — stated as fact that your child had used no time. It
  now says the figures have not loaded until they have. Figures that *did* arrive stay on screen if
  a later refresh fails: they are then out of date rather than unknown, and the staleness warning
  already covers that case. Signing out forgets them too — otherwise the next sign-in showed the
  previous session's numbers as today's, and a tab left open overnight made "today's" mean
  yesterday's.
- **A failed request for more time looked exactly like no request at all.** When the dashboard could
  not load a list, it kept whatever it had and said nothing, so a card with nothing in it meant
  either "nothing to show" or "this failed and you were not told" — and there was no way to tell
  which. That is worst on pending time requests, where both the header badge and the card itself
  are hidden when the list is empty: a server error rendered as a child who had asked for nothing,
  on the screen whose whole job is to show that they had. Failures are now reported, and the lists
  that had no message to show — routines, time requests, one-time codes — have one. The error
  messages the other lists already carried were dead code for the case that actually happens: they
  fired only if the network dropped, never if the server itself returned an error.
- **A day's per-app detail could be silently dropped.** When the same date turned up in both
  `usage.jsonl` and `screentime.jsonl`, the report kept whichever row listed more apps — so a row
  holding the same apps *plus* richer detail could lose the tie and have that detail discarded. The
  day still displayed, with less in it than was recorded, which is why nothing looked wrong. Found
  while adding the `focused` map, and it would have thrown that map away on exactly the installs
  that upgraded mid-life.
- **Denying a request could have granted it.** The approve/deny handler took a yes/no flag, and
  anything that was not literally "no" counted as yes — so a wrong value granted the child the
  minutes instead of refusing them. The two buttons on the page always passed the right thing, so
  this was never wrong in normal use; it is fixed because the direction it failed in is the wrong
  one for a parental control, and because the mistake is invisible at the call site. The decision
  is now spelled out, and anything unrecognised denies.
- **The dashboard could tell you enforcement was fine when it could not reach the PC at all.**
  The "enforcement may not be running" warning is the one that matters most — every other number
  on the page looks normal when the limits have quietly stopped being applied. It was suppressed
  in exactly the case it exists for: if the dashboard's request to the service failed, the page
  kept its starting values, read the missing answer as a good one, and showed nothing. It now
  stays quiet only until the first answer has been *attempted*, and reports honestly after that —
  including when the attempt failed. Both banners go through the same check, so they cannot
  disagree.
- **The screen-time chart drew nothing.** Thirty days of data, an empty chart, and no message to
  say why — the figures above it and the day-by-day table below it were right the whole time, so
  the page looked merely bare rather than broken. Shipped in 0.2.3, found by opening the dashboard
  in a browser rather than by reading it. The bars are now built as HTML instead of SVG, which
  removes the cause rather than working around it, and a test refuses the shape that caused it.
  *Cause, for the curious:* the bars were repeated by an Alpine `<template>` placed inside the
  `<svg>`. A `<template>` written inside `<svg>` is parsed into the SVG namespace, where it is not
  an HTML template and has no content to clone, so the loop produced nothing.
- **A new security advisory could go unnoticed for as long as nobody pushed.** The check that fails
  the build on a vulnerable dependency only ran when something else triggered a build, and GitHub's
  own alerting was switched off for the repository — so for a tool that gets installed and then left
  alone for months, nothing was watching in between. Alerts are on, advisories now open a pull
  request by themselves, and the full check also runs every Monday.
- **The build now notices when the compiled stylesheet is older than the toolchain that compiles
  it.** It already warned when the CSS was older than the markup. It could not see a Tailwind or
  daisyUI bump, so a build here could be styled by one version while the release was styled by
  another — which is exactly what had happened: 0.2.3 shipped with 4.3.3 against a local 4.3.2.
- Four `unsafe` blocks in the Windows code kept their safety reasoning in the function's
  documentation rather than beside the block. The reasoning was right; it was in the place a reader
  scrolling to the block does not look. All eight now match, and the lint that catches it is on.

### Security
- **An hour of live viewing no longer erases your security history.** Every live frame wrote its own
  line to the access log. At the old refresh rate that is 1,200 lines an hour, and the log keeps
  about 4 MB — so roughly **two days of live viewing would push out every login record**, every app
  closed, every password change, to make room for a timer. Watching a screen and losing the record
  of who signed in is a bad trade. Detailed captures — the button, and **Expand** — are still logged
  one for one, because those are few and deliberate; the live view's small frames are now counted
  and written as one line every five minutes. The log reads better for it: *the screen was watched
  for forty minutes, and looked at closely five times*.
- **Nothing served by Nestwatch is stored in your browser's cache any more.** A picture of your
  child's screen is the most sensitive thing this app produces, and nothing had ever told the
  browser not to keep it on disk.
- **The dashboard can no longer run code built from a string.** `script-src` is now `'self'` alone
  — earlier this release it stopped allowing inline scripts, and it now also refuses `eval` and
  anything like it. That closes the last route by which injected text on the page could become
  running code.
  The interface library was the reason it was ever allowed: its normal build turns every expression
  written in the markup into a function at runtime. The page now ships that library's strict build,
  which reads those expressions with its own parser and cannot reach anything outside the component.
  Twenty-six expressions moved out of the markup to suit it. Verified with the tighter rule actually
  in force: the dashboard loads with no errors and everything still works.
- **A tampered-with screen-time helper can no longer grow the service's memory or your disk without
  limit.** The helper that measures which window is in front has to run as your child to see your
  child's windows, so everything it reports is treated as something your child could have chosen —
  and the checks on it bounded what the numbers could *say* without bounding how many of them there
  could be. A modified helper could name thousands of invented programs, or send one line that
  never ends. There are now ceilings at each point the data comes to rest, set far above anything a
  real machine produces, and the busiest genuine entries are the ones kept — so a flood costs the
  flood, not the record of what your child actually used. This only ever affected the new
  never-yet-released tracking; nothing that was shipped behaved this way.
- **The dashboard can no longer run a script that arrives in its markup.** Both pages kept their
  JavaScript in the page itself, which meant the policy had to permit inline scripts in general —
  and that permission is what an injected `<script>` would have needed. The code now lives in two
  ordinary files, so the permission is gone. Nothing about the page changes for you; this closes
  the gap between "we don't do that" and "that can't happen".

### Internal
- **The build no longer claims the stylesheet is out of date when it isn't.** Tailwind skips writing
  the file when the compiled output would be byte-identical, so the build's freshness check — which
  compares modification times — reported a successful rebuild as stale after almost any edit that
  didn't change a class name. A warning that fires when nothing is wrong is worse than no warning,
  because it teaches you to ignore the one time it's right. The build now stamps the stylesheet on
  success. Contributor-facing only; nothing about the app changes.
- **The capture backend is now named in `Cargo.toml` instead of inherited**, with a test that fails
  if it is ever left to a default again. The dependency declares no default, so a plain version
  requirement silently selected the older of its two implementations — no warning, no error, no
  failing test, which is how it survived thirteen review passes. The test reads the manifest as
  text, so it works from any machine rather than needing Windows and a game.
- Screen captures are JPEG rather than PNG. Two crates added; the PNG stack stays regardless,
  because the capture library pins that feature itself — verified with `cargo tree` after
  predicting the opposite and being wrong. Raising PNG's compression instead was measured at
  **1,477 ms per frame** against 44.7 ms today, and rejected.
- **Curfew's two halves are one decision again.** The part that schedules the shutdown and the part
  that gives your child the heads-up were separate machines, joined only by the loop that called
  them — so rules about how they interact ("don't promise bedtime is coming while it is already
  happening") had nowhere to live and no way to be tested. They now come out of a single call, and
  the tests drive the real thing rather than a stand-in. Nothing behaves differently; what changed
  is that it can no longer *start* behaving differently by accident.
- **The stylesheet shrank by 15%, and the reason was not what two earlier passes assumed.** Both had
  hunted individual words in comments, because a comment mentioning a component ships that component.
  That was real but tiny. The actual cause: Tailwind's `@source` setting does not *replace* its
  automatic file detection, it adds to it — so the build had been scanning the whole tooling
  directory, including its own configuration and test files, since the beginning. Turning automatic
  detection off took the stylesheet from 102,181 to 86,736 bytes.
  The comment problem is fixed structurally too: the build now scans comment-free copies of the two
  pages rather than the pages themselves, so prose can no longer ship anything. Two things were
  learned by measuring rather than reading. The first build after that change made the stylesheet
  *grow*, because the new script's own documentation lists the component names it exists to keep out
  — a file explaining the trap was springing it. And pointed at the bundled Alpine library, the
  comment stripper removed 13,543 bytes from a file containing no comments, because minified code is
  full of slashes that are not comment markers. Bundled files are now left alone, and the script
  refuses to continue if any file loses more than half its bytes.
- **The enforcement check stopped gathering four things it immediately threw away.** Every thirty
  seconds, forever, it asked Windows for the CPU share, disk-I/O counters, memory use and full
  executable path of every process on the machine — several hundred of them — and read two fields.
  It now asks for the two. The dashboard's process panel, which genuinely needs the memory figure,
  keeps its own richer call; the two are separate types now so the cheap path cannot quietly be
  asked for the expensive number again. **Windows-only code that has never run** — it compiles clean
  for the target and is covered by tests through the fake, which is worth exactly what that is
  worth.
- **The screen-time report stopped reading the entire usage history to find thirty rows.** It parsed
  every line ever written to `usage.jsonl` — session starts, locks, warnings, grants — and then kept
  the daily summaries, which are one line a day. The cost grew with how long Nestwatch had been
  installed rather than with the window asked for. Lines that cannot match are now rejected before
  they are parsed, with the real check still done on the ones that survive, so a routine named after
  an event cannot sneak into the report.
- **A test now pins which pages work without signing in.** The handlers for you and the handlers for
  your child live in one file, and what separates them is which of two routers they are registered
  on, in a different file. Both look identical where they are written, so putting one in the wrong
  place is a single-line mistake with nothing local to catch it — and one direction of that mistake
  hands your child a control meant for you. Adding a route to the unauthenticated set now fails the
  build unless it is listed deliberately, with a note saying why it needs no password.
- **The stylesheet grew by 1.5 KB from three words in code comments, and shrank again.** The same
  defect as the `steps` one below: `tab`, `list` and `step` are all names of interface components,
  and prose containing them ships the component. This is the second time in one day, which says the
  hazard is structural rather than careless — the scanner reads comments as candidate class names,
  and no amount of care makes English avoid a vocabulary that includes "list".
- **The stylesheet shrank by 2.4 KB because two code comments stopped using the word "steps".**
  Tailwind finds class names by scanning the source files as raw text, so it does not distinguish a
  class from English prose — and daisyUI ships a `steps` component. A comment reading "Width steps
  up with the screen" was therefore emitting that entire component into every build, for a widget
  the product does not have. Measured by stripping every comment and rebuilding: prose was
  responsible for ~2.4 KB, and is now responsible for 21 bytes. Worth knowing before writing the
  next comment, and worth knowing that it cuts both ways — a class named only inside `:class` in
  JavaScript is found for the same reason, which is why `@source` scans `.js` too.
- One `accrue_capped(map, data, cap)` replaces `accrue` + `retain_top` at four call sites, and both
  halves are now private. The module's own doc had argued that `accrue` was kept separate from
  `clamp` "so the bound cannot be skipped" — while leaving the *count* bound skippable, which is
  exactly how the persisted tally ended up as the one map without it. Outside the module there is
  now no way to fold data in without bounding it, and the compiler says so. One mutation to that
  single function now fails all three flood tests; it previously took breaking three call sites.
- Two markup guards, both for defects with no symptom, joining the three that already scan these
  pages. `every_class_in_the_markup_has_a_rule_in_the_shipped_css` compares every static `class`
  against the compiled stylesheet — it reports all 69 dead references when run against the markup
  as it was, and nothing against the markup as it is. It also fails on a stale `assets/app.css`,
  which `build.rs` could previously only warn about, and a warning that scrolls past is
  indistinguishable from none. Two things it deliberately gets right, both of which produced false
  findings first: it reads only static `class="…"`, because Alpine's `:class` holds JavaScript and
  scanning it reports `===` and `null` as missing classes; and it undoes CSS escaping first,
  because Tailwind writes `2xl:max-w-[110rem]` as `.\32 xl\:max-w-\[110rem\]`.
  `every_form_control_can_be_named_by_a_screen_reader` counts a wrapping `<label>` as a name, so
  the per-weekday budget boxes pass as they stand. The class scan reads both quote styles and has
  its own test pinning what it picks up: a scanner that quietly covers less than it claims is a
  worse defect than the one it was written to catch, because it looks like coverage.
- `the_read_limit_clears_the_largest_honest_line` pins `foreground::MAX_LINE` against the biggest
  line a well-behaved watcher can emit — 170,170 bytes at the worst case for JSON escaping. The
  three numbers that decide it (`MAX_PAGES`, the title and process-name buffers in `watcher.rs`,
  the 30-second emit cadence) live in three files and are edited independently, and a limit set
  below that figure discards real samples as if forged. 64 KiB, the obvious round number, is under
  it by a factor of three.
- Lint policy moved from CI's command line into `Cargo.toml`, so `cargo clippy` and the editor
  enforce what CI enforces. Turning on `undocumented_unsafe_blocks` is what found the four above —
  it is allow-by-default, so `-D warnings` had never switched it on.
- The dashboard's 744-line script and the child page's 136-line script moved out of the markup
  into `assets/app.js` and `assets/ask.js`. Beyond the policy change above, this is what made it
  possible to test them at all. Three source scans guard the shapes that would silently undo it:
  no inline script, no `<template>` inside `<svg>`, and `scope` on every column header.
- **Tests for the dashboard's own logic, where there were none** — 81 of them as this is written:
  the version comparison, the
  enforcement-staleness check, the chart's bar heights, the shared day formatting, and the
  "any limits set" check. They run on `node:test`, which ships with Node, so nothing was added to
  the project's dependencies; `npm test` in `web/` runs them, and CI runs them on both Linux and
  Windows. The first run is what found the staleness bug above.

## [0.2.3] — 2026-08-24

### Added
- **Install offers to fix what it finds.** The checks added in 0.2.2 reported problems and left
  you to run the commands. Three of them it can now do itself, if you say yes: setting the
  network to Private, unblocking a file Windows marked as downloaded, and re-enabling a service
  left disabled. Asked one at a time, defaulting to **no**, since these change the machine's
  settings. `install --fix` answers yes in advance for an install with nobody at the console.
- **The dashboard shows which version is on that PC**, with a button to check whether it is the
  latest. The check runs in *your* browser, on the device you are reading the dashboard on —
  the monitored PC still contacts nothing, and nothing is checked unless you press it.
- **`nestwatch remote-setup`** prints a script that turns on remote administration properly, so
  you can install a new build over the network instead of walking to the PC. It generates the
  whole thing with this machine's name filled in — the usual step-by-step advice is dangerous to
  follow halfway, because the first command opens an unencrypted way in that later ones close.
  `--off` prints the teardown. See the new
  [docs/REMOTE-UPDATE.md](docs/REMOTE-UPDATE.md), which also explains why there is no auto-updater.
- **`doctor` now notices remote administration.** Unencrypted remote management is reported as a
  failure — on a home network anyone can capture the sign-in exchange and crack it later — and
  encrypted remote management as a reminder that you left a way in open.

### Improved
- **The dashboard uses the width of a large screen.** It stopped at 1024px, so on a 1920 monitor
  it used barely half. Now 1280 at large sizes and up to 1760 on very wide ones — still bounded,
  because a table stretched across a 4K display is harder to read, not easier.
- **The screenshot opens full-window.** It was capped at a size where you could see something was
  on screen but not read it. Click the picture or press Expand; Escape closes it. There is a
  Fullscreen button for the whole monitor, and live refresh keeps working while it is open.
- Warnings that tell you to change a Windows setting now give the command, not a path through
  Settings — you are usually already at a prompt that can do it in one line.

### Fixed
- **A first-time install could fail even though the service had started.** The installer asked
  Windows for permission to start and delete the service but not to *read its status*, so every
  check came back refused and it concluded the service never started — then deleted it. Only
  fresh installs were affected; upgrades worked, which is why it went unnoticed.
- **Installing over a running Nestwatch was refused.** The pre-flight port check added in 0.2.2
  saw the port in use — by the copy already running — and reported it as a conflict that stops
  the install. Fresh installs were fine, which is why it went unnoticed; every *upgrade* was
  blocked, including the remote one this release documents. It now recognises its own running
  service, and only when the port matches: a service on 8443 no longer excuses something else
  holding the 9000 an `install --port 9000` asked for.
- **A refused install could claim nothing had changed when something had.** Accept an offered fix,
  then fail on a different blocker, and the report still signed off with "Nothing has been
  changed on this machine." — printed immediately after changing the machine. It now says so.
- **Install error text printed with large gaps mid-sentence** ("the registered path may⎵⎵⎵⎵⎵⎵be
  wrong") — line continuations had been removed without collapsing the indentation.
- `nestwatch help` listed the `install` flags out of alignment and omitted `--reset-config`,
  which the README documents.
- **A mistyped option was ignored instead of refused, and one of them inverted the command.**
  `remote-setup --of > teardown.ps1` wrote the script that *enables* remote administration into a
  file named teardown — which the next step tells you to run, elevated. `install --prot 9000`
  installed on the default port and said nothing. Unrecognised options are now refused, naming
  the option and listing what the command does accept.
- **The remote-setup script's firewall step could look hung.** It selected the plaintext-WinRM
  rules by piping every firewall rule through a port lookup — one query per rule, hundreds on a
  stock Windows install. That matters more than the seconds: it is step 4 of 6 in a script that
  must not be interrupted, since step 1 opens the unencrypted listener step 3 closes, so the step
  most likely to be cancelled was the one whose cancellation does the most harm. It now queries
  the port filters directly, which is Microsoft's documented way to select rules by port. The
  script also **verifies the firewall result** now, not just the listeners, and refuses to finish
  while any inbound rule still admits 5985.

### Internal
- **Pre-flight now warns when the tools that enforce bedtime are missing.** It checked the four
  that `install` itself needs and none of the two the curfew needs — so `shutdown.exe` or
  `rundll32.exe` missing from a stripped Windows image meant a clean install, a working dashboard,
  and nothing happening at bedtime. A caution rather than a blocker: the install is genuinely
  fine. A test derives the list from the call sites, the way `tests/spawn_paths.rs` already does,
  because the hand-written list is what fell behind.
- `Finding` carries `Option<Remedy>` rather than a `Remedy::Manual` variant, so every value of
  that type is something the installer can actually perform. `apply` no longer has an unreachable
  arm returning an empty success.
- `tool_output` moved from `preflight` to `syspath`. The installer's *mutation* path depended on
  the pre-check module purely to format a subprocess error; `syspath` already owns how Windows
  tools are invoked.
- `cargo deny` now refuses HTTP-client crates (`reqwest`, `ureq`, `curl` and friends). "Nothing
  leaves the house" was stated in `SECURITY.md` and enforced by nothing: the dashboard's CSP is
  checked by a test, but a CSP constrains the browser, not the service. Adding one outbound call
  to `src/` would have kept every gate green. The `[bans]` policy was empty while CI was already
  running `cargo deny check bans`.
- Three more facts that were stated in two places now have something holding them together: the
  page's external URLs against the CSP allowlist, the installed binary's name against the paths
  the docs spell out, and the accepted-options table against the code that reads the flags.
- The guide and the generated remote-setup script are pinned to each other by a test. They had
  drifted: the guide named a firewall rule and a certificate file that the script does not
  create, so setting up with the script and tearing down with the guide would have left the
  encrypted-remoting port open.

## [0.2.2] — 2026-08-24

### Added
- **Install checks everything before it changes anything.** It used to find problems as it hit
  them, part-way through registering a service and overwriting files — so a machine with three
  problems cost three separate attempts, each ending somewhere different. Now every precondition
  is checked first, together, and *before* the password prompt:
  - the port is free (otherwise the service starts, can't bind, and exits within a second)
  - the Windows tools it needs are present
  - no leftover service is sitting disabled or still being deleted
  - the file isn't still marked as downloaded-from-the-internet
  - the network is Private, not Public

  Anything that would stop the install is reported **before a single change is made**, so it can
  say — truthfully — that nothing on the machine was touched. Anything that only *affects* the
  result is reported and the install continues.

### Fixed
- **The most common reason the dashboard "doesn't load" is now caught at install.** The firewall
  rule only applies on Private and Domain networks. On a Public one Windows blocks every
  incoming connection, so the address and QR code time out from every device — while the install
  reports success and the service runs perfectly. Previously this was a reminder printed on
  every install, next to an unrelated one, whether or not it applied.
- **A first install no longer prints two alarming errors about settings that were applied.** The
  restart-on-failure configuration ran once before the service existed (failing with "the
  specified service does not exist") and again afterwards, where it quietly worked.

## [0.2.1] — 2026-08-24

### Fixed
- **A first-time install could destroy a service that had started correctly.** The installer
  registers the service, starts it, then watches until it reports running. It was asking Windows
  for permission to start and delete the service but not to *read its status* — so every check
  came back "refused", the installer concluded it had never started, and deleted it. Upgrades
  were unaffected, which is why this went unnoticed: only a fresh install could hit it. If your
  install failed with *"the service did not reach a running state"*, this was almost certainly
  why, and the service was probably running at the time.

### Improved
- **Failures now say what Windows said.** Errors from the service manager were reported as "IO
  error in winapi call" regardless of the actual problem, discarding the error code that names
  it. Every failure now reports the code, what it means, and what to do — including the common
  ones: a leftover service still being deleted, a service left disabled by a half-finished
  removal, and permission refusals.
- **The install prints its own progress, not other programs'.** Lines like `processed file:`,
  `Successfully processed 1 files` and `Deleted 1 rule(s)` came from the Windows tools the
  installer calls and are now hidden unless something fails, where they explain it. One of them
  was worse than noise: a `[SC] ... FAILED` line that the installer printed and then ignored.
  If that step fails it now says so, and what it costs — the service still installs and runs,
  it just won't restart itself automatically.
- The install banner names the version: `== nestwatch v0.2.1 :: install ==`.

## [0.2.0] — 2026-08-24

### Changed
- **The password minimum is 8 characters, down from 10** — and there are still no rules about
  mixing letters, digits and symbols. Current guidance (NIST SP 800-63B Rev 4) says requiring
  those makes passwords worse, not better, so instead the obvious guesses are refused:
  `12345678`, `password123`, one character repeated, a straight run, a short pattern repeated.
  An all-digit password is fine if it isn't one of those. `docs/SECURITY.md` explains the
  reasoning, including where this departs from the standard and why.

### Fixed
- **Two settings saved at the same moment could corrupt `config.json`.** Every writer shared one
  temp file, so two overlapping saves interleaved into it and the result was published over the
  real config. A corrupt config stops the service from starting, which locks the parent out until
  a reinstall — the worst thing this file can do. Each save now writes to its own temp file, and
  the mutate-and-persist pair is serialized, so a save can no longer land an older snapshot on
  top of a newer one and silently revert a setting at the next restart. Reachable from ordinary
  use: approving a time request while a rules change is still saving.
- The certificate and its key are written the same way as everything else in the data folder, so
  an interrupted write cannot leave a half-cert whose fingerprint no longer matches the one
  printed at install.
- **Install no longer gives up on a service that is merely slow to start.** It waited 6 seconds,
  which is less than Windows Defender can spend scanning a newly written program the first time
  it runs — so a service that was about to come up fine was rolled back. It now waits 30 seconds,
  the same as Windows itself.
- **A failed install now tells you what happened.** "The service did not reach a running state"
  named no cause and suggested nothing. It now reports what the service was last seen doing,
  which separates *never started* from *started and stopped* — different problems that had the
  same message — names the likely causes in order, and points at the log and Event Viewer. It
  also says plainly that nothing was left behind, so it is safe to fix and try again.
- **A typo in the password confirmation no longer aborts the whole install.** It asks again.

### Improved
- **Every password rejection now says what was actually wrong.** Too short reports the number of
  characters it counted, so "it says 8 but I typed 10" is answerable instead of an argument. A
  mismatch says whether the two entries differ in length and by how much, without showing either.
  A leading or trailing space is pointed out rather than silently accepted or silently removed.
  The dashboard shows the same explanations as the installer instead of its own guess.

### Security
- The dependency license and duplicate-version policies are now enforced on every push. They were
  written but never run, so nothing checked that the dependency tree stayed compatible with the
  MIT license the project ships under.
- Released binaries carry a **signed build-provenance attestation**. The published checksum only
  proves a download wasn't corrupted; the attestation proves the binary came from this
  repository's release workflow, and is checkable with
  `gh attestation verify nestwatch.exe --repo emrecdr/nestwatch`.
- Dependency and workflow updates are proposed automatically, so a pinned action can no longer sit
  quietly on a version with a known advisory.

## [0.1.0] — 2026-08-19

First release of the current codebase. The project was developed privately before this point;
the history was reset and versioning restarted, so this entry describes what the software does
rather than what changed.

### Screen-time limits
- **Daily budget** in minutes, optionally different per weekday, counting only *active* use — not
  idle, locked or logged-out time. Survives reboots and resets at midnight.
- **When the budget is spent:** lock, shut down, or warn only.
- **Countdown warnings** to the child at 15, 5 and 1 minutes, for both the budget and bedtime.
  A budget shorter than a threshold never announces it, a mid-day restart does not replay warnings
  already passed, and granting extra time re-arms them.
- **Curfew** — one or more time windows per weekday, with the same countdown, then shutdown; a
  cancelled shutdown is re-issued rather than offering another countdown to cancel.
- **Resists clock tampering.** Changing the PC's time zone — which Windows lets a standard user do
  with no prompt — cannot reset the day's tally or move the curfew window. Real daylight-saving
  changes are still followed.

### Visibility
- **Today's usage** — minutes used and remaining, with per-app and per-group bars.
- **Screen-time report** — the last 30 days as a chart, with per-app minutes for apps that have a
  limit, plus a comparison against the previous period. Days the service was not running show as
  **not measured** rather than zero, so a stopped enforcer cannot be mistaken for a quiet week.
  The figures count time the PC was unlocked with an app *running* — not focused attention, and
  not per-account — which the card states, because that makes them different from a phone's.
- **Usage history** and an **access log** of logins with their source address.
- **`nestwatch doctor`** — is the service up, the port listening, the firewall rule right, the
  network private, the certificate valid, and is anything actually being enforced. Every problem
  prints its fix. The report leads with the build version.

### Remote control
- Screenshot the desktop (with optional live refresh), list and kill running apps, lock the
  screen, or shut down with a warned countdown — from any device on the same home network.

### Asking for more time
- The child's own page shows the time left and can request more; the parent approves or denies.
- Single-use offline codes cover times the parent is away or the network is down.

### App rules
- Blocklist, per-app daily limits, and groups sharing one pool. Deliberately documented as
  habit-shaping rather than a wall: matching is by filename, so a renamed copy escapes them. The
  budget and curfew are the controls that hold.

### Security
- **LAN-only** — a Windows firewall rule *and* an application-level check, two independent gates.
- **HTTPS** with a self-signed certificate whose fingerprint is printed at install and reprintable
  on demand, so a new device can be verified.
- **Argon2id** password hashing with per-IP throttling that a stranger cannot use to lock the
  parent out; sessions survive reboots.
- **Origin-checked requests** — a login cookie alone cannot distinguish this dashboard from a page
  served on another port of the same PC, so every request is checked against the browser's own
  report of where it came from. Links, bookmarks and the pairing QR still work.
- **Windows system tools are called by absolute path**, never by bare name, so a look-alike file
  beside the executable cannot be run with administrator rights.
- **Data directory restricted** to the system and administrators: the password hash, TLS key and
  every log are unreadable and undeletable by a standard user.
- **Tamper-resistant service** a standard user cannot stop, with automatic restart configured.

### Known limits
Recorded openly in [docs/OPEN-FINDINGS.md](docs/OPEN-FINDINGS.md) and on the project page, because
a tool like this is worth less if it overstates itself: app rules lose to a rename, time is counted
while an app runs rather than while it is watched, totals are per-machine rather than per-account,
a wedged enforcer is reported but not yet restarted automatically, and a local administrator on the
PC can defeat all of it by design.
