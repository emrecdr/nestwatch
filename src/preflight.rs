//! Everything that must be true before `install` changes anything.
//!
//! Install is a sequence of irreversible-ish steps — stop the old service, overwrite the binary,
//! rewrite the firewall rule, register and start a service — and each one used to discover its
//! own problems as it hit them. That produced installs that reported success and did not work
//! (a Public network, so the firewall rule can never match), installs that failed halfway
//! (a leftover service still being deleted), and installs that asked for a password before
//! finding out they could not proceed.
//!
//! So the preconditions are checked first, together, before anything is touched.
//!
//! **Every check runs, even after one fails.** Stopping at the first problem means the parent
//! fixes it, re-runs an elevated command, and meets the next one — a machine with three problems
//! costs three trips. All of them are reported at once.
//!
//! The decisions here are pure and unit-tested; the I/O that feeds them lives in [`gather`],
//! which is the same split the enforcers use.

use std::fmt::Write as _;

/// How much a finding matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Install cannot succeed. Refuse before touching anything.
    Blocker,
    /// Install will succeed, but something about the result will not be what was wanted.
    Caution,
}

/// One precondition that is not met.
///
/// `fix` is not optional. A finding a parent cannot act on is a finding that wastes their time —
/// this is a tool installed by hand, at a machine, usually not by someone who wants to be
/// reading about service control managers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    /// What is wrong, in one line.
    pub what: String,
    /// Why it matters — specifically, what will go wrong if it is left alone.
    pub why: String,
    /// What to do about it.
    pub fix: String,
}

impl Finding {
    pub fn blocker(
        what: impl Into<String>,
        why: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self {
            severity: Severity::Blocker,
            what: what.into(),
            why: why.into(),
            fix: fix.into(),
        }
    }

    pub fn caution(
        what: impl Into<String>,
        why: impl Into<String>,
        fix: impl Into<String>,
    ) -> Self {
        Self {
            severity: Severity::Caution,
            what: what.into(),
            why: why.into(),
            fix: fix.into(),
        }
    }
}

/// Whether any finding makes the install impossible.
pub fn blocked(findings: &[Finding]) -> bool {
    findings.iter().any(|f| f.severity == Severity::Blocker)
}

/// The report a parent reads.
///
/// Blockers first regardless of the order they were found in, because that is the order they have
/// to be dealt with. Pure, so the wording is testable without a Windows machine.
pub fn render(findings: &[Finding]) -> String {
    if findings.is_empty() {
        return "Pre-flight checks passed.\n".to_string();
    }

    let mut out = String::new();
    let (blockers, cautions): (Vec<_>, Vec<_>) = findings
        .iter()
        .partition(|f| f.severity == Severity::Blocker);

    if !blockers.is_empty() {
        let _ = writeln!(
            out,
            "\nPre-flight found {} thing{} that will stop this install:\n",
            blockers.len(),
            if blockers.len() == 1 { "" } else { "s" }
        );
        for (i, f) in blockers.iter().enumerate() {
            let _ = write!(out, "{}", entry(i + 1, f));
        }
    }

    if !cautions.is_empty() {
        let _ = writeln!(
            out,
            "\nPre-flight found {} thing{} worth knowing before you continue:\n",
            cautions.len(),
            if cautions.len() == 1 { "" } else { "s" }
        );
        for (i, f) in cautions.iter().enumerate() {
            let _ = write!(out, "{}", entry(i + 1, f));
        }
    }

    if !blockers.is_empty() {
        let _ = writeln!(
            out,
            "Nothing has been changed on this machine. Fix the above and run install again."
        );
    }
    out
}

/// One finding, indented so several read as a list rather than a wall.
///
/// Continuation lines of the fix align under the first one rather than under its `->` marker, so
/// a two-line instruction reads as one instruction. Trailing whitespace is stripped: these
/// strings are written as Rust line continuations, which leave a space before every break.
fn entry(n: usize, f: &Finding) -> String {
    let block = |s: &str, first: &str, rest: &str| -> String {
        s.lines()
            .enumerate()
            .map(|(i, l)| {
                let pad = if i == 0 { first } else { rest };
                format!("{pad}{}\n", l.trim_end())
            })
            .collect()
    };
    format!(
        "  {n}. {}\n{}{}\n",
        f.what.trim_end(),
        block(&f.why, "     ", "     "),
        block(&f.fix, "     -> ", "        "),
    )
}

/// Run every precondition check and return what is not met.
///
/// Ordering inside this function is irrelevant — [`render`] sorts by severity — so the checks are
/// grouped by what they inspect rather than by how bad they are.
pub fn gather(port: u16) -> Vec<Finding> {
    let mut out = Vec::new();
    check_port(port, &mut out);
    #[cfg(windows)]
    {
        check_system_tools(&mut out);
        check_existing_service(&mut out);
        check_mark_of_the_web(&mut out);
        check_network_profile(&mut out);
    }
    out
}

