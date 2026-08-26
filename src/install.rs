//! One-time setup and teardown.
//!
//! `install` stores the password (as an Argon2 hash only), generates the TLS cert, and —
//! on Windows — copies the binary to a protected location, registers a SYSTEM service that
//! auto-starts and auto-restarts, opens a LAN-scoped firewall rule, and ACL-hardens its
//! files so a standard (non-admin) user can't stop, read, or delete it.
//!
//! Ordering matters: the data directory is created and ACL-locked **before** any secret is
//! written into it, so the TLS key / password hash are never briefly world-readable.
//!
//! Prerequisite for tamper-resistance: the child must be a **standard user**. Against a
//! local administrator no software-only measure is reliable. Run from an elevated console.

use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::auth;
use crate::config::{self, Config, DEFAULT_PORT};

// Only referenced by the Windows service/firewall code paths.
// `pub(crate)` so `doctor` checks the rule this module actually creates, rather than a second
// copy of the name that silently stops matching if this one is ever changed.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const FIREWALL_RULE: &str = "HostHealthService";

pub fn install() -> Result<()> {
    println!("== nestwatch v{} :: install ==\n", crate::VERSION);
    // Fail fast (before prompting for a password or creating anything) if we're not elevated:
    // the ACL-hardening below would otherwise lock the data dir and then be unable to write the
    // config into it, leaving a confusing half-installed state. See `ensure_elevated`.
    ensure_elevated("install", SERVICE_ELEVATION_REASON)?;

    // Say what this run is doing to what is already here, before it does it. Printed rather than
    // acted on: nothing branches on this yet, and the first thing worth knowing when a machine
    // starts misbehaving after an update is which version it came from.
    match classify_install(&read_stamp(), crate::VERSION) {
        InstallKind::Fresh => {}
        InstallKind::Reinstall => {
            println!(
                "{} is already installed; this run reinstalls it.\n",
                crate::VERSION
            )
        }
        InstallKind::Upgrade { from } => println!(
            "{from} is installed; this run updates it to {}.\n",
            crate::VERSION
        ),
        InstallKind::Downgrade { from } => println!(
            "WARNING: {from} is already installed and is NEWER than this build ({}).\n\
             Settings written by the newer version may not survive being read and rewritten by \
             this one.\n",
            crate::VERSION
        ),
        InstallKind::Unknown => println!(
            "NOTE: an install record exists but could not be read.\n\
             Continuing, but this cannot tell whether that was an older or newer build.\n"
        ),
    }

    let args: Vec<String> = std::env::args().collect();

    // Distinguish "no config yet" (a fresh install — fine) from "there IS a config and it won't
    // parse". Both used to collapse to `None`, so a corrupt file meant `..Default::default()`
    // silently reset the curfew, rules, routines and port — while the docs promise a reinstall
    // preserves them. Losing a carefully-tuned setup without being told is worse than stopping.
    let existing = match Config::load() {
        Ok(cfg) => Some(cfg),
        Err(_) if !config::data_paths().config.exists() => None,
        Err(e) => {
            let path = config::data_paths().config;
            if !args.iter().any(|a| a == "--reset-config") {
                bail!(
                    "{} exists but can't be read ({e}).\n\
                     Continuing would reset your curfew, screen-time rules and routines to their \
                     defaults.\n\
                     If that's what you want, re-run with `--reset-config`. Otherwise restore or \
                     repair that file first (you may need an elevated console to read it).",
                    path.display()
                );
            }
            println!(
                "(--reset-config: replacing the unreadable {})",
                path.display()
            );
            None
        }
    };

    // Port precedence: --port flag > existing config > default.
    let port = match parse_port_flag(&args)? {
        Some(p) => p,
        None => existing.as_ref().map(|c| c.port).unwrap_or(DEFAULT_PORT),
    };

    // Everything that must already be true, checked together before anything is touched — and
    // before the password prompt, so a machine that cannot be installed on never asks for a
    // secret first. Every step below this point changes something: it stops the running service,
    // overwrites the binary, rewrites the firewall rule.
    let mut findings = crate::preflight::gather(port);
    print!(
        "{}",
        crate::preflight::render(&findings, crate::preflight::Machine::Untouched)
    );

    // Offer to fix what we can, rather than printing a command and hoping. The first parent to
    // use this scrolled past a correct Public-network warning twice, which is the expected
    // outcome when the remedy is four levels into Settings and the install carries on regardless.
    //
    // Always asked, never assumed: these change the machine's configuration, and this runs as
    // SYSTEM. `--fix` answers yes in advance, for a headless install where nobody is at the
    // console to answer.
    if !findings.is_empty() {
        let assume_yes = args.iter().any(|a| a == "--fix");
        // Only re-check if something was actually changed. Re-rendering unconditionally printed
        // the whole report twice on the common path where nothing was fixable.
        if offer_fixes(&findings, assume_yes)? {
            findings = crate::preflight::gather(port);
            if findings.is_empty() {
                println!("All pre-flight checks pass now.\n");
            } else {
                println!("Still outstanding:");
                print!(
                    "{}",
                    crate::preflight::render(&findings, crate::preflight::Machine::Changed)
                );
            }
        }
    }

    if crate::preflight::blocked(&findings) {
        // The findings have already been printed in full, with fixes. Repeating them in the error
        // would print each one twice; this line only has to stop the run.
        bail!("pre-flight checks failed — nothing was installed.");
    }

    // Interactive by default; NESTWATCH_PASSWORD allows a silent/headless install.
    let password = match std::env::var("NESTWATCH_PASSWORD") {
        // Headless: there is nobody to re-prompt, so a bad value has to fail — but it still says
        // exactly what was wrong with it, and names the variable, because the value is invisible
        // here in a way a typed one is not.
        Ok(pw) if !pw.is_empty() => {
            if let Err(problem) = auth::check_password(&pw) {
                bail!("NESTWATCH_PASSWORD is not usable: {}", problem.message());
            }
            pw
        }
        _ => prompt_for_password()?,
    };

    let paths = config::data_paths();
    // Create + lock down the data dir BEFORE writing any secret into it.
    prepare_data_dir(&paths.dir)?;

    let cfg = Config {
        port,
        password_hash: auth::hash_password(&password)?,
        // Anchor the clock to wherever this machine is at install time, so a later timezone
        // change (which a standard user can make with no UAC prompt) can't move the day boundary
        // or the curfew window. Re-recorded on every install, so a genuine relocation is handled
        // by reinstalling.
        tz_offset_mins: Some(crate::clock::current_offset_mins()),
        // Preserve existing settings (curfew, rules, granted extra) across reinstalls.
        ..existing.unwrap_or_default()
    };
    cfg.save()?;

    // Reuse the existing certificate when it still covers this machine.
    //
    // Reissuing changes the fingerprint, which makes EVERY paired phone and laptop show the
    // "not trusted" warning again and invalidates the exception they accepted. Doing that on every
    // routine upgrade trains the parent to click through warnings without looking — the exact
    // habit the fingerprint check depends on them not having. Reissue only when the addresses
    // changed (a stale SAN would stack a name-mismatch error on top) or on `--new-cert`.
    let hosts = crate::cert::reachable_hosts();
    let force_new = args.iter().any(|a| a == "--new-cert");
    let covered = !cfg.cert_sans.is_empty() && cfg.cert_sans == hosts;
    let reuse = !force_new && covered && paths.cert.exists() && paths.key.exists();

    let fingerprint = if reuse {
        println!(
            "\nKeeping the existing certificate (already covers {}).",
            hosts.join(", ")
        );
        println!("Devices you've already paired won't warn again. Use `--new-cert` to reissue.");
        crate::cert::read_fingerprint(&paths.cert)?
    } else {
        if !cfg.cert_sans.is_empty() && !covered && !force_new {
            println!(
                "\nThis PC's address changed ({} -> {}), so a new certificate is needed.",
                cfg.cert_sans.join(", "),
                hosts.join(", ")
            );
            println!(
                "Your devices will show the trust warning once more — verify the new fingerprint below."
            );
        }
        let fp = crate::cert::generate(&paths.cert, &paths.key)?;
        // Record what the new cert covers, so the next install can make this same decision.
        let mut cfg = cfg.clone();
        cfg.cert_sans = hosts;
        cfg.save()?;
        fp
    };

    deploy(cfg.port)?;

    // After `deploy`, never before: the stamp records what is *installed*, and deploy is the step
    // that can fail and roll back to the previous binary. Writing it earlier would leave a machine
    // claiming a version it is not running. Not fatal on its own — a missing stamp costs the next
    // install its "updating X -> Y" line, which is not worth failing an otherwise good install for.
    if let Err(e) = write_stamp() {
        println!("(could not record the installed version: {e})");
    }

    println!("\nInstalled.");
    print_access_block(cfg.port);
    println!("\nTLS cert SHA-256 — verify this the first time your browser warns, so you know");
    println!("you're trusting THIS machine and not a LAN impostor:");
    println!("  {fingerprint}");
    Ok(())
}

