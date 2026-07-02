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
#[cfg_attr(not(windows), allow(dead_code))]
const FIREWALL_RULE: &str = "HostHealthService";

pub fn install() -> Result<()> {
    println!("== nestwatch :: install ==\n");

    let args: Vec<String> = std::env::args().collect();
    let existing = Config::load().ok();

    // Port precedence: --port flag > existing config > default.
    let port = match parse_port_flag(&args)? {
        Some(p) => p,
        None => existing.as_ref().map(|c| c.port).unwrap_or(DEFAULT_PORT),
    };

    // Interactive by default; NESTWATCH_PASSWORD allows a silent/headless install.
    let password = match std::env::var("NESTWATCH_PASSWORD") {
        Ok(pw) if !pw.is_empty() => pw,
        _ => {
            let pw = rpassword::prompt_password("Set a control password: ")?;
            let confirm = rpassword::prompt_password("Confirm password:      ")?;
            if pw != confirm {
                bail!("passwords do not match");
            }
            pw
        }
    };
    if password.chars().count() < 10 {
        bail!("please choose a password of at least 10 characters (a passphrase is ideal)");
    }

    let paths = config::data_paths();
    // Create + lock down the data dir BEFORE writing any secret into it.
    prepare_data_dir(&paths.dir)?;

    let cfg = Config {
        port,
        password_hash: auth::hash_password(&password)?,
        // Preserve an existing curfew across reinstalls.
        curfew: existing.map(|c| c.curfew).unwrap_or_default(),
    };
    cfg.save()?;
    // Always (re)generate at install: picks up the current LAN IP as a SAN and yields a
    // fingerprint to show the operator.
    let fingerprint = crate::cert::generate(&paths.cert, &paths.key)?;

    deploy(cfg.port)?;

    println!(
        "\nInstalled. Reach the dashboard at https://<this-pc>:{}",
        cfg.port
    );
    println!("\nTLS cert SHA-256 — verify this the first time your browser warns, so you know");
    println!("you're trusting THIS machine and not a LAN impostor:");
    println!("  {fingerprint}");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    let purge = std::env::args().any(|a| a == "--purge");
    remove_service()?;
    if purge {
        let dir = config::data_paths().dir;
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            println!("(could not remove {}: {e})", dir.display());
        } else {
            println!("Purged config/cert at {}", dir.display());
        }
    } else {
        println!(
            "Config/cert left in {} (use `uninstall --purge` to remove).",
            config::data_paths().dir.display()
        );
    }
    Ok(())
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

// ---------------------------------------------------------------------------
// Windows: install/protect the SYSTEM service
// ---------------------------------------------------------------------------

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

#[cfg(windows)]
fn deploy(port: u16) -> Result<()> {
    use std::ffi::{OsStr, OsString};

    use windows_service::service::{
        ServiceAccess, ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceType,
    };
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    use crate::service::{SERVICE_DESCRIPTION, SERVICE_DISPLAY_NAME, SERVICE_NAME};

    let dir = install_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let target_exe = dir.join("host-health.exe");
    let current_exe = std::env::current_exe()?;

    let manager = ServiceManager::local_computer(
        None::<&OsStr>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )?;

    // If the service already exists, this is an update: stop it (to release the locked exe),
    // overwrite the binary, and reuse the registration.
    let existing = manager
        .open_service(
            SERVICE_NAME,
            ServiceAccess::QUERY_STATUS
                | ServiceAccess::STOP
                | ServiceAccess::START
                | ServiceAccess::CHANGE_CONFIG,
        )
        .ok();
    if let Some(svc) = &existing {
        stop_and_wait(svc)?;
    }

    if current_exe != target_exe {
        std::fs::copy(&current_exe, &target_exe)
            .with_context(|| format!("copying binary to {}", target_exe.display()))?;
    }

    // Harden the install dir (Users get read+execute so the helper can run) and (re)create
    // the LAN-scoped firewall rule.
    harden_program_dir(&dir)?;
    configure_firewall(port)?;

    match existing {
        Some(svc) => {
            svc.start(&[] as &[&OsStr])
                .context("starting existing service")?;
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
            let service = manager
                .create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START)
                .context("creating service")?;
            let _ = service.set_description(SERVICE_DESCRIPTION);
            configure_recovery();
            if let Err(e) = service.start(&[] as &[&OsStr]) {
                // Don't leave a registered-but-dead service behind.
                let _ = service.delete();
                return Err(anyhow::anyhow!(e)).context("starting new service (rolled back)");
            }
            println!("Installed service '{SERVICE_NAME}' (LocalSystem, auto-start/restart).");
        }
    }

    println!("Binary: {}", dir.join("host-health.exe").display());
    println!("Reminder: this resists a STANDARD user — ensure your son is not an administrator,");
    println!("and that this PC's network is set to 'Private' (not 'Public') so the rule applies.");
    Ok(())
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
    let mut cmd = std::process::Command::new("icacls");
    cmd.arg(path).arg("/inheritance:r");
    for grant in grants {
        cmd.arg("/grant:r").arg(grant);
    }
    let status = cmd.status().context("running icacls")?;
    if !status.success() {
        bail!(
            "failed to ACL-harden {} (icacls exited {status}); refusing to continue",
            path.display()
        );
    }
    Ok(())
}

