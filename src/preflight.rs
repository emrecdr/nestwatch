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
/// A problem this installer can correct itself, given permission.
///
/// An enum rather than a boxed closure so the decision to offer a fix stays pure data — testable,
/// printable, and comparable — with the side effects confined to [`apply`]. Same split the
/// enforcers use, and the reason the checks above are unit-testable at all.
///
/// Deliberately narrow. A fix belongs here only when it is unambiguous, reversible, and squarely
/// within what installing this tool implies. Anything needing a judgement call — which program to
/// stop so a port frees up, whether to repair Windows — stays [`Remedy::Manual`], because
/// guessing on someone else's machine is worse than asking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Remedy {
    /// Nothing safe to do automatically; the `fix` text is the whole answer.
    Manual,
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
    /// Whether the installer can do it for them, on request.
    pub remedy: Remedy,
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
            remedy: Remedy::Manual,
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
            remedy: Remedy::Manual,
        }
    }

    /// Attach a fix the installer can apply itself.
    #[must_use]
    pub fn fixable(mut self, remedy: Remedy) -> Self {
        self.remedy = remedy;
        self
    }
}

/// Findings this installer could correct, in report order.
pub fn fixable(findings: &[Finding]) -> Vec<&Finding> {
    findings
        .iter()
        .filter(|f| f.remedy != Remedy::Manual)
        .collect()
}

/// No remedy is constructible off Windows -- every check that produces one is `cfg(windows)` --
/// so this exists only to keep the cross-platform `install` path compiling and testable.
#[cfg(not(windows))]
pub fn apply(remedy: &Remedy) -> Result<String, String> {
    match remedy {
        Remedy::Manual => Ok(String::new()),
        other => Err(format!("{other:?} is only available on Windows")),
    }
}

