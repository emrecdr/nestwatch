# Changelog

All notable changes to Nestwatch. Dates are the release-tag dates.

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
