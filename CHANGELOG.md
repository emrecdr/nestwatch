# Changelog

All notable changes to Nestwatch. Dates are the release-tag dates.

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