/// The useful part of a failed command's output, for putting in an error message.
///
/// Windows CLI tools split themselves between stdout and stderr inconsistently — `icacls` reports
/// failures on stdout, `sc` on both — so take whichever has content. Collapsed to one line
/// because it is being embedded in a sentence, and trimmed because these tools pad with blanks.
#[cfg(windows)]
pub(crate) fn tool_output(out: &std::process::Output) -> String {
    let pick = |b: &[u8]| String::from_utf8_lossy(b).trim().to_string();
    let (o, e) = (pick(&out.stdout), pick(&out.stderr));
    let text = match (o.is_empty(), e.is_empty()) {
        (false, false) => format!("{o} {e}"),
        (false, true) => o,
        (true, false) => e,
        (true, true) => return "(it printed nothing)".into(),
    };
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Carry out one remedy.
///
/// Returns the error as a `String` because every caller prints it rather than matching on it: a
/// fix that fails is reported and the install carries on, since none of these are required for a
/// correct install -- they are conveniences for problems the parent could fix by hand.
#[cfg(windows)]
pub fn apply(remedy: &Remedy) -> Result<String, String> {
    match remedy {
        Remedy::Manual => Ok(String::new()),

        // Documented as equivalent to the Unblock tick-box in the file's properties: it removes
        // the Zone.Identifier alternate data stream and nothing else. Done natively rather than
        // through `Unblock-File`, since deleting the stream IS the operation and a subprocess
        // would only add a failure mode.
        Remedy::UnblockFile(path) => {
            let ads = format!("{}:Zone.Identifier", path.display());
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
                .args([
                    "-NoProfile",
                    "-Command",
                    "Get-NetConnectionProfile | Where-Object {$_.NetworkCategory -eq 'Public'} | \
                     Set-NetConnectionProfile -NetworkCategory Private",
                ])
                .output()
                .map_err(|e| format!("could not run PowerShell: {e}"))?;
            if !out.status.success() {
                return Err(format!("PowerShell refused it: {}", tool_output(&out)));
            }
            // Confirm rather than assume: the cmdlet can exit 0 having matched nothing.
            let left = crate::doctor::network_profiles();
            if !left.is_empty() && left.iter().all(|p| p == "Public") {
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
                Err(format!("sc.exe refused it: {}", tool_output(&out)))
            }
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
            .fixable(Remedy::EnableService),
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
        )
        .fixable(Remedy::UnblockFile(exe.clone())));
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
        out.push(
            Finding::caution(
                "this PC's network is set to Public",
                "The firewall rule only applies on Private and Domain networks. On a Public one \n\
             Windows blocks every incoming connection, so the dashboard address and the QR code \n\
             will time out from every device -- even though the service is running.",
                "From this same elevated PowerShell, one line:\n\
             \n  \
             Get-NetConnectionProfile | Where-Object {$_.NetworkCategory -eq 'Public'} |\n    \
                 Set-NetConnectionProfile -NetworkCategory Private\n\
             \n\
             (That switches every Public adapter on this machine to Private, which is what you\n\
             want on a home PC. Run Get-NetConnectionProfile first if you would rather see them\n\
             and pick one by -Name.)\n\
             \n\
             Or by hand: Settings > Network & internet > (your Wi-Fi) > Network profile type.\n\
             Either way it takes effect immediately -- nothing needs reinstalling, and you do\n\
             not need to run install again.",
            )
            .fixable(Remedy::MakeNetworkPrivate),
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
        let out = render(&[f]);
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
    /// Failed tool output has to survive into the message, since for icacls and netsh it is the
    /// only description of what went wrong.
    #[cfg(windows)]
    #[test]
    fn tool_output_prefers_whichever_stream_spoke() {
        use std::os::windows::process::ExitStatusExt;
        let out = |o: &str, e: &str| std::process::Output {
            status: std::process::ExitStatus::from_raw(1),
            stdout: o.as_bytes().to_vec(),
            stderr: e.as_bytes().to_vec(),
        };
        assert_eq!(tool_output(&out("on stdout", "")), "on stdout");
        assert_eq!(tool_output(&out("", "on stderr")), "on stderr");
        assert_eq!(tool_output(&out("a", "b")), "a b");
        assert_eq!(tool_output(&out("", "")), "(it printed nothing)");
        // sc.exe pads with blank lines; the result is embedded in a sentence.
        assert_eq!(
            tool_output(&out("[SC] ChangeServiceConfig2 FAILED 1072:\n\n", "")),
            "[SC] ChangeServiceConfig2 FAILED 1072:"
        );
    }

    #[test]
    fn only_findings_with_a_remedy_are_offered() {
        let manual = Finding::blocker("port busy", "would exit", "stop the other program");
        let auto = Finding::caution("network is Public", "unreachable", "set it to Private")
            .fixable(Remedy::MakeNetworkPrivate);

        assert_eq!(manual.remedy, Remedy::Manual);
        assert_eq!(fixable(std::slice::from_ref(&manual)).len(), 0);
        assert_eq!(fixable(&[manual, auto.clone()]).len(), 1);
        assert_eq!(fixable(std::slice::from_ref(&auto)).len(), 1);
        assert_eq!(fixable(&[]).len(), 0);
    }

    /// `Manual` must stay the default. A constructor that silently attached a real remedy would
    /// make the installer act on a machine without being asked.
    #[test]
    fn findings_are_manual_until_told_otherwise() {
        assert_eq!(
            Finding::caution("a", "b", "c").remedy,
            Remedy::Manual,
            "a plain finding must never carry an automatic action"
        );
        assert_eq!(
            Finding::blocker("a", "b", "c").remedy,
            Remedy::Manual,
            "a plain finding must never carry an automatic action"
        );
    }

    /// Every fixable finding still has to explain the manual route. The fix can be declined, can
    /// fail, and on a headless install is never offered at all -- so the words are the fallback,
    /// not decoration.
    #[test]
    fn a_fixable_finding_still_explains_the_manual_route() {
        let f = Finding::caution("network is Public", "unreachable", "set it to Private")
            .fixable(Remedy::MakeNetworkPrivate);
        assert!(!f.fix.trim().is_empty());
        assert!(render(&[f]).contains("set it to Private"));
    }

    #[test]
    fn counts_agree_with_singular_and_plural() {
        assert!(render(&[b()]).contains("1 thing that will stop"));
        assert!(render(&[b(), b()]).contains("2 things that will stop"));
    }
}