/// Remove everything `install` put on this machine.
///
/// **Fails if anything survived.** This used to print `(could not remove ...)` and return `Ok(())`,
/// so a half-finished uninstall reported success — the worst direction to be wrong in here, because
/// the parent walks away believing the controls are gone while a service or a hooked watcher is
/// still running. Every removal is attempted, and the ones that failed are reported together at the
/// end; see [`render_leftovers`].
pub fn uninstall() -> Result<()> {
    ensure_elevated("uninstall", SERVICE_ELEVATION_REASON)?;
    // Don't leave a live pairing token behind for a service that's going away.
    crate::pairing::clear(&config::data_paths().pairing);
    let purge = std::env::args().any(|a| a == "--purge");
    let mut leftovers: Vec<Leftover> = Vec::new();

    remove_service(&mut leftovers)?;

    if purge {
        let dir = config::data_paths().dir;
        let removed = std::fs::remove_dir_all(&dir);
        if removed.is_ok() {
            println!("Purged config/cert at {}", dir.display());
        }
        leftovers.extend(Leftover::removal("the data directory", &dir, removed));
    } else {
        // The stamp records which build was installed, so it belongs to the installation rather
        // than to the parent's settings — it goes even when the settings stay. Leaving it would
        // tell the next install it is upgrading from a version that is no longer on the machine.
        let stamp = stamp_path();
        leftovers.extend(Leftover::removal(
            "the installed-version record",
            &stamp,
            std::fs::remove_file(&stamp),
        ));
        println!(
            "Config/cert left in {} (use `uninstall --purge` to remove).",
            config::data_paths().dir.display()
        );
    }

    if !leftovers.is_empty() {
        bail!("{}", render_leftovers(&leftovers));
    }
    println!("\nUninstalled.");
    Ok(())
}

/// Print everything the parent needs to actually reach the dashboard: a scannable pairing QR,
/// the real URLs, and the child's `/ask` link.
///
/// Shared by `install` and `nestwatch pair`. Addresses come from [`crate::cert::reachable_hosts`]
/// — the same list used for the certificate SANs — so the address we advertise is always one the
/// cert covers, and the parent never gets a name-mismatch error stacked on the trust warning.
///
/// Best-effort throughout: a machine that's offline, or a QR that won't render, degrades to
/// printing plain text. Nothing here is worth failing an install over.
pub fn print_access_block(port: u16) {
    let hosts = crate::cert::reachable_hosts();
    let Some((primary, rest)) = hosts.split_first() else {
        println!(
            "\nCouldn't detect this PC's network address — is it offline? Once it's on the home\n\
             Wi-Fi, run `ipconfig`, then browse to https://<that-address>:{port}"
        );
        return;
    };

    // A pairing token turns "type an IP and a passphrase on a phone" into "point the camera".
    match crate::pairing::mint(&config::data_paths().pairing) {
        Ok(token) => {
            // Read back off disk rather than threaded down from `generate`, because this block
            // is shared with `nestwatch pair`, where the cert may be months old and no
            // fingerprint is in hand. `.ok()` on purpose: an unreadable cert costs the QR its
            // fragment — a client falls back to comparing the fingerprint by eye — and that is
            // strictly better than failing an install over a QR decoration.
            let fingerprint = crate::cert::read_fingerprint(&config::data_paths().cert).ok();
            let scanned = crate::pairing::pair_url(primary, port, &token, fingerprint.as_deref());
            // The fingerprint goes in the QR and NOT in the line underneath it, because the two
            // are read by different things. Only a client that scans can use it; anyone reading
            // this line is going to type it into a browser, which cannot check a fingerprint and
            // would just be handed 95 characters of noise. Measured, printing the pinned form
            // here takes the line from 46 columns to 145 — it wraps on an 80-column console,
            // which is the format a person copies from.
            let typed = crate::pairing::pair_url(primary, port, &token, None);
            println!("\nScan this with your phone's camera — it opens the dashboard, signed in:");
            match crate::pairing::qr_code(&scanned) {
                Some(qr) => println!("\n{qr}"),
                None => println!(),
            }
            println!("  {typed}");
            println!(
                "  (valid {} minutes, one use — run `nestwatch pair` for a new one)",
                crate::pairing::TTL_SECS / 60
            );
            // Not "the password you just set" — `nestwatch pair` shares this block, and there
            // the password may be months old.
            println!("\nOr open it and sign in with your password:");
        }
        Err(e) => {
            // Visible, not debug-level: a `pair` that silently prints no QR looks broken.
            println!("\n(Couldn't create a pairing code: {e})");
            println!(
                "Open this on your phone or laptop (same Wi-Fi) and sign in with your password:"
            );
        }
    }

    println!("  https://{primary}:{port}");
    for host in rest {
        println!("  https://{host}:{port}   (also works)");
    }
    println!("\nYour child asks for more time at:");
    println!("  https://{primary}:{port}/ask");
}

/// Parse an optional `--port <N>` from argv.
fn parse_port_flag(args: &[String]) -> Result<Option<u16>> {
    if let Some(i) = args.iter().position(|a| a == "--port") {
        let raw = args.get(i + 1).context("--port requires a value")?;
        let port: u16 = raw.parse().context("--port must be 1..=65535")?;
        if port == 0 {
            bail!("--port must be 1..=65535");
        }
        return Ok(Some(port));
    }
    Ok(None)
}

/// Whether this process holds an elevated token.
///
/// Also used by `doctor`, which must not mistake "the data folder is locked to Administrators"
/// for "nothing is installed".
///
/// Returns `false` if the query itself fails — callers treat that as "assume not elevated",
/// which is the safe direction for both a refusal and a diagnostic.
#[cfg(windows)]
pub fn is_elevated() -> bool {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // SAFETY: Win32 token FFI; the process-token handle is closed on every path.
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut ret_len = 0u32;
        let info = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut core::ffi::c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret_len,
        );
        let _ = CloseHandle(token);
        info.is_ok() && elevation.TokenIsElevated != 0
    }
}

/// On non-Windows (dev builds) there is no elevation concept; behave as if we have it so the
/// dev `doctor` reports on the real state of the local data dir.
#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    true
}

/// Why `install`/`uninstall` need elevation, appended to the refusal message.
pub const SERVICE_ELEVATION_REASON: &str = "It registers a SYSTEM service, edits the firewall, and locks its data directory to \
     Administrators — all of which require elevation. Also confirm your account is an \
     administrator: `net localgroup Administrators`.";

/// Refuse to run `action` from a non-elevated console, explaining `why`.
///
/// Under UAC an administrator account in an ordinary console holds a *filtered* token whose
/// Administrators SID is deny-only. For `install` that meant it could create the data dir but
/// then not write into it once ACL-locked — a confusing "Access is denied" **after** a
/// half-hardened directory already existed. Checking up front turns that into one clear message
/// and guarantees no partially-installed state is left behind.
///
/// No `#[cfg]` split needed: [`is_elevated`] already returns `true` on non-Windows, so this is
/// an unconditional `Ok` there. One platform seam for the whole concept instead of two.
pub fn ensure_elevated(action: &str, why: &str) -> Result<()> {
    if is_elevated() {
        return Ok(());
    }
    bail!(
        "`nestwatch {action}` must be run from an elevated console.\n\
         Right-click PowerShell or Command Prompt, choose \"Run as administrator\", and run it \
         again.\n({why})"
    )
}

// ---------------------------------------------------------------------------
// Windows: install/protect the SYSTEM service
// ---------------------------------------------------------------------------

/// Filename the binary is installed and registered under (low-profile, matches the service).
// Named cross-platform, like FIREWALL_RULE above, so a test can pin it against the
// documentation that spells it out by hand. Renaming it would otherwise break the
// documented remote-update sequence with nothing failing.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const INSTALL_EXE_NAME: &str = "host-health.exe";

/// Offer to apply the fixes we can, one at a time.
///
/// One prompt per problem rather than one for all of them: they are unrelated, and a parent who
/// wants the network changed does not necessarily want a service re-enabled in the same breath.
///
/// Defaults to NO on every prompt. These alter the machine's configuration from a process running
/// with full privileges, so silence must mean "don't".
/// Returns whether anything was actually changed, so the caller knows whether re-checking is
/// worth doing -- and, more importantly, worth *printing*.
fn offer_fixes(findings: &[crate::preflight::Finding], assume_yes: bool) -> Result<bool> {
    use std::io::Write as _;

    let fixable = crate::preflight::fixable(findings);
    if fixable.is_empty() {
        return Ok(false);
    }
    let mut changed = false;

    println!(
        "{} of these can be fixed from here.\n",
        if fixable.len() == 1 { "One" } else { "Some" }
    );

    for f in fixable {
        let yes = if assume_yes {
            println!("  {} — fixing (--fix)", f.what);
            true
        } else {
            print!("  Fix \"{}\" now? [y/N] ", f.what);
            std::io::stdout().flush().ok();
            let mut line = String::new();
            // A closed stdin (piped install, no console) reads as empty, which is "no" -- the
            // safe direction, and the same as pressing Enter.
            if std::io::stdin().read_line(&mut line).is_err() {
                line.clear();
            }
            matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
        };

        if !yes {
            println!("      skipped — the instructions above still apply.");
            continue;
        }
        // `fixable` selected these, so every one carries a remedy; the type says so too.
        let Some(remedy) = &f.remedy else { continue };
        match crate::preflight::apply(remedy) {
            Ok(msg) => {
                println!("      done: {msg}");
                changed = true;
            }
            // A failed fix is not a failed install: every one of these is optional, and the
            // written instructions are still there. Report and carry on.
            Err(e) => println!("      could not: {e}\n      Do it by hand as described above."),
        }
    }
    println!();
    Ok(changed)
}

/// Ask for the password, and keep asking.
///
/// This used to `bail!` on the first mismatch or short entry, which aborted the whole install
/// over a typo in the confirmation — after which the parent re-runs an elevated command and
/// starts again. Nothing has been written to disk at this point, so re-prompting is free.
///
/// Every rejection says what was measured, not only what was required. The report that prompted
/// this was "I am entering 10 characters and it says I need 10": a message that only restates the
/// rule cannot resolve that, and a message carrying the count resolves it immediately.
fn prompt_for_password() -> Result<String> {
    const TRIES: u32 = 5;
    for attempt in 1..=TRIES {
        let pw = rpassword::prompt_password("Set a control password: ")?;
        let confirm = rpassword::prompt_password("Confirm password:      ")?;

        let complaint = if pw != confirm {
            Some(auth::describe_mismatch(&pw, &confirm))
        } else {
            auth::check_password(&pw).err().map(|p| p.message())
        };

        match complaint {
            None => {
                if let Some(note) = auth::password_caution(&pw) {
                    println!("{note}");
                }
                return Ok(pw);
            }
            Some(msg) if attempt < TRIES => {
                println!("\n{msg}\nLet's try again ({} left).\n", TRIES - attempt);
            }
            Some(msg) => bail!("{msg}\n\nNo attempts left — run `install` again when ready."),
        }
    }
    unreachable!("loop returns or bails on the final attempt")
}

