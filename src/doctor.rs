//! `nestwatch doctor` — one screen that answers "is this actually working?".
//!
//! Before this existed, the only way to check an install was `docs/WINDOWS-TESTING.md`: ~50
//! manual checkboxes across `sc query`, `sc qc`, `netsh`, `dir` and `type`. That's a reasonable
//! acceptance test to run once; it is *not* a reasonable answer to "something seems off".
//!
//! Every check here is cheap, read-only, and reports a **fix** alongside any problem — the point
//! is to be actionable by a parent, not to produce a compliance report. Nothing here can change
//! the system.
//!
//! Deliberately *not* checked: the certificate's SANs (would need an X.509 parser for a single
//! diagnostic line).
//!
//! Note the enforcer-liveness check only means anything when run **against a live service** — a
//! one-shot CLI process has its own (never-beating) heartbeat cells, so it reports "not running"
//! from a separate process by construction. It is reported as a warning, not a failure, for that
//! reason; the dashboard banner is the reliable signal.

use std::fmt::Write as _;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use anyhow::Result;

use crate::config::{self, Config};

/// The remediation a parent is told most often; one copy so the wording can't drift, and so a
/// change can't land in only the two non-Windows copies.
const RUN_INSTALL: &str = "Run `nestwatch install` from an elevated console.";

/// How long to wait when probing the local listener.
const PROBE_TIMEOUT: Duration = Duration::from_millis(1500);

/// Whether anything is accepting connections on this port, on this machine.
///
/// A TCP connect rather than parsing a tool's output: `winrm enumerate` and firewall rule names
/// are both localised, and a successful connect is the same evidence on every Windows language.
/// Callers that need the *reason* a connect failed keep their own `match` — this is for the ones
/// that only need the yes/no.
#[cfg(windows)]
fn listening(port: u16) -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    TcpStream::connect_timeout(&addr, PROBE_TIMEOUT).is_ok()
}

// `Debug` so a failing assertion names the level it got instead of printing nothing useful.
#[derive(Debug, PartialEq, Clone, Copy)]
enum Level {
    Ok,
    Warn,
    Fail,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Ok => "[ok]  ",
            Level::Warn => "[warn]",
            Level::Fail => "[FAIL]",
        }
    }
}

struct Check {
    level: Level,
    text: String,
    /// What to actually do about it. Shown indented under the check.
    fix: Option<String>,
}

#[derive(Default)]
struct Report {
    sections: Vec<(String, Vec<Check>)>,
}

impl Report {
    fn section(&mut self, title: &str) -> &mut Vec<Check> {
        self.sections.push((title.to_string(), Vec::new()));
        &mut self.sections.last_mut().expect("just pushed").1
    }

    fn counts(&self) -> (usize, usize) {
        let all = self.sections.iter().flat_map(|(_, c)| c);
        (
            all.clone().filter(|c| c.level == Level::Warn).count(),
            all.filter(|c| c.level == Level::Fail).count(),
        )
    }

    fn render(&self) -> String {
        // The version leads the report on purpose: `doctor` is what you run when something is
        // wrong on a machine you visit rarely, and "which build is this?" is the first question —
        // including whether a given security fix is present at all.
        let mut out = format!("\nnestwatch doctor — v{}\n", crate::VERSION);
        for (title, checks) in &self.sections {
            let _ = write!(out, "\n  {title}\n");
            for c in checks {
                let _ = writeln!(out, "    {} {}", c.level.tag(), c.text);
                if let Some(fix) = &c.fix {
                    for line in fix.lines() {
                        let _ = writeln!(out, "           {line}");
                    }
                }
            }
        }
        let (warns, fails) = self.counts();
        let summary = match (warns, fails) {
            (0, 0) => "\nAll good.\n".to_string(),
            (w, 0) => format!("\n{w} warning(s).\n"),
            (0, f) => format!("\n{f} problem(s).\n"),
            (w, f) => format!("\n{f} problem(s), {w} warning(s).\n"),
        };
        out.push_str(&summary);
        out
    }
}

fn ok(text: impl Into<String>) -> Check {
    Check {
        level: Level::Ok,
        text: text.into(),
        fix: None,
    }
}

fn warn(text: impl Into<String>, fix: impl Into<String>) -> Check {
    Check {
        level: Level::Warn,
        text: text.into(),
        fix: Some(fix.into()),
    }
}

