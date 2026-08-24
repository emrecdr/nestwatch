//! Home remote-control server for a child's Windows PC.
//!
//! The crate is organised in layers:
//! - `web` / `api` / `auth` / `security` / `server` — HTTP presentation (handlers, middleware,
//!   LAN gate, router assembly and the TLS listener).
//! - `state` / `error` — shared application state and the single error type.
//! - `curfew` / `rules` — the two background enforcers (curfew window; usage rules: screen-time
//!   budget, app blocklist, per-app limits).
//! - `countdown` — when to give the child advance warning that a limit is approaching. Shared by
//!   both enforcers so "15 minutes left" behaves identically for screen time and for bedtime.
//! - `heartbeat` — last-completed-tick stamps for the two enforcers, so a silently dead one is
//!   visible in the dashboard instead of looking like an idle day.
//! - `clock` — tamper-resistant local time. A standard Windows user can change the time zone with
//!   no prompt, and both enforcers key off the date and the wall clock.
//! - `audit` / `usage` / `screentime` / `timereq` / `timecode` / `jsonl` — append-only JSONL logs
//!   (security audit, usage history, daily screen-time rollups, the request-more-time queue,
//!   redeemable time codes) over a shared store. Anything that folds one of these logs and then
//!   appends must hold that store's lock.
//! - `sessionstore` / `pairing` / `token` — persisted login sessions, the one-time QR pairing
//!   token, and the shared alphabet/RNG both it and time codes draw from.
//! - `control` / `session` / `helper` — `SystemControl`, the OS abstraction (real Windows +
//!   fake), plus the interactive-session helper (screenshot / lock).
//! - `config` / `cert` — persisted configuration and the self-signed TLS cert.
//! - `install` / `doctor` — one-time setup (password, cert, service, ACLs, firewall) and the
//!   read-only self-check that reports whether all of it is actually working.
//! - `service` / `syspath` — the Windows SCM entry point, and absolute paths to the system
//!   executables we shell out to (never a bare name, which Rust resolves by searching the
//!   application's own directory *before* `System32`). Windows only.
//!
//! Everything above `control` is OS-agnostic and runs (and is tested) on any platform.

pub mod api;
pub mod audit;
pub mod auth;
pub mod cert;
pub mod clock;
pub mod config;
pub mod control;
pub mod countdown;
pub mod curfew;
pub mod doctor;
pub mod error;
pub mod heartbeat;
pub mod helper;
pub mod install;
pub mod jsonl;
pub mod pairing;
pub mod preflight;
pub mod remotesetup;
pub mod rules;
pub mod screentime;
pub mod security;
pub mod server;
pub mod sessionstore;
pub mod state;
pub mod timecode;
pub mod timereq;
pub mod token;
pub mod usage;
pub mod web;

#[cfg(windows)]
pub mod service;
#[cfg(windows)]
pub mod session;
#[cfg(windows)]
pub mod syspath;

use anyhow::{Context, Result};

/// Parse `argv` and dispatch the requested subcommand.
/// The build's own version, from `Cargo.toml` at compile time.
///
/// `env!` bakes this in as an ordinary string constant, so `strip = true` in the release profile
/// cannot remove it — which was the point: before this existed, a binary sitting on the managed
/// PC could not be asked which build it was, and that is the first thing worth knowing when
/// something misbehaves or when checking whether a security fix is present.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

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
        "fingerprint" => print_fingerprint(),
        "pair" => print_pairing(),
        "doctor" | "status" => doctor::run(),
        "remote-setup" => print_remote_setup(),
        "version" | "--version" | "-V" => {
            println!("nestwatch {VERSION}");
            Ok(())
        }
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

/// `pair`: mint a fresh one-time pairing token and print its QR, for adding another device
/// (a second phone, a laptop) long after install without retyping an address or a passphrase.
///
/// Reads the port from the saved config so it always matches the running service.
fn print_pairing() -> Result<()> {
    // Minting writes into the ACL-locked data dir, so this needs elevation like install does.
    // Without the check it "succeeded" while printing no QR (the mint failure was logged at
    // debug, invisible by default) and guessed DEFAULT_PORT, so a `--port 9443` install got a
    // confidently wrong URL.
    install::ensure_elevated(
        "pair",
        "It reads the saved port and writes a one-time token into the protected data folder, \
         both of which require elevation.",
    )?;
    let port = config::Config::load()
        .context("reading the saved settings — is nestwatch installed?")?
        .port;
    install::print_access_block(port);
    Ok(())
}

/// `fingerprint`: print the installed cert's SHA-256 fingerprint, so a parent can verify it when
/// adding a new device long after install (when `install` printed it once).
fn print_fingerprint() -> Result<()> {
    let cert = config::data_paths().cert;
    let fp = cert::read_fingerprint(&cert)?;
    println!("TLS certificate SHA-256 fingerprint:\n{fp}");
    Ok(())
}

/// `remote-setup`: print the script that turns remote administration on (or `--off`).
///
/// Prints rather than runs. Enabling remote administration is a decision about the whole machine,
/// not about screen time, so it stays the parent's — to read first, then run. It also keeps this
/// tool from owning a general-purpose admin channel, and being responsible for one.
fn print_remote_setup() -> Result<()> {
    if std::env::args().any(|a| a == "--off") {
        print!("{}", remotesetup::teardown_script());
        return Ok(());
    }
    // The certificate has to carry the name that will be typed when connecting, and this is the
    // only place that knows it for certain.
    let host = hostname();
    eprintln!(
        "# Review this, then run it on THIS PC in an elevated PowerShell.\n\
         # Save it first if you prefer:  nestwatch remote-setup > setup.ps1\n\
         # Background and the risks it avoids: docs/REMOTE-UPDATE.md\n"
    );
    print!("{}", remotesetup::script(&host));
    Ok(())
}

/// This machine's name, as it must appear in the certificate and in the connect command.
fn hostname() -> String {
    // COMPUTERNAME on Windows, HOSTNAME elsewhere; the fallback is a visible placeholder rather
    // than a plausible-looking wrong name, which would fail confusingly at connect time.
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "THIS-PC-NAME".into())
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
    // Indentation comes from the spaces BEFORE each backslash: a Rust line continuation eats the
    // newline and all leading whitespace on the next source line, so the source cannot be laid
    // out to match the output. Keep them in step by eye.
    println!(
        "nestwatch {VERSION} — home remote control (LAN only)\n\n\
         USAGE:\n  \
           nestwatch install       set password + TLS cert, install the SYSTEM service\n      \
                                   --port N  listen on a port other than 8443\n      \
                                   --fix     apply pre-flight fixes without asking\n  \
           nestwatch uninstall     stop + remove the service (--purge also removes data)\n  \
           nestwatch doctor        check the install and report anything wrong\n  \
           nestwatch pair          show a QR code to sign in another phone or laptop\n  \
           nestwatch fingerprint   print the TLS cert SHA-256 (to verify a new device)\n  \
           nestwatch version       print this build's version\n  \
           nestwatch remote-setup  print a script enabling remote admin (--off to undo)\n  \
           nestwatch run           run the HTTPS server in the foreground (dev)\n\n\
         Internal (invoked automatically):\n  \
           nestwatch service-run            SCM entry point for the service\n  \
           nestwatch helper --capture PATH  capture a screenshot in the user session\n"
    );
}