#[cfg(windows)]
fn install_dir() -> std::path::PathBuf {
    use std::path::PathBuf;
    std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"))
        .join("HostHealth")
}

/// Create the data dir and restrict it to SYSTEM + Administrators before secrets land in it.
#[cfg(windows)]
fn prepare_data_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    harden_acl(dir)
}

#[cfg(not(windows))]
fn prepare_data_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))
}

/// Rights on the service handle the installer works through.
///
/// Named, and pinned by a test, because a missing right here does not fail where it is granted —
/// it fails much later in whichever call needed it, as an opaque winapi error. This has happened
/// twice. `DELETE` was missing once, so the rollback silently could not delete and "rolled back"
/// was a lie. Then `QUERY_STATUS` was missing from the create path only, so `start_and_verify`
/// could never read the status of a *freshly installed* service: every poll returned
/// access-denied for the full timeout, and the installer rolled back services that had started
/// perfectly well. Updates were unaffected, which is why it survived — [`UPDATE_ACCESS`] always
/// had the right.
///
/// The two masks differ only where they must: an update needs `STOP` (to release the locked exe)
/// and a new install needs `DELETE` (to roll itself back).
#[cfg(windows)]
const CREATE_ACCESS: windows_service::service::ServiceAccess = {
    use windows_service::service::ServiceAccess as A;
    A::QUERY_STATUS
        .union(A::CHANGE_CONFIG)
        .union(A::START)
        .union(A::DELETE)
};

/// Rights used when an existing service is being upgraded in place.
#[cfg(windows)]
const UPDATE_ACCESS: windows_service::service::ServiceAccess = {
    use windows_service::service::ServiceAccess as A;
    A::QUERY_STATUS
        .union(A::STOP)
        .union(A::START)
        .union(A::CHANGE_CONFIG)
};

#[cfg(windows)]
fn deploy(port: u16) -> Result<()> {
    use std::ffi::{OsStr, OsString};

    use windows_service::service::{
        ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceType,
    };
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    use crate::service::{SERVICE_DESCRIPTION, SERVICE_DISPLAY_NAME, SERVICE_NAME};

    let dir = install_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let target_exe = dir.join(INSTALL_EXE_NAME);
    let current_exe = std::env::current_exe()?;

    let manager = ServiceManager::local_computer(
        None::<&OsStr>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )?;

    // If the service already exists, this is an update: stop it (to release the locked exe),
    // overwrite the binary, and reuse the registration.
    let existing = manager.open_service(SERVICE_NAME, UPDATE_ACCESS).ok();
    if let Some(svc) = &existing {
        stop_and_wait(svc)?;
    }

    // The service is stopped, but its resident watchers are not: `run_watcher_supervisor` is a
    // detached thread that dies with the service process without terminating the helpers it
    // spawned, and a helper ignores the resulting broken pipe. Those helpers are running *this*
    // binary, and Windows keeps a running executable's image open — so the copy below failed with a
    // sharing violation whenever anyone was signed in. The rollback then correctly restarted the old
    // service, and the update simply never applied. Killing them here is what makes an upgrade
    // possible on a machine that is actually in use.
    //
    // After the stop and before the copy, deliberately: killing them earlier lets the still-running
    // supervisor spawn replacements, and killing them later is too late for the copy.
    terminate_resident_helpers();

    // Everything from the stop above until the service is started again is a window where an
    // upgrade has enforcement switched OFF. Any `?` in here used to abort the install and leave
    // it that way until the next reboot — with a failed binary copy (antivirus holding the file,
    // or the lingering helper handled just above) being the likely trigger. Do the fallible work
    // first, then put the old service back if it didn't work out.
    let staged = (|| -> Result<()> {
        if current_exe != target_exe {
            std::fs::copy(&current_exe, &target_exe)
                .with_context(|| format!("copying binary to {}", target_exe.display()))?;
        }
        // Users get read+execute so the child's session can run the screenshot helper.
        harden_program_dir(&dir)
    })();

    if let Err(e) = staged {
        if let Some(svc) = &existing {
            match svc.start(&[] as &[&OsStr]) {
                Ok(()) => println!(
                    "\nUpdate failed — restarted the previous version, so screen-time limits and \
                     curfew keep working."
                ),
                Err(restart) => println!(
                    "\nWARNING: the update failed AND the previous service could not be restarted \
                     ({restart}).\nEnforcement is OFF until you re-run install or reboot."
                ),
            }
        }
        return Err(e);
    }

    configure_firewall(port)?;

    // Applied on BOTH paths -- but on each one only *after* there is a service to apply it to.
    //
    // This used to run once here, before the match. On an upgrade that is fine, the service
    // already exists. On a first install there is nothing registered yet, so both `sc` calls
    // failed with 1060 (service does not exist), and the call inside the None arm below then
    // quietly did the real work. Harmless, and invisible, until failures started being reported
    // properly -- at which point a successful install printed two alarming notes about settings
    // that had in fact been applied a moment later.
    match existing {
        Some(svc) => {
            configure_recovery();
            start_and_verify(&svc, port).context("restarting the updated service")?;
            println!("Updated and restarted service '{SERVICE_NAME}'.");
        }
        None => {
            let info = ServiceInfo {
                name: OsString::from(SERVICE_NAME),
                display_name: OsString::from(SERVICE_DISPLAY_NAME),
                service_type: ServiceType::OWN_PROCESS,
                start_type: ServiceStartType::AutoStart,
                error_control: ServiceErrorControl::Normal,
                executable_path: target_exe,
                launch_arguments: vec![OsString::from("service-run")],
                dependencies: vec![],
                account_name: None, // LocalSystem
                account_password: None,
            };
            let service = manager.create_service(&info, CREATE_ACCESS).map_err(|e| {
                // Every code is decoded now, so 1072 no longer needs its own branch --
                // matching on the *text* of an error message was fragile anyway.
                anyhow::anyhow!(
                    "could not register the service.\n  {}",
                    describe_service_error(&e)
                )
            })?;
            let _ = service.set_description(SERVICE_DESCRIPTION);
            configure_recovery();
            if let Err(e) = start_and_verify(&service, port) {
                // Don't leave a registered-but-dead service behind. The handle must carry DELETE
                // for this to work — without it `delete()` fails with access-denied, the error
                // was swallowed, and the message below ("rolled back") was simply untrue.
                if let Err(del) = service.delete() {
                    // Printed, not logged: this contradicts the "rolled back" in the error about
                    // to be returned, and a tracing warning is invisible in a console install.
                    println!(
                        "\nWARNING: could not remove the service that was just created --\n  {}\n\
                         It is registered but not running. `sc delete {}` removes it, and a \
                         reboot clears a pending deletion.",
                        describe_service_error(&del),
                        crate::service::SERVICE_NAME,
                    );
                }
                return Err(e).context("starting the new service (rolled back)");
            }
            println!("Installed service '{SERVICE_NAME}' (LocalSystem, auto-start/restart).");
        }
    }

    println!("Binary: {}", dir.join(INSTALL_EXE_NAME).display());
    println!("Reminder: this resists a STANDARD user — ensure your son is not an administrator.");
    Ok(())
}

/// How long to watch a freshly started service before believing it.
///
/// 30 seconds, matching the SCM's own default start timeout (`ServicesPipeTimeout`), so the
/// installer never gives up on a service Windows itself is still willing to wait for. It was 6,
/// which is under the time Defender can spend scanning a freshly written, unsigned executable on
/// its first launch — the installer would roll back a service that was about to come up fine.
#[cfg(windows)]
const VERIFY_POLLS: u32 = 60;
#[cfg(windows)]
const VERIFY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// Start a service and confirm it is still running a moment later.
///
/// `StartServiceW` returns as soon as the SCM *accepts* the request, so a successful `start()`
/// proves nothing about whether the process stayed up. A service that fails to bind its port —
/// easily the most likely real failure, since something else may already hold 8443 — dies within
/// milliseconds, and the installer would still print "Installed", leaving the parent with a dead
/// dashboard, no enforcement, and no error to search for.
#[cfg(windows)]
fn start_and_verify(service: &windows_service::service::Service, port: u16) -> Result<()> {
    use std::ffi::OsStr;

    use windows_service::service::ServiceState;

    service.start(&[] as &[&OsStr]).map_err(|e| {
        anyhow::anyhow!(
            "Windows refused to start the service.\n  {}",
            describe_service_error(&e)
        )
    })?;

    let waited = VERIFY_INTERVAL * VERIFY_POLLS;
    let logs = config::data_paths().dir.display().to_string();

    // Remember the last thing we actually saw. On timeout this is the whole diagnosis: still
    // StartPending means the process never finished handing control to `service_main`, while a
    // query that keeps erroring means we could not read the service at all. Reporting neither —
    // which is what "did not reach a running state" did — leaves nothing to act on.
    let mut running_streak = 0;
    let mut last_seen = String::from("no status reported at all");

    for _ in 0..VERIFY_POLLS {
        std::thread::sleep(VERIFY_INTERVAL);
        match service.query_status() {
            Ok(s) if s.current_state == ServiceState::Running => {
                running_streak += 1;
                // Two consecutive Running readings — it survived past its own startup.
                if running_streak >= 2 {
                    return Ok(());
                }
            }
            Ok(s) if s.current_state == ServiceState::Stopped => {
                bail!(
                    "the service started and then stopped immediately{}.\n\n\
                     Most likely: port {port} is already used by another program.\n  \
                     check:   netstat -ano | findstr :{port}\n  \
                     or use:  install --port <other-port>\n\n\
                     The reason it stopped is in the newest service.<date>.log in\n  \
                     {logs}\n\
                     (readable as Administrator).",
                    exit_detail(&s.exit_code),
                );
            }
            // StartPending or a transient query failure — keep watching, but remember which.
            Ok(s) => {
                running_streak = 0;
                last_seen = format!("{:?}", s.current_state);
            }
            Err(e) => {
                running_streak = 0;
                last_seen = format!(
                    "the status could not be read --\n     {}",
                    describe_service_error(&e)
                );
            }
        }
    }

    bail!(
        "the service did not start within {}s.\n\n\
         Last state seen: {last_seen}\n\n\
         It never reported Running, and it never reported Stopped either — so it is not the \
         port being busy (that shows up as an immediate stop). The service reports Running \
         before it reads any configuration, so this usually means the process could not be \
         launched at all.\n\n\
         Most likely causes, in order:\n  \
         1. Antivirus is holding or blocking the freshly written executable. Check your \
            protection history for host-health.exe, and allow it.\n  \
         2. The file was still marked as downloaded-from-the-internet. Right-click the original \
            .exe -> Properties -> tick Unblock, then install again.\n\n\
         What to look at:\n  \
         sc query {}\n  \
         Event Viewer -> Windows Logs -> System, filter Source = Service Control Manager\n  \
         the newest service.<date>.log in {logs}\n\n\
         Nothing was left installed -- this rolled back, so it is safe to fix and re-run.",
        waited.as_secs(),
        crate::service::SERVICE_NAME,
    )
}