/// What `doctor` should say about the trusted clock, given what was recorded and what the machine
/// reports now.
///
/// Pure and taking both readings as arguments, for the same reason `clock::decide` is: the zone
/// identity only exists on Windows, so a function that read it itself could not be tested anywhere
/// this suite runs.
///
/// This section exists because nothing reported on the tamper defence at all. `doctor` is what a
/// parent runs to find out whether the controls are actually working, and the clock is what stops
/// the two controls that hold — the budget reset and the curfew window — from being moved by a
/// child with no administrator rights and no prompt. A defence nobody can see the state of is one
/// nobody can tell has stopped applying.
fn clock_check(recorded_zone: Option<&str>, current_zone: Option<&str>, anchored: bool) -> Check {
    const REINSTALL: &str = "Re-run `nestwatch install` from an elevated console. It re-records \
both,\nand preserves your port, curfew and rules.";

    match (recorded_zone, current_zone) {
        (Some(recorded), Some(current)) if recorded == current => ok(format!(
            "clock anchored to this machine's time zone ({recorded})"
        )),
        // The one that matters. Either the PC genuinely moved, or someone changed the zone —
        // and the service is currently ignoring the OS clock because of it, which is correct but
        // is not a state to leave running silently.
        (Some(recorded), Some(current)) => fail(
            format!(
                "time zone changed since install — recorded {recorded}, now {current}. \
                 Screen-time and curfew are using the recorded zone, not this one."
            ),
            "If the PC genuinely moved, re-anchor it: sign in to the dashboard and use\n             `Re-anchor the clock`, or re-run `nestwatch install`. If it did not move,\n             someone changed the time zone — the controls held, and that is what this says.",
        ),
        // Windows could not tell us, or this is a dev build. Not an error; just not the strong check.
        (Some(recorded), None) if anchored => ok(format!(
            "clock anchored (zone {recorded} recorded; this platform cannot read the current one)"
        )),
        (None, _) if anchored => warn(
            "clock anchored by offset only — no time zone recorded",
            format!(
                "This install predates the zone check, so it uses the weaker one: an offset \
                 alone\ncannot tell two zones apart, and a child can shift the clock up to two \
                 hours in\nsummer by choosing one that shares the offset.\n{REINSTALL}"
            ),
        ),
        (None, _) => warn(
            "clock NOT anchored — plain local time is in use",
            format!(
                "Changing the time zone needs no administrator rights, so the daily budget \
                 and the\ncurfew window can both be moved by the child until this is \
                 recorded.\n{REINSTALL}"
            ),
        ),
        (Some(_), None) => warn(
            "clock anchored, but the current time zone could not be read",
            "Non-fatal: the service falls back to the offset check, which is what it always\n             used. Worth a look if this is Windows — it means a system call is failing.",
        ),
    }
}

fn fail(text: impl Into<String>, fix: impl Into<String>) -> Check {
    Check {
        level: Level::Fail,
        text: text.into(),
        fix: Some(fix.into()),
    }
}

/// What `doctor` should say about this machine's ability to capture the screen at all.
///
/// **Three states, never two.** A supported build reports as checked, an unreadable build says it
/// could not be read, and an old build warns. Returning nothing on success would leave the report
/// with no "Screen capture" section, and an absent section reads exactly like a check that never
/// ran — the distinction this product spends real effort keeping everywhere else (`measured` on
/// `DayRow`, `focus_missing`, `Stamp::Missing` against `Stamp::Corrupt`). A parent whose
/// screenshots fail on a *modern* machine needs to see this was checked and was not the cause.
///
/// The unreadable case does **not** warn, matching [`crate::preflight::check_windows_build`]: a
/// misbehaving syscall must not alarm a parent whose machine is almost certainly fine. But it does
/// not claim support either, because that would report an unmeasured thing as measured.
///
/// Pure, and takes the build rather than reading it, for the same reason [`version_check`] does:
/// the decision is then testable on the machine this is developed on, which is not the machine it
/// runs on. The floor itself stays in [`crate::preflight::capture_build_ok`] — one definition.
///
/// Its only non-test caller is inside `#[cfg(windows)] platform_checks`, so off Windows it is dead
/// by definition, and keeping it compiled here is the point: the decision stays testable where the
/// tests run. Same shape as the helpers in `install.rs`.
#[cfg_attr(not(windows), allow(dead_code))]
fn capture_check(build: u32) -> Check {
    if build == 0 {
        return ok(
            "could not read this Windows build number — assuming it is new enough for screen \
             capture, which is what install assumes too",
        );
    }
    if crate::preflight::capture_build_ok(build) {
        return ok(format!("Windows build {build} supports screen capture"));
    }
    warn(
        format!(
            "Windows build {build} is older than {} (version {}) — screenshots and the live \
             view cannot work on this machine",
            crate::preflight::MIN_CAPTURE_BUILD,
            crate::preflight::MIN_CAPTURE_VERSION
        ),
        "Everything else works normally on this build: screen-time limits, curfew, blocked apps \
         and the whole enforcement half are unaffected. Only the picture of the screen fails. \
         Run Windows Update — any Windows 10 still receiving updates is well past it.",
    )
}

