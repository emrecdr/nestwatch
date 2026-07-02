//! Windows-only: run the app as a SYSTEM service via the Service Control Manager.
//!
//! A standard (non-admin) user cannot stop or delete a SYSTEM service — this is what makes
//! the enforcement (curfew/kill/shutdown/serving) tamper-resistant. Screen capture is
//! delegated to a session helper (see `crate::session`) because Session 0 has no desktop.
//!
//! Compile-checked via the Windows target; must be runtime-verified on Windows.

use std::ffi::OsString;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

/// Internal service name (also used by install/uninstall). Low-profile, not impersonating.
pub const SERVICE_NAME: &str = "HostHealthService";
pub const SERVICE_DISPLAY_NAME: &str = "Host Health Service";
pub const SERVICE_DESCRIPTION: &str = "Monitors host health and availability.";

/// Entry point for the `service-run` subcommand (invoked by the SCM).
pub fn run() -> Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
    Ok(())
}

define_windows_service!(ffi_service_main, service_main);

fn service_main(_arguments: Vec<OsString>) {
    if let Err(err) = run_service() {
        tracing::error!(error = ?err, "service exited with error");
    }
}

fn run_service() -> Result<()> {
    // The control handler signals shutdown through this channel.
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();

    let event_handler = move |control_event| -> ServiceControlHandlerResult {
        match control_event {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                let _ = shutdown_tx.send(());
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        }
    };

    let status_handle = service_control_handler::register(SERVICE_NAME, event_handler)?;
    status_handle.set_service_status(status(ServiceState::Running))?;

    // Build state with the service controller (screenshots via session helper).
    let config = Arc::new(crate::config::Config::load()?);
    let state = crate::state::AppState {
        control: crate::control::service_control(),
        curfew: Arc::new(std::sync::RwLock::new(config.curfew.clone())),
        config,
        limiter: Arc::new(crate::auth::LoginLimiter::default()),
    };

    // Graceful shutdown: when the SCM asks us to stop, trigger axum-server's handle.
    let handle = axum_server::Handle::new();
    let stop_handle = handle.clone();
    std::thread::spawn(move || {
        let _ = shutdown_rx.recv();
        stop_handle.graceful_shutdown(Some(Duration::from_secs(5)));
    });

    let runtime = tokio::runtime::Runtime::new()?;
    let result = runtime.block_on(crate::server::serve_with_handle(state, handle));

    status_handle.set_service_status(status(ServiceState::Stopped))?;
    result
}

fn status(state: ServiceState) -> ServiceStatus {
    let controls_accepted = match state {
        ServiceState::Running => {
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
        }
        _ => ServiceControlAccept::empty(),
    };
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    }
}