/// Translate a Windows service error into something that identifies the problem.
///
/// `windows_service::Error`'s own message is the text of its doc comment — "IO error in winapi
/// call" — which discards the `io::Error` it is carrying and with it the Win32 code that says
/// what actually went wrong. That message cost a real install: the code underneath was 5,
/// access-denied, because the service handle had been created without `QUERY_STATUS`, and
/// nothing on screen said so. The OS knew; we threw it away.
#[cfg(windows)]
fn describe_service_error(err: &windows_service::Error) -> String {
    let windows_service::Error::Winapi(io) = err else {
        return err.to_string();
    };
    let Some(code) = io.raw_os_error() else {
        return io.to_string();
    };

    // Only the codes a service install can realistically produce. An unknown code still reports
    // its number and the OS's own text, which is strictly more than the old message gave.
    let explain = match code {
        2 => Some((
            "ERROR_FILE_NOT_FOUND",
            "Windows could not find the program the service points at. The registered path may be wrong, or the file was removed or quarantined after it was registered.",
        )),
        5 => Some((
            "ERROR_ACCESS_DENIED",
            "Refused for lack of permission. Either this console is not elevated, or the handle the installer is using was opened without the right it needs for this call.",
        )),
        193 | 216 => Some((
            "ERROR_BAD_EXE_FORMAT",
            "The executable is not a program this machine can run — a 32/64-bit mismatch, or a truncated or corrupted download.",
        )),
        1053 => Some((
            "ERROR_SERVICE_REQUEST_TIMEOUT",
            "The service did not report back in the time Windows allows. Usually the process could not launch at all: antivirus holding the file, or a missing dependency.",
        )),
        1056 => Some((
            "ERROR_SERVICE_ALREADY_RUNNING",
            "It is already running. Nothing is wrong, but this install did not start it.",
        )),
        1058 => Some((
            "ERROR_SERVICE_DISABLED",
            "The service exists but its start type is Disabled, so Windows refuses to start it. This normally follows a half-finished removal. Re-running install after a reboot clears it; `sc config HostHealthService start= auto` fixes it in place.",
        )),
        1060 => Some((
            "ERROR_SERVICE_DOES_NOT_EXIST",
            "No service by that name is registered. Expected during a first install; a problem anywhere else.",
        )),
        1062 => Some(("ERROR_SERVICE_NOT_ACTIVE", "The service is not running.")),
        1069 => Some((
            "ERROR_SERVICE_LOGON_FAILED",
            "Windows could not start it under its configured account.",
        )),
        1072 => Some((
            "ERROR_SERVICE_MARKED_FOR_DELETE",
            "A previous copy is still being deleted, and will not finish while anything holds a handle to it. Close the Services window and Task Manager, wait a few seconds, and run install again. A reboot always clears it.",
        )),
        1073 => Some((
            "ERROR_SERVICE_EXISTS",
            "A service with this name is already registered.",
        )),
        1077 => Some((
            "ERROR_SERVICE_NEVER_STARTED",
            "Windows has not attempted to start it since the last boot.",
        )),
        _ => None,
    };

    match explain {
        Some((name, meaning)) => {
            format!("Windows error {code} ({name})\n {meaning}\n Windows says: {io}")
        }
        None => format!("Windows error {code}\n Windows says: {io}"),
    }
}

/// Render a service exit code as something worth printing, or nothing.
///
/// A zero Win32 code carries no information and reads as noise beside "stopped immediately";
/// anything else is the single most useful number in the whole failure.
#[cfg(windows)]
fn exit_detail(code: &windows_service::service::ServiceExitCode) -> String {
    use windows_service::service::ServiceExitCode;
    match code {
        ServiceExitCode::Win32(0) => String::new(),
        ServiceExitCode::Win32(c) => format!(" (Windows error {c})"),
        // `service.rs` reports ServiceSpecific(1) when it dies from an error rather than a
        // requested stop, so this arm means the app itself failed and logged why.
        ServiceExitCode::ServiceSpecific(c) => {
            format!(" (the app exited with its own error code {c}, which it will have logged)")
        }
    }
}

/// Stop a service and wait until it reports Stopped (so its exe file is released).
#[cfg(windows)]
fn stop_and_wait(service: &windows_service::service::Service) -> Result<()> {
    use windows_service::service::ServiceState;

    // Ignore "not started" errors.
    let _ = service.stop();
    for _ in 0..50 {
        match service.query_status() {
            Ok(status) if status.current_state == ServiceState::Stopped => return Ok(()),
            Ok(_) => std::thread::sleep(std::time::Duration::from_millis(200)),
            Err(_) => return Ok(()), // gone / inaccessible — treat as stopped
        }
    }
    bail!("service did not stop within 10s")
}

// Well-known SIDs (locale-independent — "Administrators"/"Users" are localized names).
#[cfg(windows)]
const SID_SYSTEM: &str = "*S-1-5-18";
#[cfg(windows)]
const SID_ADMINS: &str = "*S-1-5-32-544";
#[cfg(windows)]
const SID_USERS: &str = "*S-1-5-32-545";

/// Lock the **data** dir (password hash, TLS key) to SYSTEM + Administrators only — a
/// standard user gets no access at all. Checked; the tamper model depends on it.
#[cfg(windows)]
fn harden_acl(path: &Path) -> Result<()> {
    run_icacls(
        path,
        &[
            &format!("{SID_SYSTEM}:(OI)(CI)F"),
            &format!("{SID_ADMINS}:(OI)(CI)F"),
        ],
    )
}

/// Lock the **program** dir (the binary) to SYSTEM + Administrators full, plus Users
/// read+execute — the child can't modify/delete the binary, but CAN execute it, which is
/// required because the service launches the screenshot helper as the child via
/// CreateProcessAsUserW (that access check uses the child's token).
#[cfg(windows)]
fn harden_program_dir(path: &Path) -> Result<()> {
    run_icacls(
        path,
        &[
            &format!("{SID_SYSTEM}:(OI)(CI)F"),
            &format!("{SID_ADMINS}:(OI)(CI)F"),
            &format!("{SID_USERS}:(OI)(CI)RX"),
        ],
    )
}

#[cfg(windows)]
fn run_icacls(path: &Path, grants: &[&str]) -> Result<()> {
    let mut cmd = std::process::Command::new(crate::syspath::system32("icacls.exe"));
    cmd.arg(path).arg("/inheritance:r");
    for grant in grants {
        cmd.arg("/grant:r").arg(grant);
    }
    // `.output()` rather than `.status()`: icacls prints "processed file: ..." and
    // "Successfully processed 1 files" on every run. That is its progress, not ours, and it made
    // a normal install look like it was reporting on something. Kept and shown only on failure,
    // where it is the only description of what went wrong.
    let out = cmd.output().context("running icacls")?;
    if !out.status.success() {
        bail!(
            "could not lock down {} -- refusing to continue, because the password hash, TLS key \
             and logs would be readable by any user.\n  icacls exited {}\n  {}",
            path.display(),
            out.status,
            crate::syspath::tool_output(&out),
        );
    }
    Ok(())
}

/// Delete the app's firewall rule if present (best-effort; used on (re)install and uninstall).
#[cfg(windows)]
fn delete_firewall_rule() {
    // Captured, not inherited: netsh prints "Deleted 1 rule(s)." or "No rules match the
    // specified criteria." and both are noise during a reinstall. A failure here is genuinely
    // fine -- the rule is about to be recreated -- so nothing is reported either way.
    let _ = std::process::Command::new(crate::syspath::system32("netsh.exe"))
        .args(["advfirewall", "firewall", "delete", "rule"])
        .arg(format!("name={FIREWALL_RULE}"))
        .output();
}