/// Compare the build that is *installed* against the build running this check.
///
/// They are routinely different and nothing else on the machine says so. Copying a new binary onto
/// the PC is not installing it, so a parent who downloads an update and runs `doctor` from the
/// download directory gets a clean bill of health about a service still running the old code —
/// which is the report they will trust when deciding whether a fix is present.
///
/// `installed` is whether there is an install for a missing record to describe; on a machine with
/// no config, "no version record" would only repeat "not installed" one line further down.
///
/// The ordering rule ("0.10 is above 0.2") is [`crate::install::classify_install`]'s, reused rather
/// than restated so there is one definition and one set of tests for it.
fn version_check(stamped: &crate::install::Stamp, running: &str, installed: bool) -> Option<Check> {
    use crate::install::InstallKind;

    Some(match crate::install::classify_install(stamped, running) {
        InstallKind::Reinstall => ok(format!("installed version {running} matches this binary")),
        InstallKind::Upgrade { from } => warn(
            format!("this binary is {running} but the installed service is {from}"),
            "Copying the binary onto the machine does not update the service. Run\n\
                 `nestwatch install` from an elevated console to apply it.",
        ),
        InstallKind::Downgrade { from } => warn(
            format!("this binary is {running} — older than the installed service ({from})"),
            "You are probably running an old copy. Check which binary you launched\n\
                 before acting on anything else in this report.",
        ),
        InstallKind::Unknown => warn(
            "the installed-version record is unreadable",
            "Harmless on its own — re-running `nestwatch install` rewrites it.",
        ),
        InstallKind::Fresh if installed => warn(
            "no installed-version record",
            "Expected if this was installed by a build older than the record itself.\n\
                 Re-running `nestwatch install` writes one.",
        ),
        InstallKind::Fresh => return None,
    })
}