/// Recreate an inbound TCP rule scoped to the local subnet on Private/Domain networks.
#[cfg(windows)]
fn configure_firewall(port: u16) -> Result<()> {
    use std::process::Command;

    // Idempotent: delete any stale rule (possibly on an old port) first.
    let _ = Command::new("netsh")
        .args(["advfirewall", "firewall", "delete", "rule"])
        .arg(format!("name={FIREWALL_RULE}"))
        .status();

    let status = Command::new("netsh")
        .args(["advfirewall", "firewall", "add", "rule"])
        .arg(format!("name={FIREWALL_RULE}"))
        .args(["dir=in", "action=allow", "protocol=TCP"])
        .arg(format!("localport={port}"))
        .args(["profile=private,domain", "remoteip=LocalSubnet"])
        .status()
        .context("running netsh")?;
    if !status.success() {
        // Non-fatal: warn loudly rather than abort, since the app still runs locally.
        println!(
            "WARNING: could not add firewall rule (netsh exited {status}); remote access may be blocked."
        );
    }
    Ok(())
}

/// Auto-restart on failure (best-effort; three attempts, 5s apart, daily reset).
#[cfg(windows)]
fn configure_recovery() {
    use crate::service::SERVICE_NAME;
    let _ = std::process::Command::new("sc")
        .args([
            "failure",
            SERVICE_NAME,
            "reset=",
            "86400",
            "actions=",
            "restart/5000/restart/5000/restart/5000",
        ])
        .status();
}

#[cfg(windows)]
fn remove_service() -> Result<()> {
    use std::ffi::OsStr;
    use std::process::Command;

    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    use crate::service::SERVICE_NAME;

    let manager = ServiceManager::local_computer(None::<&OsStr>, ServiceManagerAccess::CONNECT)?;
    if let Ok(service) = manager.open_service(
        SERVICE_NAME,
        ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
    ) {
        let _ = stop_and_wait(&service);
        service.delete().context("deleting service")?;
        println!("Stopped and deleted service '{SERVICE_NAME}'.");
    } else {
        println!("Service '{SERVICE_NAME}' was not installed.");
    }

    // Remove the firewall rule and the installed binary directory.
    let _ = Command::new("netsh")
        .args(["advfirewall", "firewall", "delete", "rule"])
        .arg(format!("name={FIREWALL_RULE}"))
        .status();
    let dir = install_dir();
    if dir.exists()
        && let Err(e) = std::fs::remove_dir_all(&dir)
    {
        println!("(could not remove {}: {e})", dir.display());
    }
    Ok(())
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
fn remove_service() -> Result<()> {
    println!("(service uninstall is Windows-only)");
    Ok(())
}