/// Recreate an inbound TCP rule scoped to the local subnet on Private/Domain networks, then
/// read it back to confirm it applied. Non-fatal: the app-layer LAN allowlist
/// (`security::require_lan_peer`) is the actual guarantee that off-LAN clients are rejected, so
/// a firewall hiccup degrades defense-in-depth but never leaves the controls exposed. We still
/// warn loudly so the parent can fix it.
#[cfg(windows)]
fn configure_firewall(port: u16) -> Result<()> {
    use std::process::Command;

    // Idempotent: delete any stale rule (possibly on an old port) first.
    delete_firewall_rule();

    let added = Command::new(crate::syspath::system32("netsh.exe"))
        .args(["advfirewall", "firewall", "add", "rule"])
        .arg(format!("name={FIREWALL_RULE}"))
        .args(["dir=in", "action=allow", "protocol=TCP"])
        .arg(format!("localport={port}"))
        .args(["profile=private,domain", "remoteip=LocalSubnet"])
        .output()
        .context("running netsh")?;
    if !added.status.success() {
        println!(
            "WARNING: could not add the firewall rule -- netsh exited {} and said:\n  {}\n\
             The app-layer LAN allowlist still applies, so this is not a security hole, but \
             other devices may not be able to reach the dashboard.",
            added.status,
            crate::syspath::tool_output(&added),
        );
        return Ok(());
    }

    if !firewall_rule_is_subnet_scoped() {
        println!(
            "WARNING: firewall rule '{FIREWALL_RULE}' did not read back with a LocalSubnet \
             scope; verify it in 'Windows Defender Firewall with Advanced Security'."
        );
    }
    Ok(())
}

/// Read the firewall rule back and confirm it carries the LocalSubnet scope.
///
/// `LocalSubnet` is a value token, not a localized label, so this is locale-independent. Shared
/// with `doctor` so the installer's own verification and the diagnostic can't disagree about what
/// a correct rule looks like — or about what it's called.
#[cfg(windows)]
pub(crate) fn firewall_rule_is_subnet_scoped() -> bool {
    let shown = show_firewall_rule();
    // Both conditions: a rule disabled in the firewall GUI still reads back with its LocalSubnet
    // scope intact, so scope alone reported a healthy rule while inbound traffic was blocked —
    // a false OK for the single most common "I can't connect from my phone" cause. `Enabled` and
    // `LocalSubnet` are value tokens, not localized labels, so this stays locale-independent.
    matches!(shown, Ok(out) if {
        let text = String::from_utf8_lossy(&out.stdout);
        text.contains("LocalSubnet") && text.contains("Yes")
    })
}

/// Auto-restart on failure (best-effort; three attempts, 5s apart, daily reset).
///
/// Also sets the `failureflag`, without which Windows runs these actions **only** when the
/// process dies without reporting Stopped — not when it reports Stopped with an error code, which
/// is what a clean-but-failed startup does. Called on every install, not just the first: an
/// upgrade previously left an existing service with whatever recovery config it already had.
#[cfg(windows)]
fn configure_recovery() {
    use crate::service::SERVICE_NAME;

    // Both of these used `.status()`, so sc.exe printed straight to the console -- including
    // "[SC] ChangeServiceConfig2 FAILED 1072" on a real install, which the installer then
    // ignored and carried on. An alarming line that the program itself disregards is worse than
    // either reporting it or not running it. Capture, and say plainly what a failure costs.
    let run = |args: &[&str], what: &str| match std::process::Command::new(
        crate::syspath::system32("sc.exe"),
    )
    .args(args)
    .output()
    {
        Ok(out) if out.status.success() => {}
        Ok(out) => println!(
            "\nNote: could not {what}.\n  sc.exe exited {}\n  {}\n  \
                 The service still installs and runs; it just will not restart itself \
                 automatically if it dies. Re-running install once the cause is fixed sets it.",
            out.status,
            crate::syspath::tool_output(&out),
        ),
        Err(e) => println!("\nNote: could not run sc.exe to {what} ({e})."),
    };

    run(
        &[
            "failure",
            SERVICE_NAME,
            "reset=",
            "86400",
            "actions=",
            "restart/5000/restart/5000/restart/5000",
        ],
        "set the service to restart itself after a failure",
    );
    // Restart on a non-zero exit code too, not only on an unreported death.
    run(
        &["failureflag", SERVICE_NAME, "1"],
        "set the service to restart after an error exit as well as a crash",
    );
}

/// The full image path of a running process, or `None` if it cannot be asked.
///
/// `PROCESS_QUERY_LIMITED_INFORMATION`, not `PROCESS_QUERY_INFORMATION`: the wider right fails
/// against a process running at a different integrity level, and a helper we cannot name is one we
/// would leave running — which is the whole failure being fixed.
///
/// Shared with [`crate::watcher::process_name`], which wants the same pid→path chain and then only
/// the file name. It lives here rather than there because `install` is compiled on every platform
/// while `watcher` is `#[cfg(windows)]`, so this direction is the only one that links.
#[cfg(windows)]
pub(crate) fn process_image_path(pid: u32) -> Option<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::core::PWSTR;

    // SAFETY: Win32 process FFI. The handle is closed on every path; `buf`/`len` are owned by this
    // frame and `len` is initialised to the buffer's true capacity as the API requires.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let path = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .ok()
        .map(|()| String::from_utf16_lossy(&buf[..len as usize]));
        let _ = CloseHandle(handle);
        path
    }
}

/// Every running process whose executable *file name* matches, paired with its full image path.
///
/// Name first, path second, and the order is the point: the snapshot gives names for free, so only
/// the one or two processes that could possibly be ours pay for an `OpenProcess`. The name is a
/// filter, never the decision — [`helpers_to_terminate`] makes that on the full path.
#[cfg(windows)]
fn processes_named(name: &str) -> Vec<(u32, String)> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    let mut found = Vec::new();
    // SAFETY: Win32 snapshot FFI. `entry.dwSize` is set before the first call as the API requires;
    // the snapshot handle is closed on the way out.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return found;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let exe = exe_name_from_buf(&entry.szExeFile);
                if exe.eq_ignore_ascii_case(name)
                    && let Some(path) = process_image_path(entry.th32ProcessID)
                {
                    found.push((entry.th32ProcessID, path));
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    found
}

/// Kill `pid` and wait for it to actually be gone.
///
/// The wait is not optional. `TerminateProcess` only *initiates* termination, and the caller's next
/// move is to overwrite or delete the executable that process has open — so returning before the
/// kernel has torn the process down would reintroduce the exact sharing violation this exists to
/// prevent, intermittently and only on slower machines.
#[cfg(windows)]
fn terminate_and_wait(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
    };

    // SAFETY: Win32 process FFI. The handle is closed on both paths.
    unsafe {
        // `PROCESS_SYNCHRONIZE` as well as `PROCESS_TERMINATE`: without it the handle cannot be
        // waited on, and the wait below is the part that makes this safe to follow with a file
        // operation on the terminated process's image.
        let Ok(handle) = OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, false, pid) else {
            return false;
        };
        let asked = TerminateProcess(handle, 0).is_ok();
        // 5s: generous for a process being torn down, and bounded so a wedged one cannot hang an
        // install. A timeout is reported as failure, which surfaces as a leftover rather than as a
        // silently broken upgrade.
        let gone = asked && WaitForSingleObject(handle, 5_000) == WAIT_OBJECT_0;
        let _ = CloseHandle(handle);
        gone
    }
}

/// Terminate every resident watcher still running the installed binary.
///
/// Called before the binary is overwritten (upgrade) and before it is deleted (uninstall), because
/// Windows holds a running executable's image open and both operations fail against it. See
/// [`helpers_to_terminate`] for why the helper is there to begin with and why it does not leave on
/// its own.
///
/// Reports its own result rather than returning a count for each caller to phrase. Both call sites
/// printed the same event under two different names — "resident watcher" in one, "helper" in the
/// other — which is one program describing one action to one person in two vocabularies.
#[cfg(windows)]
fn terminate_resident_helpers() {
    let target = install_dir().join(INSTALL_EXE_NAME);
    let candidates = processes_named(INSTALL_EXE_NAME);
    let killed = helpers_to_terminate(&candidates, &target, std::process::id())
        .into_iter()
        .filter(|pid| terminate_and_wait(*pid))
        .count();
    if killed > 0 {
        println!("Stopped {killed} helper process(es) holding the binary open.");
    }
}