/// Run every check and print the report. Exits non-zero if anything is outright broken, so it's
/// usable from a script; warnings alone still exit 0.
pub fn run() -> Result<()> {
    let mut report = Report::default();
    let paths = config::data_paths();

    // The data dir is locked to SYSTEM + Administrators, so without elevation `Config::load` and
    // every `Path::exists()` on a file inside it fail with access-denied — which is
    // indistinguishable from "absent" unless we check first. Reporting "not installed" for a
    // perfectly healthy install would be bad enough; the fix it printed was "run install", which
    // regenerates the certificate and invalidates every paired device. So: establish elevation
    // up front and downgrade anything we simply cannot see to a warning.
    let elevated = crate::install::is_elevated();

    // --- Configuration -----------------------------------------------------
    let config = Config::load().ok();
    let port = config
        .as_ref()
        .map(|c| c.port)
        .unwrap_or(config::DEFAULT_PORT);
    {
        let checks = report.section("Configuration");
        match &config {
            Some(_) => checks.push(ok(format!(
                "config.json readable, port {port} ({})",
                paths.dir.display()
            ))),
            None if !elevated => checks.push(warn(
                "can't read the settings from here (not an elevated console)",
                "This is expected — the data folder is locked to Administrators. Re-run from\n\
                 an elevated console for the full report. Nothing below that depends on\n\
                 reading it could be checked.",
            )),
            None if paths.config.exists() => checks.push(fail(
                format!("config.json at {} is unreadable", paths.config.display()),
                "It may be corrupt. Re-run `nestwatch install` from an elevated console —\n\
                 note this regenerates the certificate, so paired devices will warn once more.",
            )),
            None => checks.push(fail("not installed — no config found", RUN_INSTALL)),
        }

        // Only meaningful where the record could actually be read: the data dir is locked to
        // Administrators, so an unelevated run cannot tell "no record" from "cannot see it".
        if (elevated || config.is_some())
            && let Some(check) = version_check(
                &crate::install::read_stamp(),
                crate::VERSION,
                config.is_some(),
            )
        {
            checks.push(check);
        }
    }

    // --- Network -----------------------------------------------------------
    {
        let hosts = crate::cert::reachable_hosts();
        // --- Trusted clock -----------------------------------------------------
        {
            let checks = report.section("Trusted clock");
            checks.push(clock_check(
                config.as_ref().and_then(|c| c.tz_zone.as_deref()),
                crate::clock::current_zone_identity().as_deref(),
                config.as_ref().is_some_and(|c| c.tz_offset_mins.is_some()),
            ));
        }

        let checks = report.section("Network");
        match hosts.first() {
            Some(ip) => checks.push(ok(format!("reachable at https://{ip}:{port}"))),
            None => checks.push(warn(
                "couldn't detect a LAN address — this PC looks offline",
                "Connect it to the home Wi-Fi, then run this again.",
            )),
        }
        // Is anything actually listening? A TCP connect proves the service bound the port,
        // without needing an HTTP client or trusting the self-signed cert.
        let local = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        match TcpStream::connect_timeout(&local, PROBE_TIMEOUT) {
            Ok(_) => checks.push(ok(format!("something is listening on port {port}"))),
            Err(e) => checks.push(fail(
                format!("nothing is listening on port {port} ({e})"),
                "The service isn't running or failed to start. Check the newest\n\
                 service.<date>.log in the data folder (readable as Administrator).",
            )),
        }
        platform_network_checks(port, checks);
    }

    // --- Certificate -------------------------------------------------------
    {
        let checks = report.section("Certificate");
        if !elevated && !paths.cert.exists() {
            // Same access-denied-looks-like-missing trap as the config above.
            checks.push(warn(
                "can't inspect the certificate from here (not an elevated console)",
                "Re-run from an elevated console to check its expiry and fingerprint.",
            ));
        } else if !paths.cert.exists() {
            checks.push(fail("no TLS certificate", RUN_INSTALL));
        } else {
            match crate::cert::days_until_expiry(&paths.cert) {
                Some(0) => checks.push(fail(
                    "the certificate has expired",
                    "Run `nestwatch install` to issue a fresh one (the fingerprint changes,\n\
                     so you'll accept the browser warning once more).",
                )),
                Some(d) if d < crate::cert::RENEW_WARN_DAYS => checks.push(warn(
                    format!("expires in about {d} days"),
                    "Run `nestwatch install` to refresh it before it lapses.",
                )),
                Some(d) => checks.push(ok(format!("valid for about {d} more days"))),
                None => checks.push(warn(
                    "couldn't read the certificate's age",
                    "Run this from an elevated console — the data folder is locked to\n\
                     SYSTEM and Administrators.",
                )),
            }
            // The first and last groups are what a parent can realistically eyeball; comparing
            // all 32 is known not to work in practice.
            match crate::cert::read_fingerprint(&paths.cert) {
                Ok(fp) => {
                    let groups: Vec<&str> = fp.split(':').collect();
                    match (
                        groups.get(..4),
                        groups.get(groups.len().saturating_sub(4)..),
                    ) {
                        (Some(head), Some(tail)) if groups.len() == 32 => {
                            checks.push(ok(format!(
                                "fingerprint starts {} and ends {}",
                                head.join(":"),
                                tail.join(":")
                            )));
                        }
                        // Don't silently emit nothing — an unreadable fingerprint is a finding.
                        _ => checks.push(warn(
                            "the certificate's fingerprint looks malformed",
                            "Re-run `nestwatch install` from an elevated console to reissue it.",
                        )),
                    }
                }
                Err(e) => checks.push(warn(
                    format!("couldn't read the certificate's fingerprint ({e})"),
                    "Re-run `nestwatch install` from an elevated console to reissue it.",
                )),
            }
        }
    }

    // --- Enforcement -------------------------------------------------------
    {
        let checks = report.section("Enforcement");
        match &config {
            None => checks.push(warn(
                "can't tell — no config",
                "Install first, then run this again.",
            )),
            Some(cfg) => {
                let curfew_on = cfg.curfew.enabled;
                let rules_on = cfg.rules.enabled;
                // `has_targets`, not `any_configured` — the latter folds in `enabled`, which made
                // the "configured but paused" branch below unreachable and reported a fully
                // configured, paused install as "nothing is being enforced yet" (advising the
                // parent to add limits that already existed).
                let configured = cfg.rules.has_targets();

                if !curfew_on && !configured {
                    checks.push(warn(
                        "nothing is being enforced yet",
                        "Nestwatch is running and counting screen time, but no limits exist.\n\
                         Open the dashboard and set a daily limit, or turn on Curfew.",
                    ));
                } else {
                    if configured && rules_on {
                        checks.push(ok(format!(
                            "screen-time rules active (daily limit {} min, {} blocked app(s), \
                             {} per-app limit(s))",
                            cfg.rules.daily_budget_mins,
                            cfg.rules.blocklist.len(),
                            cfg.rules.app_limits.len()
                        )));
                    } else if configured && !rules_on {
                        checks.push(warn(
                            "screen-time rules are configured but PAUSED",
                            "Flip Enforcing back on in the dashboard when the break is over.",
                        ));
                    }
                    if curfew_on {
                        checks.push(ok("curfew is on"));
                    } else {
                        checks.push(warn(
                            "curfew is off",
                            "Set a bedtime window in the dashboard if you want one.",
                        ));
                    }
                }

                // Liveness: a dead enforcer looks exactly like an idle day everywhere else.
                match crate::heartbeat::worst_age_secs() {
                    Some(age) if age <= 150 => {
                        checks.push(ok(format!("enforcement checked in {age}s ago")))
                    }
                    Some(age) => checks.push(warn(
                        format!("enforcement last checked in {} min ago", age / 60),
                        "If the service is running, its background checks have stopped — limits\n\
                         and curfew are not being applied. Restart it:\n\
                         sc stop HostHealthService && sc start HostHealthService",
                    )),
                    None => checks.push(warn(
                        "no enforcement check-in seen from this process",
                        "Expected when running `doctor` as a one-off — the live service keeps its\n\
                         own heartbeat. Check the dashboard's Today card, which reads the running\n\
                         service's value.",
                    )),
                }

                if configured && cfg.rules.warn_secs == 0 {
                    checks.push(warn(
                        "the child gets no warning before enforcement fires",
                        "Set a warning of 30-60 seconds in the dashboard so a lock isn't a\n\
                         surprise mid-sentence.",
                    ));
                }
            }
        }
    }

    // --- Platform (Windows service, ACLs, accounts) ------------------------
    platform_checks(&mut report);

    print!("{}", report.render());
    let (_, fails) = report.counts();
    if fails > 0 {
        std::process::exit(1);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Windows-specific checks
// ---------------------------------------------------------------------------

/// The network category of every adapter, one entry each.
///
/// Shared with the installer, which needs the same answer for the same reason and must not reach
/// a different one — the same rule already applied to `firewall_rule_is_subnet_scoped`.
///
/// One line PER ADAPTER on purpose. Hyper-V, WSL, VirtualBox and VPN adapters routinely report
/// Public, so a substring test over the joined output cried wolf about a perfectly good Wi-Fi
/// connection. Empty means the query failed, which is not the same as "Public" and must not be
/// reported as it.
#[cfg(windows)]
pub(crate) fn network_profiles() -> Vec<String> {
    std::process::Command::new(crate::syspath::powershell())
        .args([
            "-NoProfile",
            "-Command",
            "Get-NetConnectionProfile | Select-Object -ExpandProperty NetworkCategory",
        ])
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// The command that switches every Public adapter to Private.
///
/// One copy, because this string is both **run** — by [`crate::preflight::apply`], when a parent
/// accepts the offered fix — and **printed** as advice, here and in pre-flight's finding. It had
/// become three copies, and this release showed exactly how they rot: doctor's advice gained the
/// command because pre-flight's already had it, updating two of the three. The third would then
/// quietly tell a parent to run something other than what `install --fix` does.
///
/// One line on purpose. Split across two with a trailing `|`, a parent who copies only the first
/// line leaves PowerShell sitting at a continuation prompt with nothing to say why.
#[cfg(windows)]
pub(crate) const MAKE_PRIVATE_PS: &str = "Get-NetConnectionProfile | Where-Object {$_.NetworkCategory -eq 'Public'} | \
     Set-NetConnectionProfile -NetworkCategory Private";

/// Whether every adapter Windows reported sits on a Public profile.
///
/// Takes the profiles instead of reading them, so a caller holding them already does not spawn a
/// second PowerShell. Owning the predicate here is the point: an empty list means the query
/// failed, which is *not* the same as Public, and that rule was previously re-derived at each of
/// the three call sites — one of which relied on an earlier branch to be correct at all.
#[cfg(windows)]
pub(crate) fn all_public(profiles: &[String]) -> bool {
    !profiles.is_empty() && profiles.iter().all(|p| p == "Public")
}

#[cfg(windows)]
fn platform_network_checks(port: u16, checks: &mut Vec<Check>) {
    // Remote administration, if it was ever turned on, and specifically whether it was left in
    // the dangerous shape. Port 5985 is WinRM's plaintext listener: on a workgroup network that
    // means NTLM over HTTP, whose exchange can be captured and cracked offline by anyone on the
    // LAN -- which here is the person being managed. Probed rather than parsed, because
    // `winrm enumerate` output and firewall rule names are both localised.
    //
    // Windows-only, and here rather than in the cross-platform body of `run` for that reason: on
    // a Linux or macOS dev box those ports mean nothing, and anything bound to 5986 would fail a
    // check whose remedy is a Windows command.
    if listening(5985) {
        checks.push(fail(
            "remote management is listening WITHOUT encryption (port 5985)",
            "Anyone on this network can capture the sign-in exchange and crack it\n\
             offline -- including the person using this PC.\n\
             Turn it off:            nestwatch remote-setup --off\n\
             Or set it up properly:  nestwatch remote-setup",
        ));
    }
    // HTTPS remoting is a deliberate choice, not a fault -- but it is an administrative way in,
    // and leaving it on between updates is the common mistake. Report it so it cannot be
    // forgotten about.
    if listening(5986) {
        checks.push(warn(
            "remote management is enabled (HTTPS, port 5986)",
            "Fine while you need it. Turn it off when you are done, so it is not a\n\
             permanent way in:  nestwatch remote-setup --off",
        ));
    }

    // The firewall rule is scoped to private+domain profiles; on a "Public" network it simply
    // never matches, which presents as "I can't connect from my phone" with no other symptom.
    // The per-adapter reading lives in `network_profiles`, shared with the installer's pre-flight.
    let profiles = network_profiles();

    if profiles.is_empty() {
        checks.push(warn(
            "couldn't read the network profile",
            "Check it by hand: Settings > Network & internet > (your Wi-Fi) > it must be\n\
             set to Private, or the firewall rule never matches.",
        ));
    } else if all_public(&profiles) {
        checks.push(warn(
            "this PC's network is set to Public",
            format!(
                "The firewall rule only applies on Private/Domain networks, so other devices\n\
                 can't reach the dashboard. Fix it from an elevated PowerShell:\n\
                 {MAKE_PRIVATE_PS}\n\
                 Or: Settings > Network & internet > (your Wi-Fi) > Network profile type."
            ),
        ));
    } else {
        checks.push(ok(format!(
            "network profile: {} ({} adapter(s))",
            profiles.join(", "),
            profiles.len()
        )));
    }

    // Shared with the installer's own read-back, so the two can't disagree about the rule's
    // name or what "correctly scoped" means.
    if crate::install::firewall_rule_is_subnet_scoped() {
        checks.push(ok(format!(
            "firewall rule '{}' present, scoped to the local subnet",
            crate::install::FIREWALL_RULE
        )));
    } else {
        checks.push(warn(
            format!(
                "the firewall rule '{}' is missing or not subnet-scoped",
                crate::install::FIREWALL_RULE
            ),
            format!(
                "Re-run `nestwatch install --port {port}` from an elevated console.\n\
                 (The app-layer LAN check still blocks off-network clients either way.)"
            ),
        ));
    }
}

#[cfg(not(windows))]
fn platform_network_checks(_port: u16, _checks: &mut Vec<Check>) {}

#[cfg(windows)]
fn platform_checks(report: &mut Report) {
    use std::ffi::OsStr;
    use std::process::Command;

    use windows_service::service::{ServiceAccess, ServiceState};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    use crate::service::SERVICE_NAME;

    // Whether this machine can capture at all, before anything about the service. A parent whose
    // screenshots never work is looking for exactly this line, and it is the one fact here that no
    // amount of reinstalling changes.
    report
        .section("Screen capture")
        .push(capture_check(crate::preflight::os_build()));

    {
        let checks = report.section("Service");
        let status = ServiceManager::local_computer(None::<&OsStr>, ServiceManagerAccess::CONNECT)
            .and_then(|m| m.open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS))
            .and_then(|s| s.query_status());
        match status {
            Ok(s) if s.current_state == ServiceState::Running => {
                checks.push(ok(format!("{SERVICE_NAME} is running")));
            }
            Ok(s) => checks.push(fail(
                format!(
                    "{SERVICE_NAME} is not running (state: {:?})",
                    s.current_state
                ),
                format!(
                    "Start it with `sc start {SERVICE_NAME}` from an elevated console, then\n\
                     check the newest service.<date>.log if it stops again."
                ),
            )),
            Err(_) => checks.push(fail(
                format!("{SERVICE_NAME} is not installed"),
                RUN_INSTALL,
            )),
        }
    }

    {
        let checks = report.section("Accounts");
        // Tamper-resistance is void if the child is a local administrator — and nothing else
        // in the product can detect that, so it's the most valuable check here.
        // Query by SID, not by the name "Administrators" — that name is localized
        // ("Administratoren", "Beheerders", …), so `net localgroup Administrators` fails outright
        // on a non-English Windows. `net`'s non-zero exit was also never checked, so the failure
        // fell into the success branch and printed an empty list under scary wording. The rest of
        // this file already uses well-known SIDs for exactly this reason.
        match Command::new(crate::syspath::powershell())
            .args([
                "-NoProfile",
                "-Command",
                "Get-LocalGroupMember -SID S-1-5-32-544 | Select-Object -ExpandProperty Name",
            ])
            .output()
        {
            Ok(out) if out.status.success() => {
                let text = String::from_utf8_lossy(&out.stdout);
                let admins: Vec<String> = text
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect();
                // Informational, not a warning: a healthy install would otherwise never be able
                // to print "All good." on Windows, which trains the parent to skim past warnings —
                // including the Public-network one that actually matters.
                checks.push(ok(format!(
                    "local administrators: {} (your child's account must NOT be listed)",
                    admins.join(", ")
                )));
            }
            _ => checks.push(warn(
                "couldn't list local administrators",
                "Check by hand — your child's account must not be an administrator, or they can\n\
                 stop the service and undo everything:\n\
                 Get-LocalGroupMember -SID S-1-5-32-544",
            )),
        }
    }
}

#[cfg(not(windows))]
fn platform_checks(report: &mut Report) {
    let checks = report.section("Platform");
    checks.push(ok(
        "running on a non-Windows host (dev mode) — no service, ACL or firewall checks",
    ));
}

#[cfg(test)]
mod tests {

    /// Every branch a parent can land on, because this section is read rather than run.
    ///
    /// The `Fail` case is the one that earns the section: a changed zone means the service is
    /// deliberately ignoring the OS clock, which is correct and is not a state to leave running
    /// unseen.
    mod clock {
        use super::super::{Level, clock_check};

        const HOME: &str = "W. Europe Standard Time";

        #[test]
        fn a_matching_zone_is_reported_ok_and_names_it() {
            let c = clock_check(Some(HOME), Some(HOME), true);
            assert_eq!(c.level, Level::Ok);
            assert!(c.text.contains(HOME), "name the zone: {}", c.text);
        }

        #[test]
        fn a_changed_zone_fails_and_says_which_way_it_moved() {
            let c = clock_check(Some(HOME), Some("UTC"), true);
            assert_eq!(
                c.level,
                Level::Fail,
                "a zone change is the exact thing the anchor exists to catch"
            );
            assert!(
                c.text.contains(HOME) && c.text.contains("UTC"),
                "{}",
                c.text
            );
            let fix = c.fix.unwrap_or_default();
            assert!(
                fix.contains("Re-anchor") || fix.contains("re-anchor"),
                "the fix must name the action that resolves it: {fix}"
            );
        }

        #[test]
        fn an_install_predating_the_zone_check_is_warned_not_failed() {
            let c = clock_check(None, Some(HOME), true);
            assert_eq!(c.level, Level::Warn);
            assert!(
                c.fix.unwrap_or_default().contains("two hours"),
                "say what the weaker check actually costs"
            );
        }

        #[test]
        fn no_anchor_at_all_is_warned_with_what_it_leaves_open() {
            let c = clock_check(None, None, false);
            assert_eq!(c.level, Level::Warn);
            assert!(c.text.contains("NOT anchored"), "{}", c.text);
        }

        /// A dev build, or a Windows call that failed. Not an error either way -- the service
        /// falls back to the check it always used.
        #[test]
        fn an_unreadable_current_zone_never_fails() {
            for anchored in [true, false] {
                let c = clock_check(Some(HOME), None, anchored);
                assert_ne!(
                    c.level,
                    Level::Fail,
                    "not being able to read the zone is not a fault (anchored={anchored})"
                );
            }
        }
    }

    use super::*;

    /// `doctor` is where the README sends a parent when something looks wrong, so a green report on
    /// a build that can never capture sends them hunting in the wrong place. `install` mentions it
    /// once, in a console they scroll past; this is the surface they come back to.
    #[test]
    fn doctor_reports_a_windows_build_too_old_to_capture() {
        let c = capture_check(17_763);
        assert_eq!(
            c.level,
            Level::Warn,
            "a caution, not a failure: every other half of the product works on this build"
        );
        assert!(
            c.text.contains("1903"),
            "name the version a parent can check with winver: {}",
            c.text
        );
        assert!(
            c.fix.is_some(),
            "a warn without a fix is just bad news; say what to do"
        );
    }

    /// A supported build must say so, rather than say nothing.
    ///
    /// `doctor` is a checklist, and a section that is simply absent reads exactly like a check that
    /// never ran — which is the distinction this whole product is built on keeping (`measured` on
    /// `DayRow`, `focus_missing`, `Stamp::Missing` vs `Stamp::Corrupt`). A parent whose screenshots
    /// fail on a modern machine needs to see that this was checked and was not the cause.
    #[test]
    fn a_supported_build_is_reported_as_checked_not_as_silence() {
        let c = capture_check(19_045);
        assert_eq!(c.level, Level::Ok);
        assert!(
            c.text.contains("19045"),
            "name the build, so a parent reporting a problem can quote it: {}",
            c.text
        );
    }

    /// An unreadable build is the third state and must not be rendered as either of the other two.
    ///
    /// `capture_build_ok(0)` is `true`, so this does **not** warn — matching `install`, which
    /// deliberately does not block on a syscall that misbehaved. But claiming the build "supports
    /// screen capture" would report an unmeasured thing as measured, which is the exact error the
    /// `Option`s and `measured` flags elsewhere exist to prevent.
    #[test]
    fn an_unreadable_build_says_so_rather_than_claiming_support() {
        let c = capture_check(0);
        assert_eq!(
            c.level,
            Level::Ok,
            "must not alarm: install does not either"
        );
        let s = c.text.to_lowercase();
        assert!(
            s.contains("could not") || s.contains("unknown") || s.contains("unreadable"),
            "say the number could not be read rather than asserting support: {}",
            c.text
        );
        assert!(
            !s.contains("supports screen capture"),
            "an unmeasured build must not be reported as a measured pass: {}",
            c.text
        );
    }

    /// The call site, not the decision — `doctor_reports_a_windows_build_too_old_to_capture` proves
    /// `capture_check` is right, and would stay green if nothing ever called it.
    ///
    /// A source scan because the property is the *existence of a call*, inside a `#[cfg(windows)]`
    /// function this machine cannot execute. `capture_check` keeps its own tests, so deleting the
    /// call would not even raise dead code. This is the same shape as
    /// `rules::tests::standing_down_closes_an_open_session`. Scanners like this are a smell and
    /// their number should not grow — but no unit test can watch a call site inside code this
    /// machine cannot compile.
    #[test]
    fn the_capture_check_reaches_the_report() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/doctor.rs"),
        )
        .expect("reading src/doctor.rs")
        .replace("\r\n", "\n");
        let windows_checks = src
            .split_once("#[cfg(windows)]\nfn platform_checks(")
            .expect("the Windows platform_checks must exist")
            .1;
        let body = windows_checks
            .split_once("\n#[cfg(not(windows))]")
            .expect("platform_checks must end")
            .0;
        assert!(
            body.contains("capture_check(crate::preflight::os_build())"),
            "doctor must ask whether this machine can capture; without this call the check is \
             dead code and a pre-1903 machine reads as entirely healthy"
        );
    }

    #[test]
    fn the_report_header_names_the_build() {
        let out = Report::default().render();
        assert!(
            out.contains(crate::VERSION),
            "doctor's output must name the build it came from; got:\n{out}"
        );
    }

    /// The case this check exists for: a newer binary sitting beside an older installed service.
    /// Reporting that as healthy is how someone concludes a fix is present when it is not.
    #[test]
    fn a_newer_binary_beside_an_older_service_is_a_warning_that_says_to_install() {
        let c = version_check(
            &crate::install::Stamp::Version("0.2.3".into()),
            "0.10.0",
            true,
        )
        .expect("must report something");
        assert_eq!(c.level, Level::Warn);
        assert!(
            c.text.contains("0.10.0") && c.text.contains("0.2.3"),
            "both versions must appear so it is obvious which is which: {}",
            c.text
        );
        assert!(
            c.fix.as_deref().is_some_and(|f| f.contains("install")),
            "the fix has to name the step that was missed: {:?}",
            c.fix
        );
    }

    #[test]
    fn a_matching_version_is_reported_as_healthy() {
        let c = version_check(
            &crate::install::Stamp::Version("0.2.3".into()),
            "0.2.3",
            true,
        )
        .expect("must report something");
        assert_eq!(c.level, Level::Ok);
    }

    /// Running an old copy makes every other line in the report describe something else. Say so
    /// before the reader acts on the rest of it.
    #[test]
    fn an_older_binary_than_the_installed_service_is_flagged() {
        let c = version_check(
            &crate::install::Stamp::Version("0.3.0".into()),
            "0.2.3",
            true,
        )
        .expect("must report something");
        assert_eq!(c.level, Level::Warn);
        assert!(c.text.contains("older"), "got: {}", c.text);
    }

    /// An unreadable record must not read as "no record" — same distinction `classify_install`
    /// draws, and it has to survive the trip to the report.
    ///
    /// Both routes to `Unknown` are checked: a record that would not parse at all (`Corrupt`) and
    /// one that parsed but holds something this build cannot order.
    #[test]
    fn an_unreadable_record_is_reported_rather_than_passed_over() {
        for stamp in [
            crate::install::Stamp::Corrupt,
            crate::install::Stamp::Version("garbage".into()),
        ] {
            let c = version_check(&stamp, "0.2.3", true).expect("must report something");
            assert_eq!(c.level, Level::Warn);
            assert!(c.text.contains("unreadable"), "got: {}", c.text);

            // The regression this guards. `read_stamp` used to return the placeholder string
            // "(unreadable)" in the version field, which this line then wrapped in parentheses of
            // its own — rendering "the installed-version record is unreadable ((unreadable))". The
            // earlier version of this test asserted only `contains("unreadable")`, so it passed
            // while the output was malformed. Assert the shape, not just the keyword.
            assert!(
                !c.text.contains("(("),
                "doubled parentheses in the report: {}",
                c.text
            );
            assert!(
                !c.text.contains("unreadable ("),
                "a placeholder is being printed where a version belongs: {}",
                c.text
            );
        }
    }

    /// On a machine with nothing installed this would only repeat the "not installed" failure
    /// printed a line above it.
    #[test]
    fn a_machine_with_no_install_gets_no_version_line() {
        assert!(version_check(&crate::install::Stamp::Missing, "0.2.3", false).is_none());
    }

    #[test]
    fn an_install_predating_the_record_is_mentioned_once() {
        let c = version_check(&crate::install::Stamp::Missing, "0.2.3", true)
            .expect("must report something");
        assert_eq!(c.level, Level::Warn);
    }

    #[test]
    fn render_groups_checks_and_counts_them() {
        let mut r = Report::default();
        let s = r.section("Network");
        s.push(ok("reachable"));
        s.push(warn("network is Public", "set it to Private"));
        let s2 = r.section("Service");
        s2.push(fail("not running", "start it"));

        assert_eq!(r.counts(), (1, 1));
        let out = r.render();
        assert!(out.contains("  Network"));
        assert!(out.contains("[ok]   reachable"));
        assert!(out.contains("[warn] network is Public"));
        assert!(out.contains("set it to Private"), "fixes must be shown");
        assert!(out.contains("1 problem(s), 1 warning(s)"));
    }

    #[test]
    fn a_clean_report_says_so() {
        let mut r = Report::default();
        r.section("Network").push(ok("fine"));
        assert_eq!(r.counts(), (0, 0));
        assert!(r.render().contains("All good."));
    }

    #[test]
    fn multi_line_fixes_stay_indented() {
        let mut r = Report::default();
        r.section("X").push(warn("problem", "line one\nline two"));
        let out = r.render();
        assert!(out.contains("           line one"));
        assert!(out.contains("           line two"));
    }
}
