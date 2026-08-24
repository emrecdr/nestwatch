# Changelog

All notable changes to Nestwatch. Dates are the release-tag dates.

## [Unreleased]

### Improved
- **The dashboard is readable with a screen reader.** Its six tables had no column headers as far
  as assistive software was concerned — the header row was styled, not labelled, so a figure was
  read out without the column it belonged to. And the two panels that refresh by themselves every
  minute, today's usage and the more-time requests, changed silently: you were told the numbers
  once, on load, and never again. Both fixed, and a test now refuses a seventh table that repeats
  the first mistake.

### Fixed
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
- **The dashboard can no longer run a script that arrives in its markup.** Both pages kept their
  JavaScript in the page itself, which meant the policy had to permit inline scripts in general —
  and that permission is what an injected `<script>` would have needed. The code now lives in two
  ordinary files, so the permission is gone. Nothing about the page changes for you; this closes
  the gap between "we don't do that" and "that can't happen".

### Internal
- Lint policy moved from CI's command line into `Cargo.toml`, so `cargo clippy` and the editor
  enforce what CI enforces. Turning on `undocumented_unsafe_blocks` is what found the four above —
  it is allow-by-default, so `-D warnings` had never switched it on.
- The dashboard's 744-line script and the child page's 136-line script moved out of the markup
  into `assets/app.js` and `assets/ask.js`. Beyond the policy change above, this is what made it
  possible to test them at all. Three source scans guard the shapes that would silently undo it:
  no inline script, no `<template>` inside `<svg>`, and `scope` on every column header.
- **21 tests for the dashboard's own logic**, where there were none — the version comparison, the
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