/// Remove the service, the firewall rule and the installed binary, recording anything that
/// survived instead of stopping at the first failure.
///
/// **Nothing here short-circuits.** An uninstall that cannot delete the service should still take
/// the firewall rule and the binary with it: stopping at the first problem leaves more behind than
/// necessary, and the caller reports the whole list at once rather than making the parent re-run to
/// discover the next item.
#[cfg(windows)]
fn remove_service(leftovers: &mut Vec<Leftover>) -> Result<()> {
    use std::ffi::OsStr;

    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    use crate::service::SERVICE_NAME;

    let manager = ServiceManager::local_computer(None::<&OsStr>, ServiceManagerAccess::CONNECT)?;

    // `service` is bound by the `if let`, so it drops at the end of that arm — before the
    // registration check further down, which is what that check needs. `DeleteService` only *marks*
    // a service for deletion and the record survives until the last handle closes, so verifying
    // while still holding one would report every uninstall as incomplete. No extra scope is needed
    // to get that; an earlier version wrapped this in one and claimed the braces were load-bearing,
    // which was wrong in the direction that stops the next reader removing dead code.
    let delete_failed = if let Ok(service) = manager.open_service(
        SERVICE_NAME,
        ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
    ) {
        let _ = stop_and_wait(&service);
        match service.delete() {
            Ok(()) => {
                println!("Stopped and deleted service '{SERVICE_NAME}'.");
                false
            }
            Err(e) => {
                leftovers.push(Leftover {
                    what: "the service",
                    detail: SERVICE_NAME.to_string(),
                    why: describe_service_error(&e),
                });
                true
            }
        }
    } else {
        // Deliberately still verifies below. "Cannot open it" is not the same as "it is not there"
        // — an access failure prints this line too, and in that case the registration check is the
        // only thing that would notice a service still sitting on the machine.
        println!("Service '{SERVICE_NAME}' was not installed.");
        false
    };

    // A successful `delete()` is not proof the service is gone, and the difference is one a parent
    // meets later as a bare "os error 1072" on their *next* install. If anything else holds a
    // handle — an open Services console is the usual one — the record stays behind, marked for
    // deletion, and the SCM refuses to register a new service under the same name until it clears.
    // Reporting it here, while they are still looking at the uninstall, is the whole point.
    //
    // Retried briefly rather than checked once: our own handle has just closed and the SCM does
    // that asynchronously, so a single immediate query can see a service that is already on its
    // way out.
    // Skipped when the delete itself already failed: that is one problem, and reporting it twice
    // under two names would make the leftover list look worse than the machine is.
    if !delete_failed && service_still_registered(&manager) {
        leftovers.push(Leftover {
            what: "the service registration (marked for deletion)",
            detail: SERVICE_NAME.to_string(),
            why:
                "something still holds a handle to it — close Services and Task Manager; a reboot \
                  always clears it"
                    .to_string(),
        });
    }

    // Before the binary is deleted, not after: the helpers this kills are running *from* that
    // binary, and Windows keeps a running executable's image open. Skipping this is what made
    // `remove_dir_all` below fail whenever anyone was signed in.
    terminate_resident_helpers();

    delete_firewall_rule();
    // Verified, because `delete_firewall_rule` is best-effort by design: it ignores netsh's result
    // so a reinstall does not report on a rule it is about to recreate anyway. That is right for
    // install and wrong for uninstall, where an undeleted rule is a hole left open on the machine
    // and nothing else would ever mention it.
    if firewall_rule_exists() {
        leftovers.push(Leftover {
            what: "the firewall rule",
            detail: FIREWALL_RULE.to_string(),
            why: "netsh did not remove it".to_string(),
        });
    }

    let dir = install_dir();
    leftovers.extend(Leftover::removal(
        "the installed program directory",
        &dir,
        std::fs::remove_dir_all(&dir),
    ));
    Ok(())
}

/// Whether the service is still registered a moment after being deleted.
///
/// Polled for about a second: closing the last handle lets the SCM drop the record, but it does so
/// on its own schedule, and a single query issued immediately after `delete()` can still see it.
/// Answering "still there" too eagerly would put a scary line on a perfectly good uninstall.
#[cfg(windows)]
fn service_still_registered(manager: &windows_service::service_manager::ServiceManager) -> bool {
    use windows_service::service::ServiceAccess;

    use crate::service::SERVICE_NAME;

    for attempt in 0..5 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        if manager
            .open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS)
            .is_err()
        {
            return false;
        }
    }
    true
}

/// Whether a firewall rule by our name exists at all.
///
/// Distinct from [`firewall_rule_is_subnet_scoped`], which asks whether a rule is correctly scoped
/// — a question that answers "no" both for a wrong rule and for no rule, and so cannot tell an
/// uninstall whether it finished.
///
/// Decided on netsh's **exit status**, not its output: "No rules match the specified criteria." is
/// localized and would make this work only on English installs, which is the same locale trap
/// `firewall_rule_is_subnet_scoped` documents avoiding by matching value tokens.
#[cfg(windows)]
fn firewall_rule_exists() -> bool {
    matches!(show_firewall_rule(), Ok(out) if out.status.success())
}

/// Ask netsh about our rule, once, for both questions asked of it.
///
/// [`firewall_rule_exists`] reads the exit status and [`firewall_rule_is_subnet_scoped`] reads the
/// output — two projections of one query, which is what they now visibly are. Written out twice
/// they were also the third and fourth hand-built `netsh … name={FIREWALL_RULE}` invocation in this
/// file, so the rule name and the hardened `system32()` path had four places to stay in step.
#[cfg(windows)]
fn show_firewall_rule() -> std::io::Result<std::process::Output> {
    std::process::Command::new(crate::syspath::system32("netsh.exe"))
        .args(["advfirewall", "firewall", "show", "rule"])
        .arg(format!("name={FIREWALL_RULE}"))
        .output()
}

// ---------------------------------------------------------------------------
// Non-Windows: dev convenience (write config/cert, no service)
// ---------------------------------------------------------------------------

#[cfg(not(windows))]
fn deploy(_port: u16) -> Result<()> {
    println!("(service install is Windows-only — config + cert written for dev `run`)");
    Ok(())
}

#[cfg(not(windows))]
fn remove_service(_leftovers: &mut Vec<Leftover>) -> Result<()> {
    println!("(service uninstall is Windows-only)");
    Ok(())
}

// ---------------------------------------------------------------------------
// What this install is doing to whatever was here before
// ---------------------------------------------------------------------------

/// The relationship between the version on disk and the version doing the installing.
///
/// Recorded because nothing currently persists a version at all: `crate::VERSION` is printed and
/// discarded, so an upgrade cannot see what it is upgrading *from*. That makes three things
/// impossible — migrating data written by an older build, warning about a downgrade, and letting
/// `doctor` notice that the binary and the data dir disagree.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum InstallKind {
    /// Nothing was installed here before.
    Fresh,
    /// Same version over itself — a repair, not a change.
    Reinstall,
    Upgrade {
        from: String,
    },
    /// An older binary over a newer stamp. Not blocked, but never silent: data written by the
    /// newer build may carry keys this one drops on its next write.
    Downgrade {
        from: String,
    },
    /// A stamp exists and this build cannot order it against itself.
    ///
    /// Deliberately **not** folded into `Fresh`. They differ in the only way that matters: `Fresh`
    /// means nothing was here, `Unknown` means something was here and we cannot say what. Collapsing
    /// the two is the same mistake `install` already fixed for an unreadable config, where a parse
    /// failure silently reset the curfew and rules to defaults.
    ///
    /// Carries nothing. There is by definition no version to report, and the previous shape — a
    /// `from` holding a placeholder like `"(unreadable)"` — put a message where a version belongs.
    /// Both consumers then wrapped it in their own parentheses and printed
    /// `record is unreadable ((unreadable))`, and any future consumer that *compared* `from` rather
    /// than printing it — a data migration keyed on the previous version being the obvious one, and
    /// the reason this enum exists — would have matched a sentence as if it were a version.
    Unknown,
}

/// What the on-disk install record says, if anything.
///
/// Three outcomes with three representations, so none of them has to be smuggled through another's
/// type. `Missing` and `Corrupt` are the distinction [`InstallKind::Unknown`] exists to preserve;
/// keeping them apart here is what stops a placeholder string reaching a version field.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Stamp {
    /// No record on disk.
    Missing,
    /// A record exists and no version could be read out of it — unparseable, or missing the key.
    Corrupt,
    /// Whatever the record claims. Not necessarily a version this build can order.
    Version(String),
}

/// Split a `major.minor.patch` version into orderable parts. `None` if it is not that shape.
fn version_parts(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.trim().split('.');
    let mut next = || it.next()?.parse::<u64>().ok();
    let parts = (next()?, next()?, next()?);
    // A trailing component means this is not the shape we think it is; say so rather than
    // comparing a prefix and being confidently wrong about which build wrote the data.
    it.next().is_none().then_some(parts)
}

/// Decide what this install is doing, from the stamp left by the last one.
pub(crate) fn classify_install(previous: &Stamp, current: &str) -> InstallKind {
    let prev = match previous {
        Stamp::Missing => return InstallKind::Fresh,
        Stamp::Corrupt => return InstallKind::Unknown,
        Stamp::Version(v) => v,
    };
    let (Some(p), Some(c)) = (version_parts(prev), version_parts(current)) else {
        return InstallKind::Unknown;
    };
    match p.cmp(&c) {
        std::cmp::Ordering::Equal => InstallKind::Reinstall,
        std::cmp::Ordering::Less => InstallKind::Upgrade { from: prev.clone() },
        std::cmp::Ordering::Greater => InstallKind::Downgrade { from: prev.clone() },
    }
}

/// Where the installed version is recorded, beside the config it describes.
fn stamp_path() -> std::path::PathBuf {
    config::data_paths().dir.join("installed.json")
}

/// The version the last install wrote, if any.
///
/// Three outcomes, kept distinct on purpose (see [`InstallKind::Unknown`]): `None` for no stamp at
/// all, `Some(version)` for one this build can read, and `Some(raw)` for a stamp that exists and
/// does not parse — which must reach [`classify_install`] as something rather than nothing.
pub(crate) fn read_stamp() -> Stamp {
    let Ok(raw) = std::fs::read_to_string(stamp_path()) else {
        return Stamp::Missing;
    };
    // Deliberately not `Missing` on either failure below: a corrupt stamp is not an absent one, and
    // that difference is the whole reason `Stamp` has three variants.
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(v) => match v.get("version").and_then(|v| v.as_str()) {
            Some(version) => Stamp::Version(version.to_string()),
            None => Stamp::Corrupt,
        },
        Err(_) => Stamp::Corrupt,
    }
}