/// The port must be free *before* the service is registered.
///
/// Otherwise the service starts, fails to bind, and exits within milliseconds — which surfaces as
/// "started and then stopped immediately" from the installer, several irreversible steps later.
/// Binding is the only honest test: `netstat` parsing cannot see a socket opened between the
/// check and the bind, and neither can this, but a real bind at least fails the same way the
/// service would.
fn check_port(port: u16, out: &mut Vec<Finding>) {
    use std::net::TcpListener;
    // 0.0.0.0 because that is what the server binds; a listener on a single interface would miss
    // a conflict on another one.
    match TcpListener::bind(("0.0.0.0", port)) {
        Ok(l) => drop(l),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => out.push(Finding::blocker(
            format!("port {port} is already in use"),
            "The service binds this port at startup. Another program has it, so the service \n\
             would start, fail to bind, and exit within a second of being installed.",
            format!(
                "Find the program holding it:  netstat -ano | findstr :{port}\n\
                 Then either stop it, or install on a different port:  install --port 8444"
            ),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => out.push(Finding::blocker(
            format!("not allowed to bind port {port}"),
            "Windows refused the port. Ports below 1024 are reserved, and a port can also be \n\
             held in an excluded range.",
            "Use a port above 1024:  install --port 8443\n\
             Check exclusions with:  netsh interface ipv4 show excludedportrange protocol=tcp",
        )),
        // Anything else (a firewall hook, an exotic failure) is not something to block on: the
        // service gets the same treatment and reports it properly now.
        Err(_) => {}
    }
}

/// The Windows tools install shells out to must exist where we expect them.
///
/// `syspath` resolves these by absolute path precisely so a look-alike beside the .exe cannot be
/// run with admin rights. That protection turns a missing tool into a confusing failure deep in
/// the install rather than an obvious one here.
#[cfg(windows)]
fn check_system_tools(out: &mut Vec<Finding>) {
    let missing: Vec<String> = [
        crate::syspath::system32("sc.exe"),
        crate::syspath::system32("netsh.exe"),
        crate::syspath::system32("icacls.exe"),
        crate::syspath::powershell(),
    ]
    .into_iter()
    .filter(|p| !p.exists())
    .map(|p| p.display().to_string())
    .collect();

    if !missing.is_empty() {
        out.push(Finding::blocker(
            "some Windows system tools are missing",
            format!(
                "Install needs these to register the service, add the firewall rule and lock \n\
                 down the data folder. Not found:\n  {}",
                missing.join("\n  ")
            ),
            "This usually means a damaged Windows install or a heavily stripped image. \n\
             `sfc /scannow` from an elevated prompt is the standard repair.",
        ));
    }
}

/// A leftover service in a state that will make this install fail.
///
/// Both of these were hit on a real machine, one after the other: a service still being deleted
/// (1072) refuses every configuration call, and a service left Disabled by a half-finished
/// removal refuses to start (1058). Each aborted the install partway, after the binary had
/// already been overwritten.
#[cfg(windows)]
fn check_existing_service(out: &mut Vec<Finding>) {
    use windows_service::service::{ServiceAccess, ServiceStartType, ServiceState};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    let Ok(manager) =
        ServiceManager::local_computer(None::<&std::ffi::OsStr>, ServiceManagerAccess::CONNECT)
    else {
        return;
    };
    let name = crate::service::SERVICE_NAME;

    let Ok(svc) = manager.open_service(
        name,
        ServiceAccess::QUERY_STATUS | ServiceAccess::QUERY_CONFIG,
    ) else {
        // Not registered. That is the normal first-install case, and 1072 (marked for deletion)
        // also lands here on some builds -- which the query below cannot see either. The create
        // call reports that one properly now.
        return;
    };

    if let Ok(cfg) = svc.query_config()
        && cfg.start_type == ServiceStartType::Disabled
    {
        out.push(Finding::blocker(
            format!("the existing '{name}' service is disabled"),
            "Windows refuses to start a disabled service (error 1058), so this install would \n\
             register everything and then fail at the last step. It is usually left this way by \n\
             a removal that did not finish.",
            format!(
                "Re-enable it:  sc config {name} start= auto\n\
                 Or remove it and let this install recreate it:  sc delete {name}\n\
                 A reboot clears a deletion that is still pending."
            ),
        ));
    }

    if let Ok(status) = svc.query_status()
        && status.current_state == ServiceState::StopPending
    {
        out.push(Finding::caution(
            format!("the existing '{name}' service is still stopping"),
            "Install stops it before replacing the binary, and a stop that is taking this long \n\
             may leave the file locked.",
            "Wait a few seconds and run install again. If it persists, reboot.",
        ));
    }
}

/// Windows marks files downloaded from the internet, and refuses to run some of them.
///
/// Detected by the `Zone.Identifier` alternate data stream, which is where the mark lives — the
/// same thing "Unblock" in the file's properties removes. Worth catching here because the
/// symptom otherwise is a service that will not launch, with nothing pointing at the cause.
#[cfg(windows)]
fn check_mark_of_the_web(out: &mut Vec<Finding>) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    // An ADS is opened as `path:StreamName`. Present means the file is still marked.
    let ads = format!("{}:Zone.Identifier", exe.display());
    if std::fs::metadata(&ads).is_ok() {
        out.push(Finding::caution(
            "this .exe is still marked as downloaded from the internet",
            "Windows may refuse to run it as a service, which shows up as a service that never \n\
             starts and no explanation. It can also make antivirus hold the copy install makes.",
            format!(
                "Right-click {} -> Properties -> tick Unblock -> OK, then run install again.\n\
                 Or from this prompt:  Unblock-File '{}'",
                exe.display(),
                exe.display()
            ),
        ));
    }
}

/// The firewall rule install adds is scoped to Private and Domain networks.
///
/// On a Public network it never matches, Windows blocks all inbound, and every device times out —
/// an install that reports success, a service running correctly, and a dashboard nobody can
/// reach. A caution rather than a blocker: the install is genuinely fine, and the setting can be
/// changed afterwards without reinstalling.
#[cfg(windows)]
fn check_network_profile(out: &mut Vec<Finding>) {
    let profiles = crate::doctor::network_profiles();
    // Empty means the query failed, which is not the same as Public. Per adapter, because
    // Hyper-V, WSL and VPN adapters are routinely Public while the real Wi-Fi is fine.
    if !profiles.is_empty() && profiles.iter().all(|p| p == "Public") {
        out.push(Finding::caution(
            "this PC's network is set to Public",
            "The firewall rule only applies on Private and Domain networks. On a Public one \n\
             Windows blocks every incoming connection, so the dashboard address and the QR code \n\
             will time out from every device -- even though the service is running.",
            "Settings > Network & internet > (your Wi-Fi) > Network profile type > Private.\n\
             It takes effect immediately; nothing needs reinstalling.",
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b() -> Finding {
        Finding::blocker(
            "port 8443 is in use",
            "the service would exit at once",
            "pick another",
        )
    }
    fn c() -> Finding {
        Finding::caution(
            "network is Public",
            "nothing can reach it",
            "set it to Private",
        )
    }

    #[test]
    fn only_blockers_stop_the_install() {
        assert!(!blocked(&[]));
        assert!(!blocked(&[c(), c()]), "cautions must never block");
        assert!(blocked(&[c(), b()]), "a blocker anywhere blocks");
    }

    /// A clean run has to say so. Silence reads as "the check did not run", which is exactly the
    /// doubt this whole module exists to remove.
    #[test]
    fn a_clean_report_says_so() {
        let out = render(&[]);
        assert!(out.contains("passed"), "got: {out}");
    }

    /// Blockers come first even when found later, because that is the order they must be dealt
    /// with, and a parent reading top-down should not start with something optional.
    #[test]
    fn blockers_are_reported_before_cautions() {
        let out = render(&[c(), b()]);
        let (ci, bi) = (out.find("Public").unwrap(), out.find("8443").unwrap());
        assert!(bi < ci, "blocker must precede caution:\n{out}");
    }

    /// The promise that nothing was touched only holds when we are refusing to proceed. Printing
    /// it on a cautions-only run would be a lie — that install goes ahead and changes plenty.
    #[test]
    fn the_nothing_changed_promise_appears_only_when_refusing() {
        assert!(render(&[b()]).contains("Nothing has been changed"));
        assert!(
            !render(&[c()]).contains("Nothing has been changed"),
            "a cautions-only install proceeds and does change things"
        );
        assert!(!render(&[]).contains("Nothing has been changed"));
    }

    /// Every finding must carry all three parts. A problem with no fix is a problem the reader
    /// is standing at a machine unable to act on.
    #[test]
    fn every_finding_shows_what_why_and_fix() {
        let out = render(&[b()]);
        assert!(out.contains("port 8443 is in use"), "what:\n{out}");
        assert!(
            out.contains("the service would exit at once"),
            "why:\n{out}"
        );
        assert!(out.contains("-> pick another"), "fix:\n{out}");
    }

    #[test]
    fn counts_agree_with_singular_and_plural() {
        assert!(render(&[b()]).contains("1 thing that will stop"));
        assert!(render(&[b(), b()]).contains("2 things that will stop"));
    }
}
