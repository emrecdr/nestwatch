//! Home remote-control server for a child's Windows PC.
//!
//! The crate is organised in layers:
//! - `web` / `api` / `auth` / `security` — HTTP presentation (handlers, middleware, LAN gate).
//! - `state` / `error` — shared application state and the single error type.
//! - `curfew` / `rules` — the two background enforcers (curfew window; usage rules: screen-time
//!   budget, app blocklist, per-app limits).
//! - `audit` / `usage` / `timereq` / `jsonl` — append-only JSONL logs (security audit, usage
//!   history, the request-more-time queue) over a shared store.
//! - `control` / `session` / `helper` — `SystemControl`, the OS abstraction (real Windows +
//!   fake), plus the interactive-session helper (screenshot / lock).
//! - `config` / `cert` — persisted configuration and the self-signed TLS cert.
//! - `install` — one-time setup (password, cert, service, ACLs, firewall).
//!
//! Everything above `control` is OS-agnostic and runs (and is tested) on any platform.

pub mod api;
pub mod audit;
pub mod auth;
pub mod cert;
pub mod config;
pub mod control;
pub mod curfew;
pub mod error;
pub mod helper;
pub mod install;
pub mod jsonl;
pub mod rules;
pub mod security;
pub mod server;
pub mod state;
pub mod timereq;
pub mod usage;
pub mod web;

#[cfg(windows)]
pub mod service;
#[cfg(windows)]
pub mod session;

use anyhow::Result;

/// Parse `argv` and dispatch the requested subcommand.
pub fn run_cli() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("run");

    // The screenshot helper streams raw PNG bytes to stdout — do NOT initialize tracing (or
    // anything else that writes stdout) before handling it, or it would corrupt the stream.
    if cmd == "helper" {
        return run_helper(&args);
    }

    init_tracing(cmd);
    // rustls 0.23 requires a crypto provider to be installed. We build against the
    // `ring` provider (no C toolchain needed) and install it once at startup.
    let _ = rustls::crypto::ring::default_provider().install_default();

    match cmd {
        "install" => install::install(),
        "uninstall" => install::uninstall(),
        "run" => run_server(),
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

/// `helper --capture-stdout` (used by the service) or `helper --capture <path>` (dev):
/// capture a screenshot in the interactive user session.
fn run_helper(args: &[String]) -> Result<()> {
    match args.get(2).map(String::as_str) {
        Some("--capture-stdout") => helper::capture_to_stdout(),
        Some("--capture") => match args.get(3) {
            Some(path) => helper::capture_to_file(path),
            None => {
                eprintln!("usage: nestwatch helper --capture <path>");
                std::process::exit(2);
            }
        },
        Some("--lock") => helper::lock(),
        _ => {
            eprintln!("usage: nestwatch helper --capture-stdout | --capture <path> | --lock");
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
    let config = config::Config::load()?;
    let state = state::AppState::new(control::interactive_control(), config);
    // Build the runtime explicitly (rather than `#[tokio::main]`) so the sync
    // subcommands — `install`, `uninstall` — never spin one up needlessly.
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(server::serve(state))
}

/// Initialize logging. The interactive subcommands (`run`, `install`, `uninstall`) log to
/// **stdout** where a console exists. The `service-run` subcommand runs as the SYSTEM service
/// in Session 0 — which has **no console** — so its diagnostics would otherwise vanish; it logs
/// to a daily-rotated file in the ACL-hardened data dir instead, where a standard user can't
/// read them. Never called for `helper` (that path streams raw PNG to stdout).
fn init_tracing(cmd: &str) {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    if cmd == "service-run" {
        match service_log_appender() {
            // Blocking appender (no `WorkerGuard`): the release build aborts on panic, which
            // skips destructors — so a non-blocking guard's flush-on-drop wouldn't run exactly
            // when we most want the log. Diagnostics are low-volume, so blocking is fine.
            Ok(appender) => {
                fmt()
                    .with_env_filter(filter)
                    .with_writer(appender)
                    .with_ansi(false)
                    .init();
                return;
            }
            Err(e) => {
                // Log-file setup failed; fall back to stdout (invisible under the service, but
                // init must never abort the service) and record why.
                fmt().with_env_filter(filter).init();
                tracing::error!(error = %e, "could not open service log file; using stdout");
                return;
            }
        }
    }

    fmt().with_env_filter(filter).init();
}

/// A daily-rotated `service.<date>.log` in the data dir (retained ~2 weeks, best-effort).
fn service_log_appender() -> Result<tracing_appender::rolling::RollingFileAppender> {
    use tracing_appender::rolling::{Builder, Rotation};
    let dir = config::data_paths().dir;
    Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix("service")
        .filename_suffix("log")
        .max_log_files(14)
        .build(&dir)
        .map_err(|e| anyhow::anyhow!("building log appender in {}: {e}", dir.display()))
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