/// Record what is installed now, so the next install can see what it is replacing.
fn write_stamp() -> Result<()> {
    let body = serde_json::json!({
        "version": crate::VERSION,
        // Not read by anything yet. Written because the question "when did this last change?" is
        // the first one asked when a machine starts misbehaving, and it cannot be answered
        // retroactively.
        "installed_at": crate::clock::now().to_rfc3339(),
    });
    // `write_atomic`, not `fs::write`, like every other writer into this data dir (`config.rs`,
    // `cert.rs`, `pairing.rs`, `sessionstore.rs`, `rules.rs`). A torn write here is not a lost
    // convenience: `read_stamp` would return "unreadable" permanently, which surfaces as a standing
    // warning in `doctor` that nothing short of a reinstall clears.
    let path = stamp_path();
    crate::config::write_atomic(&path, &serde_json::to_vec_pretty(&body)?)
        .with_context(|| format!("writing {}", path.display()))
}

// ---------------------------------------------------------------------------
// Uninstall: what could not be removed
// ---------------------------------------------------------------------------

/// One thing `uninstall` set out to remove and could not.
///
/// Exists because the previous behaviour printed `(could not remove ...)` as a parenthetical and
/// returned `Ok(())` — so a half-finished uninstall reported success. On this product that is the
/// worst possible direction to be wrong in: the parent believes the controls are gone, and a
/// service or a hooked helper is still running.
pub(crate) struct Leftover {
    /// What it was, in the parent's terms — "the installed binary", not "remove_dir_all".
    pub what: &'static str,
    /// Which one: a path, or a rule name.
    pub detail: String,
    /// The OS's reason, kept verbatim. A parent may not read it; whoever they forward it to will.
    pub why: String,
}

impl Leftover {
    /// A filesystem removal that failed for a reason other than the thing already being gone.
    ///
    /// **"Missing is success" is one rule and now has one definition.** It was written three
    /// different ways across the uninstall path — a `NotFound` match arm, a `kind() != NotFound`
    /// guard, and an `exists()` pre-check. The last was a different rule in the same clothes: it
    /// asks the filesystem twice and races anything deleting the path in between.
    ///
    /// Returns an `Option` so callers can `extend` a list with it, which is what removes the
    /// three-way spelling rather than merely centralising it.
    fn removal(what: &'static str, path: &Path, result: std::io::Result<()>) -> Option<Self> {
        match result {
            Ok(()) => None,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => Some(Leftover {
                what,
                detail: path.display().to_string(),
                why: e.to_string(),
            }),
        }
    }
}

/// The message `uninstall` fails with when anything survived.
///
/// Names every leftover, then says what is worth trying — and deliberately **does not** blame a
/// resident watcher. That was the overwhelmingly likely cause, and [`terminate_resident_helpers`]
/// now runs before every removal, so by the time this message is reached that explanation has
/// already been ruled out. Pointing at it anyway would send the parent to sign a user out and
/// change nothing, which is worse than admitting the cause is unknown.
pub(crate) fn render_leftovers(items: &[Leftover]) -> String {
    use std::fmt::Write as _;

    let mut s = format!(
        "uninstall did not finish — {} item(s) could not be removed:\n\n",
        items.len()
    );
    for item in items {
        let _ = writeln!(s, "  - {} - {}", item.what, item.detail);
        let _ = writeln!(s, "    ({})", item.why);
    }
    let _ = write!(
        s,
        "\nAny resident watcher processes were already stopped, so something else is holding these \
         open.\nUsual causes: a console whose current directory is inside the folder, an open \
         Services or\nTask Manager window, or antivirus. Close those and re-run \
         `{INSTALL_EXE_NAME} uninstall`."
    );
    s
}

// ---------------------------------------------------------------------------
// Helpers left resident in a user's session
// ---------------------------------------------------------------------------

