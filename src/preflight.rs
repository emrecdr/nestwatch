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
//!
//! One exception to "before anything is touched", and it is deliberate: a finding may carry a
//! [`Remedy`], and [`apply`] performs it — a PowerShell call, an `sc.exe config`, a file write.
//! Nothing here runs unasked; a remedy happens only when a parent answers yes at the prompt, or
//! passes `--fix`. Auditing this module for "does it change the machine?" should stop at
//! [`apply`] and nowhere else.

use std::fmt::Write as _;

/// How much a finding matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Install cannot succeed. Refuse before touching anything.
    Blocker,
    /// Install will succeed, but something about the result will not be what was wanted.
    Caution,
}

/// A problem this installer can correct itself, given permission.
///
/// An enum rather than a boxed closure so the decision to offer a fix stays pure data — testable,
/// printable, and comparable — with the side effects confined to [`apply`]. Same split the
/// enforcers use, and the reason the checks in [`gather`] are unit-testable at all.
///
/// Deliberately narrow. A fix belongs here only when it is unambiguous, reversible, and squarely
/// within what installing this tool implies. Anything needing a judgement call — which program to
/// stop so a port frees up, whether to repair Windows — gets no remedy at all and leaves the
/// finding's `fix` prose as the whole answer, because guessing on someone else's machine is worse
/// than asking.
///
/// Absence is `Option::None` rather than a `Manual` variant. As a variant it had to be accepted
/// by [`apply`], which returned `Ok("")` for it — a success carrying no message, unreachable
/// because [`fixable`] filters it out first, and printing as a bare `done:` if it ever were
/// reached. With `Option`, every value of this type is something [`apply`] can actually perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Remedy {
    /// Switch every Public network adapter to Private.
    MakeNetworkPrivate,
    /// Strip the downloaded-from-the-internet mark from this file.
    UnblockFile(std::path::PathBuf),
    /// Set a disabled service back to automatic start.
    EnableService,
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
    /// What to do about it, in words, whether or not [`Self::remedy`] can do it for them.
    pub fix: String,
    /// What the installer can do for them, on request, if anything.
    pub remedy: Option<Remedy>,
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
            remedy: None,
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
            remedy: None,
        }
    }

    /// Attach a fix the installer can apply itself.
    ///
    /// Named for what it sets, not for what it makes the finding: `fixable` was already the free
    /// function below, which selects findings rather than building one.
    #[must_use]
    pub fn with_remedy(mut self, remedy: Remedy) -> Self {
        self.remedy = Some(remedy);
        self
    }
}

/// Findings this installer could correct, in report order.
pub fn fixable(findings: &[Finding]) -> Vec<&Finding> {
    findings.iter().filter(|f| f.remedy.is_some()).collect()
}

/// No remedy is constructible off Windows -- every check that produces one is `cfg(windows)` --
/// so this exists only to keep the cross-platform `install` path compiling and testable.
#[cfg(not(windows))]
pub fn apply(remedy: &Remedy) -> Result<String, String> {
    Err(format!("{remedy:?} is only available on Windows"))
}

/// Where Windows records that a file was downloaded from the internet.
///
/// Shared by the check that looks for the mark and the remedy that deletes it, which sit ~280
/// lines apart. Change how the mark is located and only one of them would move otherwise —
/// leaving pre-flight reporting a finding whose fix targets a different path, which reads to a
/// parent as "it said done and nothing happened".
#[cfg(windows)]
fn zone_identifier(path: &std::path::Path) -> String {
    format!("{}:Zone.Identifier", path.display())
}

