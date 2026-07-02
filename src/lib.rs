//! Home remote-control server for a child's Windows PC.
//!
//! The crate is organised in layers:
//! - `web` / `api` / `auth`  — HTTP presentation (axum handlers + middleware)
//! - `state` / `error`       — shared application state and the single error type
//! - `control`               — `SystemControl`: the OS abstraction (real Windows + fake)
//! - `config` / `cert`       — persisted configuration and the self-signed TLS cert
//! - `install`               — one-time setup (password, cert, auto-start task)
//!
//! Everything above `control` is OS-agnostic and runs (and is tested) on any platform.

pub mod api;
pub mod auth;
pub mod cert;
pub mod config;
pub mod control;
pub mod curfew;
pub mod error;
pub mod helper;
pub mod install;
pub mod server;
pub mod state;
pub mod web;

#[cfg(windows)]
pub mod service;
#[cfg(windows)]
pub mod session;

use std::sync::Arc;

use anyhow::Result;

/// Parse `argv` and dispatch the requested subcommand.
pub fn run_cli() -> Result<()> {
    init_tracing();
    // rustls 0.23 requires a crypto provider to be installed. We build against the
    // `ring` provider (no C toolchain needed) and install it once at startup.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("run");
    match cmd {
        "install" => install::install(),
        "uninstall" => install::uninstall(),
        "run" => run_server(),
        "helper" => run_helper(&args),
        "service-run" => run_service(),
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        other => {
            eprintln!("unknown command: {other}\n");
            print_usage();
            std::process::exit(2);
        }
    }
}

/// `helper --capture <path>`: capture a screenshot to a file (runs in the user session).
fn run_helper(args: &[String]) -> Result<()> {
    match (args.get(2).map(String::as_str), args.get(3)) {
        (Some("--capture"), Some(path)) => helper::capture(path),
        _ => {
            eprintln!("usage: nestwatch helper --capture <path>");
            std::process::exit(2);
        }
    }
}

/// `service-run`: entry point invoked by the Windows Service Control Manager.
fn run_service() -> Result<()> {
    #[cfg(windows)]
    {
        service::run()
    }
    #[cfg(not(windows))]
    {
        anyhow::bail!("`service-run` is only supported on Windows")
    }
}

/// Load config, assemble [`state::AppState`], and serve until shutdown.
fn run_server() -> Result<()> {
    let config = Arc::new(config::Config::load()?);
    let state = state::AppState {
        control: control::interactive_control(),
        curfew: Arc::new(std::sync::RwLock::new(config.curfew.clone())),
        config,
        limiter: Arc::new(auth::LoginLimiter::default()),
        login_lock: Arc::new(tokio::sync::Mutex::new(())),
    };
    // Build the runtime explicitly (rather than `#[tokio::main]`) so the sync
    // subcommands — `install`, `uninstall` — never spin one up needlessly.
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(server::serve(state))
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}

fn print_usage() {
    println!(
        "nestwatch — home remote control (LAN only)\n\n\
         USAGE:\n  \
           nestwatch install     set password + TLS cert, install the SYSTEM service\n  \
           nestwatch uninstall   stop + remove the service\n  \
           nestwatch run         run the HTTPS server in the foreground (dev)\n\n\
         Internal (invoked automatically):\n  \
           nestwatch service-run            SCM entry point for the service\n  \
           nestwatch helper --capture PATH  capture a screenshot in the user session\n"
    );
}