/// The executable name out of a `PROCESSENTRY32W::szExeFile` buffer.
///
/// **Truncates at the first NUL; it must never trim trailing ones.** `szExeFile` is a
/// null-*terminated* name inside a fixed 260-unit array, and nothing promises the units past the
/// terminator are zero. `processes_named` reuses one `PROCESSENTRY32W` across the whole enumeration,
/// so a long name followed by a shorter one leaves the tail of the long one in place. Trimming
/// trailing NULs would then yield `"host-health.exe\0<residue>"`, the caller's comparison would
/// silently stop matching, the helper would survive, and the upgrade would fail on the locked binary
/// the whole enumeration exists to release — intermittently, depending on process ordering.
///
/// Pure and outside the `#[cfg(windows)]` block on purpose: this is the part with a rule worth
/// getting wrong, and it would otherwise be reachable only on the machine that cannot run tests.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn exe_name_from_buf(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Which running processes are our own resident watchers, and must be terminated.
///
/// **Why this is needed at all.** `run_watcher_supervisor` spawns the installed binary as
/// `helper --watch` inside each interactive session, and nothing stops it: the supervisor is a
/// detached thread that dies with the service without running its `TerminateProcess`, and the
/// helper ignores the resulting broken pipe. The helper's image *is* the installed binary, and
/// Windows holds a running executable open — so a leftover helper makes `std::fs::copy` fail on
/// upgrade and `remove_dir_all` fail on uninstall. `deploy` already names "a lingering helper
/// process" as a likely cause of a failed copy without doing anything about it.
///
/// **Matched on the full path, never on the name.** A child can put a file called
/// `host-health.exe` anywhere they can write. Terminating processes by name would hand them a way
/// to make the installer kill something else; matching the exact path we installed to cannot be
/// influenced from a directory they control.
///
/// Case-insensitive because process enumeration returns whatever case the OS recorded, which is
/// not necessarily the case we wrote.
///
/// Only called from the Windows teardown paths, but tested on every platform — the selection rule
/// is the security-relevant half and it must not be the part that only compiles on the target.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn helpers_to_terminate(
    procs: &[(u32, String)],
    install_exe: &Path,
    self_pid: u32,
) -> Vec<u32> {
    let target = install_exe.to_string_lossy();
    procs
        .iter()
        .filter(|(pid, path)| *pid != self_pid && path.eq_ignore_ascii_case(&target))
        .map(|(pid, _)| *pid)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The installed layout, as a path with a space in it — which every real install has and which
    /// is what the quoting below exists for.
    ///
    /// A **literal with Windows separators**, not `dir.join(INSTALL_EXE_NAME)`. `join` uses the
    /// *host's* separator, so on the dev machine it yields
    /// `C:\Program Files\HostHealth/host-health.exe` — a path that exists on no platform, and one
    /// that would make these tests assert against a shape the target never produces.
    fn installed() -> std::path::PathBuf {
        std::path::PathBuf::from(r"C:\Program Files\HostHealth\host-health.exe")
    }

    /// The fixture above hard-codes the binary's name, so it has to be the real one.
    #[test]
    fn the_test_fixture_uses_the_name_the_installer_actually_writes() {
        assert!(
            installed().to_string_lossy().ends_with(INSTALL_EXE_NAME),
            "fixture drifted from INSTALL_EXE_NAME ({INSTALL_EXE_NAME})"
        );
    }

    /// A version the record could have carried, for the cases that are about ordering rather than
    /// about the record being readable.
    fn stamped(v: &str) -> Stamp {
        Stamp::Version(v.to_string())
    }

    #[test]
    fn a_first_install_has_nothing_to_compare_against() {
        assert_eq!(
            classify_install(&Stamp::Missing, "0.2.3"),
            InstallKind::Fresh
        );
    }

    #[test]
    fn the_same_version_over_itself_is_a_reinstall_not_an_upgrade() {
        assert_eq!(
            classify_install(&stamped("0.2.3"), "0.2.3"),
            InstallKind::Reinstall
        );
    }

    #[test]
    fn a_newer_binary_over_an_older_stamp_is_an_upgrade() {
        assert_eq!(
            classify_install(&stamped("0.2.3"), "0.10.0"),
            InstallKind::Upgrade {
                from: "0.2.3".into()
            },
            "0.10 must sort above 0.2 — string ordering would call this a downgrade"
        );
    }

    #[test]
    fn an_older_binary_over_a_newer_stamp_is_a_downgrade() {
        assert_eq!(
            classify_install(&stamped("0.3.0"), "0.2.3"),
            InstallKind::Downgrade {
                from: "0.3.0".into()
            }
        );
    }

    /// The distinction this enum exists for. A record that cannot be ordered must not read as
    /// "nothing was installed here" — that is how a migration gets skipped silently.
    #[test]
    fn an_unparseable_stamp_is_never_mistaken_for_a_fresh_install() {
        for junk in ["", "garbage", "1.2", "1.2.3.4", "v1.2.3", "1.2.x"] {
            assert_eq!(
                classify_install(&stamped(junk), "0.2.3"),
                InstallKind::Unknown,
                "{junk:?} must be Unknown, never Fresh"
            );
        }
    }

    /// The other half of the same distinction, and the one that used to be smuggled through the
    /// version field as the string `"(unreadable)"`.
    #[test]
    fn a_corrupt_record_is_unknown_and_carries_no_version() {
        assert_eq!(
            classify_install(&Stamp::Corrupt, "0.2.3"),
            InstallKind::Unknown,
            "a corrupt record is not an absent one"
        );
    }

    #[test]
    fn an_uninstall_that_left_something_behind_says_what_and_how_to_finish() {
        let msg = render_leftovers(&[Leftover {
            what: "the installed binary",
            detail: r"C:\Program Files\HostHealth\host-health.exe".into(),
            why: "Access is denied. (os error 5)".into(),
        }]);

        assert!(
            msg.contains("did not finish"),
            "must not read as success: {msg}"
        );
        assert!(msg.contains(r"C:\Program Files\HostHealth\host-health.exe"));
        assert!(
            msg.contains("Access is denied."),
            "the OS reason has to survive to whoever debugs it: {msg}"
        );
        // The installer terminates resident watchers before every removal, so by the time this
        // message exists that cause is ruled out. Naming it would send the parent to sign a user
        // out and change nothing.
        assert!(
            !msg.contains("Sign that user out"),
            "must not blame a cause the uninstall already handled: {msg}"
        );
        assert!(
            msg.contains("already stopped") && msg.contains("uninstall`"),
            "must say what was ruled out and what to try next: {msg}"
        );
    }

    /// Build a `szExeFile`-shaped buffer: a name, its terminator, then whatever was left in the
    /// array by a previous, longer entry.
    fn entry_buf(name: &str, residue: &str) -> Vec<u16> {
        let mut buf: Vec<u16> = name.encode_utf16().collect();
        buf.push(0);
        buf.extend(residue.encode_utf16());
        buf.resize(260, 0);
        buf
    }

    /// The bug this guards: `processes_named` reuses one `PROCESSENTRY32W` across the enumeration,
    /// so a shorter name lands in an array still holding the tail of a longer one. Trimming trailing
    /// NULs instead of cutting at the first would carry the residue into the comparison, the helper
    /// would not be recognised, and the upgrade would fail on the binary it still holds open.
    #[test]
    fn a_process_name_stops_at_its_terminator_not_at_the_last_nul() {
        let buf = entry_buf("host-health.exe", "a-longer-previous-name.exe");
        assert_eq!(exe_name_from_buf(&buf), "host-health.exe");
    }

    #[test]
    fn a_clean_buffer_reads_back_unchanged() {
        assert_eq!(
            exe_name_from_buf(&entry_buf("chrome.exe", "")),
            "chrome.exe"
        );
    }

    /// A buffer with no terminator at all must not read past its end.
    #[test]
    fn a_buffer_with_no_terminator_is_taken_whole() {
        let full: Vec<u16> = "x".repeat(260).encode_utf16().collect();
        assert_eq!(exe_name_from_buf(&full).len(), 260);
    }

    #[test]
    fn a_resident_helper_is_selected_for_termination() {
        let exe = installed();
        let procs = vec![(4321u32, exe.to_string_lossy().into_owned())];
        assert_eq!(helpers_to_terminate(&procs, &exe, 1000), vec![4321]);
    }

    /// The installer runs from the same binary it is trying to replace. Killing itself would end
    /// the upgrade halfway through, with enforcement off.
    #[test]
    fn the_installer_never_terminates_itself() {
        let exe = installed();
        let procs = vec![(1000u32, exe.to_string_lossy().into_owned())];
        assert!(
            helpers_to_terminate(&procs, &exe, 1000).is_empty(),
            "our own pid must never be selected"
        );
    }

    /// A standard user can drop a file called `host-health.exe` in their Downloads folder. Matching
    /// on the name would let them choose what the elevated installer terminates.
    #[test]
    fn a_same_named_binary_elsewhere_on_disk_is_left_alone() {
        let exe = installed();
        let procs = vec![(
            777u32,
            r"C:\Users\son\Downloads\host-health.exe".to_string(),
        )];
        assert!(
            helpers_to_terminate(&procs, &exe, 1).is_empty(),
            "only the path we installed to may be terminated"
        );
    }

    /// Process enumeration reports whatever case the OS recorded, not the case we wrote.
    #[test]
    fn a_helper_is_matched_regardless_of_path_case() {
        let exe = installed();
        let procs = vec![(
            42u32,
            r"c:\PROGRAM FILES\hosthealth\HOST-HEALTH.EXE".to_string(),
        )];
        assert_eq!(helpers_to_terminate(&procs, &exe, 1), vec![42]);
    }

    /// `--port` is the one piece of argument handling here that is pure and platform-independent,
    /// and it sits in the file with the worst on-hardware record. Cheap to pin, so pin it.
    /// The docs spell out the installed binary's path; the code derives it. Pin them together.
    ///
    /// `INSTALL_EXE_NAME` is described in the README as "the bland on-disk name" — chosen for a
    /// reason, and therefore re-choosable. Renaming it would break the remote-update sequence in
    /// `docs/REMOTE-UPDATE.md`, which invokes that exact path over a PowerShell session, with
    /// nothing failing in CI — on the one flow where nobody is standing at the machine.
    ///
    /// Same shape as the guide/script pin in `remotesetup`, and the same reason: prose that
    /// states a fact about the code needs something holding the two together.
    #[test]
    fn the_docs_name_the_binary_this_install_actually_writes() {
        let docs = [
            ("README.md", include_str!("../README.md")),
            (
                "docs/REMOTE-UPDATE.md",
                include_str!("../docs/REMOTE-UPDATE.md"),
            ),
            (
                "docs/WINDOWS-TESTING.md",
                include_str!("../docs/WINDOWS-TESTING.md"),
            ),
        ];

        let dir = "Program Files\\HostHealth\\";
        let mut found = 0usize;
        for (name, text) in docs {
            for (n, line) in text.lines().enumerate() {
                let mut rest = line;
                while let Some(i) = rest.find(dir) {
                    rest = &rest[i + dir.len()..];
                    let named: String = rest
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '.' || *c == '-')
                        .collect();
                    // A path to the directory itself, not to a file in it.
                    if !named.ends_with(".exe") {
                        continue;
                    }
                    found += 1;
                    assert_eq!(
                        named,
                        INSTALL_EXE_NAME,
                        "{name}:{} names a binary this install never writes",
                        n + 1
                    );
                }
            }
        }
        assert!(
            found >= 4,
            "found only {found} references to the installed binary — the scan has drifted from \
             the documentation and is checking nothing"
        );
    }

    #[test]
    fn port_flag_parses_or_explains() {
        let args = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();

        assert_eq!(parse_port_flag(&args(&["install"])).unwrap(), None);
        assert_eq!(
            parse_port_flag(&args(&["install", "--port", "9443"])).unwrap(),
            Some(9443)
        );
        // The boundaries of u16, which is what makes the bare `parse` safe.
        assert_eq!(
            parse_port_flag(&args(&["install", "--port", "65535"])).unwrap(),
            Some(65535)
        );
        assert!(parse_port_flag(&args(&["install", "--port", "65536"])).is_err());
        // Port 0 parses fine as a u16 and is not a port you can serve on.
        assert!(parse_port_flag(&args(&["install", "--port", "0"])).is_err());
        assert!(parse_port_flag(&args(&["install", "--port"])).is_err());
        assert!(parse_port_flag(&args(&["install", "--port", "http"])).is_err());
    }

    /// The decoder must name the codes a real install actually produced.
    ///
    /// These four are not hypothetical: 5 is what made a fresh install impossible (the service
    /// handle lacked `QUERY_STATUS`, so every status poll was refused), and 1072, 1058 and 1060
    /// all appeared in one failing install on a real machine. Each must come back with its
    /// number, its Win32 name, and something the reader can act on.
    #[cfg(windows)]
    #[test]
    fn service_errors_are_decoded_not_just_reported() {
        use std::io;
        let decode = |code: i32| {
            describe_service_error(&windows_service::Error::Winapi(
                io::Error::from_raw_os_error(code),
            ))
        };

        for (code, name, must_mention) in [
            (5, "ERROR_ACCESS_DENIED", "elevated"),
            (1072, "ERROR_SERVICE_MARKED_FOR_DELETE", "reboot"),
            (1058, "ERROR_SERVICE_DISABLED", "Disabled"),
            (1060, "ERROR_SERVICE_DOES_NOT_EXIST", "registered"),
        ] {
            let msg = decode(code);
            assert!(
                msg.contains(&code.to_string()),
                "{code}: must state the number\n{msg}"
            );
            assert!(msg.contains(name), "{code}: must name the constant\n{msg}");
            assert!(
                msg.contains(must_mention),
                "{code}: must say something actionable containing {must_mention:?}\n{msg}"
            );
        }

        // An unrecognised code must still be more useful than the old message, which said only
        // "IO error in winapi call" no matter what the OS reported.
        let unknown = decode(4321);
        assert!(
            unknown.contains("4321"),
            "unknown codes still report the number\n{unknown}"
        );
        assert!(
            !unknown.contains("IO error in winapi call"),
            "must not fall back to the message that hid the cause\n{unknown}"
        );
    }

    /// Both service handles must carry `QUERY_STATUS`.
    ///
    /// A missing access right does not fail where it is granted. It fails later, inside whichever
    /// call needed it, as an opaque winapi error a long way from the cause — so this is pinned
    /// rather than left to review. It has gone wrong twice: `DELETE` missing made a rollback
    /// silently not roll back, and `QUERY_STATUS` missing from the create path meant
    /// `start_and_verify` could not read the status of a freshly installed service at all. Every
    /// poll returned access-denied for the whole timeout, so the installer destroyed services
    /// that had started correctly, and reported that they never started.
    ///
    /// Only the create path was affected, which is exactly why it lasted: upgrades worked.
    ///
    /// Needs no privileges — it only inspects bitflags — so it runs on the Windows CI runner
    /// rather than waiting for someone to be standing at the machine.
    #[cfg(windows)]
    #[test]
    fn both_service_handles_can_read_status() {
        use windows_service::service::ServiceAccess;

        assert!(
            CREATE_ACCESS.contains(ServiceAccess::QUERY_STATUS),
            "a new service must be queryable or start_and_verify can never confirm it started"
        );
        assert!(
            UPDATE_ACCESS.contains(ServiceAccess::QUERY_STATUS),
            "an updated service must be queryable for the same reason"
        );
        // The rights each path uniquely depends on, and which have gone missing before.
        assert!(
            CREATE_ACCESS.contains(ServiceAccess::DELETE),
            "rollback deletes the service it just created"
        );
        assert!(
            UPDATE_ACCESS.contains(ServiceAccess::STOP),
            "an upgrade stops the old service to release the locked exe"
        );
        assert!(
            CREATE_ACCESS.contains(ServiceAccess::START)
                && UPDATE_ACCESS.contains(ServiceAccess::START),
            "both paths start the service"
        );
    }
}