/// Carry out one remedy.
///
/// Returns the error as a `String` because every caller prints it rather than matching on it: a
/// fix that fails is reported and the install carries on, since none of these are required for a
/// correct install -- they are conveniences for problems the parent could fix by hand.
#[cfg(windows)]
pub fn apply(remedy: &Remedy) -> Result<String, String> {
    match remedy {
        // Documented as equivalent to the Unblock tick-box in the file's properties: it removes
        // the Zone.Identifier alternate data stream and nothing else. Done natively rather than
        // through `Unblock-File`, since deleting the stream IS the operation and a subprocess
        // would only add a failure mode.
        Remedy::UnblockFile(path) => {
            let ads = zone_identifier(path);
            match std::fs::remove_file(&ads) {
                Ok(()) => Ok(format!("unblocked {}", path.display())),
                // Already gone is success: the goal is the absence of the mark, not the deletion.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    Ok("already unblocked".into())
                }
                Err(e) => Err(format!("could not remove the mark: {e}")),
            }
        }

        Remedy::MakeNetworkPrivate => {
            let out = std::process::Command::new(crate::syspath::powershell())
                .args(["-NoProfile", "-Command", crate::doctor::MAKE_PRIVATE_PS])
                .output()
                .map_err(|e| format!("could not run PowerShell: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "PowerShell refused it: {}",
                    crate::syspath::tool_output(&out)
                ));
            }
            // Confirm rather than assume: the cmdlet can exit 0 having matched nothing.
            let left = crate::doctor::network_profiles();
            if crate::doctor::all_public(&left) {
                return Err("the command ran but the network is still Public".into());
            }
            Ok("network set to Private".into())
        }

        Remedy::EnableService => {
            let out = std::process::Command::new(crate::syspath::system32("sc.exe"))
                .args(["config", crate::service::SERVICE_NAME, "start=", "auto"])
                .output()
                .map_err(|e| format!("could not run sc.exe: {e}"))?;
            if out.status.success() {
                Ok("service set to start automatically".into())
            } else {
                Err(format!(
                    "sc.exe refused it: {}",
                    crate::syspath::tool_output(&out)
                ))
            }
        }
    }
}

/// Whether any finding makes the install impossible.
pub fn blocked(findings: &[Finding]) -> bool {
    findings.iter().any(|f| f.severity == Severity::Blocker)
}

/// Whether anything on this machine has already been altered when a report is printed.
///
/// [`render`] is pure and gets asked twice per install: once before anything happens, and again
/// after any accepted remedies have run. It closes a blocking report by telling the parent
/// nothing was touched -- which is worth saying, and was false the second time. Accepting the
/// Public-network fix and then failing on a different blocker printed "Nothing has been changed
/// on this machine." immediately after changing the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Machine {
    /// Nothing has run yet; the reassurance is true.
    Untouched,
    /// At least one remedy was applied, so the report must not claim otherwise.
    Changed,
}

/// The report a parent reads.
///
/// Blockers first regardless of the order they were found in, because that is the order they have
/// to be dealt with. Pure, so the wording is testable without a Windows machine.
pub fn render(findings: &[Finding], machine: Machine) -> String {
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
            "{} Fix the above and run install again.",
            match machine {
                Machine::Untouched => "Nothing has been changed on this machine.",
                Machine::Changed => "Apart from the fixes you accepted, nothing has been changed.",
            }
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
                let line = l.trim_end();
                // A blank separator line must not carry the padding, or it becomes a line of
                // trailing spaces -- invisible on screen, and noise in anything that quotes it.
                if line.is_empty() {
                    return "\n".to_string();
                }
                let pad = if i == 0 { first } else { rest };
                format!("{pad}{line}\n")
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
    check_port(port, running_service_port(), &mut out);
    #[cfg(windows)]
    {
        check_system_tools(&mut out);
        check_existing_service(&mut out);
        check_mark_of_the_web(&mut out);
        check_network_profile(&mut out);
    }
    out
}

/// The port our own service is serving right now, if it is running.
///
/// Only the running case counts. A stopped or disabled service holds nothing, so a busy port
/// then is somebody else's and must still block.
#[cfg(not(windows))]
fn running_service_port() -> Option<u16> {
    None
}

/// The port our own service is serving right now, if it is running.
///
/// Read as "is the service up" plus "what did we configure it with", rather than by asking the OS
/// which process owns the socket -- that needs `GetExtendedTcpTable` and a PID comparison, for an
/// answer these two facts already give. If the service is Running it has the port from its config;
/// nothing else can have bound it.
#[cfg(windows)]
fn running_service_port() -> Option<u16> {
    use windows_service::service::{ServiceAccess, ServiceState};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    let manager =
        ServiceManager::local_computer(None::<&std::ffi::OsStr>, ServiceManagerAccess::CONNECT)
            .ok()?;
    let svc = manager
        .open_service(crate::service::SERVICE_NAME, ServiceAccess::QUERY_STATUS)
        .ok()?;
    let state = svc.query_status().ok()?.current_state;
    // StartPending counts: it has already bound, or is about to, and install stops it either way.
    if !matches!(state, ServiceState::Running | ServiceState::StartPending) {
        return None;
    }
    crate::config::Config::load().ok().map(|c| c.port)
}

