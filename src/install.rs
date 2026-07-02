//! One-time setup and teardown.
//!
//! `install` stores the password (as an Argon2 hash only), generates the TLS cert, and —
//! on Windows — copies the binary to a protected location, registers a SYSTEM service that
//! auto-starts and auto-restarts, and ACL-hardens its files so a standard (non-admin) user
//! can't stop, read, or delete it. The service handles curfew/kill/shutdown/serving from
//! Session 0; screenshots are delegated to a session helper.
//!
//! Prerequisite for tamper-resistance: the child must be a **standard user**. Against a
//! local administrator no software-only measure is reliable. Must be run from an elevated
//! (Administrator) console.

use anyhow::{bail, Result};

use crate::auth;
use crate::config::{self, Config, DEFAULT_PORT};

pub fn install() -> Result<()> {
    println!("== nestwatch :: install ==\n");

    // Interactive by default; `NESTWATCH_PASSWORD` allows a silent/headless install.
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

    let cfg = Config {
        port: DEFAULT_PORT,
        password_hash: auth::hash_password(&password)?,
        curfew: crate::curfew::Curfew::default(),
    };
    cfg.save()?;

    let paths = config::data_paths();
    crate::cert::ensure_cert(&paths.cert, &paths.key)?;

    deploy(&paths)?;

    println!("\nInstalled. Reach the dashboard at https://<this-pc>:{}", cfg.port);
    Ok(())
}

pub fn uninstall() -> Result<()> {
    remove_service()?;
    println!(
        "Removed. Config/cert left in {} (delete manually for a clean slate).",
        config::data_paths().dir.display()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Windows: install/protect the SYSTEM service
// ---------------------------------------------------------------------------

#[cfg(windows)]
fn deploy(paths: &config::DataPaths) -> Result<()> {
    use std::ffi::{OsStr, OsString};
    use std::path::PathBuf;
    use std::process::Command;

    use windows_service::service::{
        ServiceAccess, ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceType,
    };
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    use crate::service::{SERVICE_DESCRIPTION, SERVICE_DISPLAY_NAME, SERVICE_NAME};

    // 1. Copy the binary into a protected Program Files directory.
    let program_files = std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
    let install_dir = program_files.join("HostHealth");
    std::fs::create_dir_all(&install_dir)?;
    let target_exe = install_dir.join("host-health.exe");
    let current_exe = std::env::current_exe()?;
    if current_exe != target_exe {
        std::fs::copy(&current_exe, &target_exe)?;
    }

    // 2. Register (or refuse if already present) the SYSTEM auto-start service.
    let manager = ServiceManager::local_computer(
        None::<&OsStr>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )?;
    if manager
        .open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS)
        .is_ok()
    {
        bail!("service '{SERVICE_NAME}' already exists — run `uninstall` first");
    }
    let info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(SERVICE_DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: target_exe,
        launch_arguments: vec![OsString::from("service-run")],
        dependencies: vec![],
        account_name: None, // None => LocalSystem
        account_password: None,
    };
    let service = manager.create_service(
        &info,
        ServiceAccess::CHANGE_CONFIG | ServiceAccess::START | ServiceAccess::QUERY_STATUS,
    )?;
    let _ = service.set_description(SERVICE_DESCRIPTION);

    // 3. Auto-restart on failure (three attempts, 5s apart; daily reset).
    let _ = Command::new("sc")
        .args([
            "failure",
            SERVICE_NAME,
            "reset=",
            "86400",
            "actions=",
            "restart/5000/restart/5000/restart/5000",
        ])
        .status();

    // 4. ACL-harden: only SYSTEM + Administrators; standard user gets no access
    //    (can't read the password hash/cert, can't modify/delete the binary).
    harden_acl(&install_dir);
    harden_acl(&paths.dir);

    // 5. Start it now.
    service.start(&[] as &[&OsStr])?;

    println!("Installed service '{SERVICE_NAME}' (LocalSystem, auto-start, auto-restart).");
    println!("Binary: {}", install_dir.join("host-health.exe").display());
    println!("Reminder: this resists a STANDARD user. Ensure your son is not an administrator.");
    Ok(())
}

#[cfg(windows)]
fn harden_acl(path: &std::path::Path) {
    let _ = std::process::Command::new("icacls")
        .arg(path)
        .args([
            "/inheritance:r",
            "/grant:r",
            "SYSTEM:(OI)(CI)F",
            "/grant:r",
            "Administrators:(OI)(CI)F",
        ])
        .status();
}

#[cfg(windows)]
fn remove_service() -> Result<()> {
    use std::ffi::OsStr;

    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    use crate::service::SERVICE_NAME;

    let manager =
        ServiceManager::local_computer(None::<&OsStr>, ServiceManagerAccess::CONNECT)?;
    if let Ok(service) = manager.open_service(
        SERVICE_NAME,
        ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
    ) {
        let _ = service.stop();
        service.delete()?;
        println!("Stopped and deleted service '{SERVICE_NAME}'.");
    } else {
        println!("Service '{SERVICE_NAME}' was not installed.");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Non-Windows: dev convenience (write config/cert, no service)
// ---------------------------------------------------------------------------

#[cfg(not(windows))]
fn deploy(_paths: &config::DataPaths) -> Result<()> {
    println!("(service install is Windows-only — config + cert written for dev `run`)");
    Ok(())
}

#[cfg(not(windows))]
fn remove_service() -> Result<()> {
    println!("(service uninstall is Windows-only)");
    Ok(())
}
