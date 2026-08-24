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

#[derive(PartialEq, Clone, Copy)]
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

fn fail(text: impl Into<String>, fix: impl Into<String>) -> Check {
    Check {
        level: Level::Fail,
        text: text.into(),
        fix: Some(fix.into()),
    }
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
    }

    // --- Network -----------------------------------------------------------
    {
        let hosts = crate::cert::reachable_hosts();
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

#[cfg(windows)]
fn platform_network_checks(port: u16, checks: &mut Vec<Check>) {
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
    } else if profiles.iter().all(|p| p == "Public") {
        checks.push(warn(
            "this PC's network is set to Public",
            "The firewall rule only applies on Private/Domain networks, so other devices\n\
             can't reach the dashboard. Fix it from an elevated PowerShell:\n\
             Get-NetConnectionProfile | Where-Object {$_.NetworkCategory -eq 'Public'} |\n\
             Set-NetConnectionProfile -NetworkCategory Private\n\
             Or: Settings > Network & internet > (your Wi-Fi) > Network profile type.",
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
    use super::*;

    #[test]
    fn the_report_header_names_the_build() {
        let out = Report::default().render();
        assert!(
            out.contains(crate::VERSION),
            "doctor's output must name the build it came from; got:\n{out}"
        );
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