/// The port must be free *before* the service is registered.
///
/// Otherwise the service starts, fails to bind, and exits within milliseconds — which surfaces as
/// "started and then stopped immediately" from the installer, several irreversible steps later.
/// Binding is the only honest test: `netstat` parsing cannot see a socket opened between the
/// check and the bind, and neither can this, but a real bind at least fails the same way the
/// service would.
fn check_port(port: u16, ours_on: Option<u16>, out: &mut Vec<Finding>) {
    use std::net::TcpListener;
    // An upgrade over a running install: the thing holding this port is us. `deploy` stops the
    // service before it rebinds, so this is not a conflict -- and treating it as one refused
    // every in-place upgrade, including the remote one docs/REMOTE-UPDATE.md describes.
    //
    // Matched on the port, not merely on "the service is up": a service serving 8443 says nothing
    // about whether something else holds the 9000 an `install --port 9000` is asking for.
    if ours_on == Some(port) {
        return;
    }
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
    const REPAIR: &str = "This usually means a damaged Windows install or a heavily stripped \n\
         image. `sfc /scannow` from an elevated prompt is the standard repair.";

    let absent = |tools: &[std::path::PathBuf]| -> Vec<String> {
        tools
            .iter()
            .filter(|p| !p.exists())
            .map(|p| p.display().to_string())
            .collect()
    };

    // Install cannot finish without these.
    let missing = absent(&[
        crate::syspath::system32("sc.exe"),
        crate::syspath::system32("netsh.exe"),
        crate::syspath::system32("icacls.exe"),
        crate::syspath::powershell(),
    ]);
    if !missing.is_empty() {
        out.push(Finding::blocker(
            "some Windows system tools are missing",
            format!(
                "Install needs these to register the service, add the firewall rule and lock \n\
                 down the data folder. Not found:\n  {}",
                missing.join("\n  ")
            ),
            REPAIR,
        ));
    }

    // Install finishes fine without these; the product does not work. A caution rather than a
    // blocker for exactly that reason -- and worth stating at install time, because the symptom
    // otherwise is bedtime arriving and nothing happening, months later, with no error anywhere.
    let missing = absent(&[
        crate::syspath::system32("shutdown.exe"),
        crate::syspath::system32("rundll32.exe"),
    ]);
    if !missing.is_empty() {
        out.push(Finding::caution(
            "the tools that enforce bedtime and lock are missing",
            format!(
                "Install will finish and the dashboard will work, but the curfew cannot lock \n\
                 or shut this PC down when it fires -- it would look like it is working and do \n\
                 nothing at the moment it matters. Not found:\n  {}",
                missing.join("\n  ")
            ),
            REPAIR,
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
        out.push(
            Finding::blocker(
                format!("the existing '{name}' service is disabled"),
                "Windows refuses to start a disabled service (error 1058), so this install would \n\
             register everything and then fail at the last step. It is usually left this way by \n\
             a removal that did not finish.",
                format!(
                    "Re-enable it:  sc config {name} start= auto\n\
                 Or remove it and let this install recreate it:  sc delete {name}\n\
                 A reboot clears a deletion that is still pending."
                ),
            )
            .with_remedy(Remedy::EnableService),
        );
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
    let ads = zone_identifier(&exe);
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
        )
        .with_remedy(Remedy::UnblockFile(exe)));
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
    // Per adapter, because Hyper-V, WSL and VPN adapters are routinely Public while the real
    // Wi-Fi is fine. `all_public` owns the "empty means the query failed" rule.
    if crate::doctor::all_public(&profiles) {
        out.push(
            Finding::caution(
                "this PC's network is set to Public",
                "The firewall rule only applies on Private and Domain networks. On a Public one \n\
             Windows blocks every incoming connection, so the dashboard address and the QR code \n\
             will time out from every device -- even though the service is running.",
                format!(
                    "From this same elevated PowerShell, one line:\n\
             \n  \
             {}\n\
             \n\
             (That switches every Public adapter on this machine to Private, which is what you\n\
             want on a home PC. Run Get-NetConnectionProfile first if you would rather see them\n\
             and pick one by -Name.)\n\
             \n\
             Or by hand: Settings > Network & internet > (your Wi-Fi) > Network profile type.\n\
             Either way it takes effect immediately -- nothing needs reinstalling, and you do\n\
             not need to run install again.",
                    crate::doctor::MAKE_PRIVATE_PS
                ),
            )
            .with_remedy(Remedy::MakeNetworkPrivate),
        );
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

    /// An upgrade is not a port conflict.
    ///
    /// `install` over a running Nestwatch binds the port its own service is already holding. That
    /// arrived as a Blocker, so pre-flight refused every in-place upgrade -- including the remote
    /// one `docs/REMOTE-UPDATE.md` describes, where nobody is at the machine to see why. Held by
    /// a real listener rather than a mocked one, because the bug was in what `bind` actually does.
    #[test]
    fn an_upgrade_over_our_own_service_is_not_a_conflict() {
        let live = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = live.local_addr().unwrap().port();

        let mut out = Vec::new();
        check_port(port, Some(port), &mut out);
        assert!(
            out.is_empty(),
            "our own service holding the port is an upgrade, not a conflict: {out:#?}"
        );
    }

    /// The narrowness of that exemption is the whole of it.
    ///
    /// Anything that is not our service on this exact port must still block: a foreign program,
    /// and -- the case a looser "is the service running?" test would wave through -- our service
    /// running on a different port while the requested one is taken by something else.
    #[test]
    fn anything_other_than_our_own_service_still_blocks() {
        let live = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = live.local_addr().unwrap().port();
        // Ephemeral ports are well above 1024, so this cannot underflow.
        for ours_on in [None, Some(port - 1)] {
            let mut out = Vec::new();
            check_port(port, ours_on, &mut out);
            assert!(
                blocked(&out),
                "a port held by something else must block (our service on {ours_on:?})"
            );
        }
    }

    /// A free port is a free port, upgrade or not.
    #[test]
    fn a_free_port_produces_no_finding() {
        let port = {
            let probe = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
            probe.local_addr().unwrap().port()
        };
        let mut out = Vec::new();
        check_port(port, None, &mut out);
        assert!(out.is_empty(), "nothing holds this port: {out:#?}");
    }

    /// The closing reassurance has to be true when it is printed.
    ///
    /// The report is rendered twice: once before anything runs, and again after accepted remedies
    /// have. The second one said "Nothing has been changed on this machine." having just switched
    /// the network profile -- the one line a parent would rely on to know where they stand.
    #[test]
    fn a_report_printed_after_a_fix_does_not_claim_nothing_changed() {
        let after = render(&[b()], Machine::Changed);
        assert!(
            !after.contains("Nothing has been changed on this machine"),
            "a remedy has already run, so this is false:\n{after}"
        );
        assert!(after.contains("Apart from the fixes you accepted"));
        assert!(
            after.contains("run install again"),
            "it must still say what to do next"
        );

        // And the untouched case must keep saying it -- that is when it reassures.
        assert!(
            render(&[b()], Machine::Untouched).contains("Nothing has been changed on this machine")
        );
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
        let out = render(&[], Machine::Untouched);
        assert!(out.contains("passed"), "got: {out}");
    }

    /// Blockers come first even when found later, because that is the order they must be dealt
    /// with, and a parent reading top-down should not start with something optional.
    #[test]
    fn blockers_are_reported_before_cautions() {
        let out = render(&[c(), b()], Machine::Untouched);
        let (ci, bi) = (out.find("Public").unwrap(), out.find("8443").unwrap());
        assert!(bi < ci, "blocker must precede caution:\n{out}");
    }

    /// The promise that nothing was touched only holds when we are refusing to proceed. Printing
    /// it on a cautions-only run would be a lie — that install goes ahead and changes plenty.
    #[test]
    fn the_nothing_changed_promise_appears_only_when_refusing() {
        assert!(render(&[b()], Machine::Untouched).contains("Nothing has been changed"));
        assert!(
            !render(&[c()], Machine::Untouched).contains("Nothing has been changed"),
            "a cautions-only install proceeds and does change things"
        );
        assert!(!render(&[], Machine::Untouched).contains("Nothing has been changed"));
    }

    /// Every finding must carry all three parts. A problem with no fix is a problem the reader
    /// is standing at a machine unable to act on.
    #[test]
    fn every_finding_shows_what_why_and_fix() {
        let out = render(&[b()], Machine::Untouched);
        assert!(out.contains("port 8443 is in use"), "what:\n{out}");
        assert!(
            out.contains("the service would exit at once"),
            "why:\n{out}"
        );
        assert!(out.contains("-> pick another"), "fix:\n{out}");
    }

    /// Fix text is often several lines with a blank one separating a pasteable command from the
    /// prose around it. A blank line must not come out as a line of spaces: invisible on screen,
    /// and noise the moment anyone quotes the output into a message or an issue.
    #[test]
    fn no_line_ever_ends_in_whitespace() {
        let f = Finding::caution(
            "something",
            "line one\n\nline three after a blank",
            "do this\n\n  some-command --flag\n\nthen that",
        );
        let out = render(&[f], Machine::Untouched);
        let bad: Vec<_> = out.lines().filter(|l| *l != l.trim_end()).collect();
        assert!(
            bad.is_empty(),
            "lines with trailing whitespace: {bad:?}\n{out}"
        );
        // And the blank lines must survive as blanks -- dropping them would run the command
        // into the prose. The command itself is indented under the fix, which is correct, so
        // look for "a blank line, then a line whose content is the command" rather than
        // hardcoding the padding.
        let lines: Vec<&str> = out.lines().collect();
        let cmd = lines
            .iter()
            .position(|l| l.trim() == "some-command --flag")
            .expect("the command line should be present");
        assert!(
            lines[cmd - 1].is_empty(),
            "a blank line should separate the command from the prose above it:\n{out}"
        );
    }

    /// A fix must be offered only where one exists, and `fixable` is what the prompt iterates.
    /// Getting this wrong in either direction is bad: a missed remedy means the parent is told to
    /// run a command we could have run, and a spurious one means prompting to fix something we
    /// cannot.
    #[test]
    fn only_findings_with_a_remedy_are_offered() {
        let manual = Finding::blocker("port busy", "would exit", "stop the other program");
        let auto = Finding::caution("network is Public", "unreachable", "set it to Private")
            .with_remedy(Remedy::MakeNetworkPrivate);

        assert_eq!(manual.remedy, None);
        assert_eq!(fixable(std::slice::from_ref(&manual)).len(), 0);
        assert_eq!(fixable(&[manual, auto.clone()]).len(), 1);
        assert_eq!(fixable(std::slice::from_ref(&auto)).len(), 1);
        assert_eq!(fixable(&[]).len(), 0);
    }

    /// No remedy must stay the default. A constructor that silently attached a real one would
    /// make the installer act on a machine without being asked.
    #[test]
    fn findings_carry_no_remedy_until_told_otherwise() {
        assert_eq!(
            Finding::caution("a", "b", "c").remedy,
            None,
            "a plain finding must never carry an automatic action"
        );
        assert_eq!(
            Finding::blocker("a", "b", "c").remedy,
            None,
            "a plain finding must never carry an automatic action"
        );
    }

    /// Every fixable finding still has to explain the manual route. The fix can be declined, can
    /// fail, and on a headless install is never offered at all -- so the words are the fallback,
    /// not decoration.
    #[test]
    fn a_fixable_finding_still_explains_the_manual_route() {
        let f = Finding::caution("network is Public", "unreachable", "set it to Private")
            .with_remedy(Remedy::MakeNetworkPrivate);
        assert!(!f.fix.trim().is_empty());
        assert!(render(&[f], Machine::Untouched).contains("set it to Private"));
    }

    #[test]
    fn counts_agree_with_singular_and_plural() {
        assert!(render(&[b()], Machine::Untouched).contains("1 thing that will stop"));
        assert!(render(&[b(), b()], Machine::Untouched).contains("2 things that will stop"));
    }
}
