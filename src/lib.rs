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
pub mod foreground;
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

#[cfg(windows)]
pub mod watcher;

use anyhow::{Context, Result};

/// The build's own version, from `Cargo.toml` at compile time.
///
/// `env!` bakes this in as an ordinary string constant, so `strip = true` in the release profile
/// cannot remove it — which was the point: before this existed, a binary sitting on the managed
/// PC could not be asked which build it was, and that is the first thing worth knowing when
/// something misbehaves or when checking whether a security fix is present.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Parse `argv` and dispatch the requested subcommand.
/// Which options each subcommand accepts, and which of them take a value.
///
/// Every flag in this crate is read where it matters, with `args.iter().any(|a| a == "--x")`.
/// That treats a typo as absence. For `--purge` the mistake is a harmless no-op; for two others
/// it is not:
///
/// - `remote-setup` has two modes and `--off` chooses between them, so
///   `remote-setup --of > teardown.ps1` writes the script that *enables* remote administration
///   into a file named teardown — which the parent then runs, elevated.
/// - `install --prot 9000` installs on the default port and says nothing, so the dashboard is
///   not where they were told it would be.
///
/// Refusing unrecognised options is the settled convention for exactly this reason. One table,
/// so a new flag that is not listed here fails loudly the first time it is used rather than at
/// whichever call site reads it.
struct Accepts {
    bare: &'static [&'static str],
    valued: &'static [&'static str],
}

fn accepts(cmd: &str) -> Option<Accepts> {
    let a = |bare, valued| Some(Accepts { bare, valued });
    match cmd {
        "install" => a(&["--fix", "--reset-config", "--new-cert"], &["--port"]),
        "uninstall" => a(&["--purge"], &[]),
        "remote-setup" => a(&["--off"], &[]),
        "run" | "doctor" | "status" | "pair" | "fingerprint" => a(&[], &[]),
        "version" | "--version" | "-V" | "help" | "--help" | "-h" => a(&[], &[]),
        // Deliberately unchecked:
        // `helper` already rejects anything it does not recognise, and is handled before this
        // point so nothing can write to the stdout it streams a JPEG on.
        // `service-run` is started by the SCM, not typed. A wrong entry in this table would turn
        // into a service that refuses to start — much worse than the typo it would catch.
        // An unknown command falls through here and is reported by the dispatch below.
        _ => None,
    }
}

/// Check `args` against [`accepts`]. `Err` is the message to print; pure, so it is testable.
fn check_flags(cmd: &str, args: &[String]) -> Result<(), String> {
    let Some(ok) = accepts(cmd) else {
        return Ok(());
    };
    let mut i = 2; // argv[0] is the program, argv[1] the subcommand.
    while i < args.len() {
        let arg = args[i].as_str();
        if ok.valued.contains(&arg) {
            // Skip the value unexamined: it is data, and may legitimately begin with a dash.
            i += 2;
            continue;
        }
        if ok.bare.contains(&arg) {
            i += 1;
            continue;
        }
        let known: Vec<&str> = ok.bare.iter().chain(ok.valued).copied().collect();
        return Err(if known.is_empty() {
            format!("unknown option `{arg}`: `{cmd}` takes no options.")
        } else {
            format!(
                "unknown option `{arg}` for `{cmd}`.\nIt accepts: {}",
                known.join(", ")
            )
        });
    }
    Ok(())
}

pub fn run_cli() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("run");

    // The screenshot helper streams raw JPEG bytes to stdout — do NOT initialize tracing (or
    // anything else that writes stdout) before handling it, or it would corrupt the stream.
    if cmd == "helper" {
        return run_helper(&args);
    }

    init_tracing(cmd);
    // rustls 0.23 requires a crypto provider to be installed. We build against the
    // `ring` provider (no C toolchain needed) and install it once at startup.
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Before anything acts on them. Exit 2 is the usage-error code the unknown-command arm below
    // already uses.
    if let Err(msg) = check_flags(cmd, &args) {
        eprintln!("{msg}\n");
        print_usage();
        std::process::exit(2);
    }

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
///
/// `--tier preview|full` may follow either capture form. It is optional and defaults to `full`,
/// which is what this subcommand did before tiers existed — see [`control::ShotTier::from_arg`]
/// for why an unrecognised value defaults the same way rather than failing.
fn run_helper(args: &[String]) -> Result<()> {
    // Scanned rather than positional: `--tier` is optional and follows a `--capture <path>` that
    // already consumes an argument of its own, so a fixed index would read the path on one form and
    // the flag on the other.
    let tier = control::ShotTier::from_arg(
        args.iter()
            .position(|a| a == "--tier")
            .and_then(|i| args.get(i + 1))
            .map(String::as_str),
    );

    match args.get(2).map(String::as_str) {
        Some("--capture-stdout") => helper::capture_to_stdout(tier),
        Some("--capture") => match args.get(3) {
            Some(path) => helper::capture_to_file(path, tier),
            None => {
                eprintln!("usage: nestwatch helper --capture <path> [--tier preview|full]");
                std::process::exit(2);
            }
        },
        Some("--lock") => helper::lock(),
        Some("--watch") => helper::watch(),
        _ => {
            eprintln!(
                "usage: nestwatch helper --capture-stdout [--tier preview|full] | \
                 --capture <path> [--tier preview|full] | --lock | --watch"
            );
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
/// read them. Never called for `helper` (that path streams raw JPEG to stdout).
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
    // A raw string, laid out exactly as it prints. The previous form built this from escaped
    // line-continuations, where the indentation came from the spaces *before* each backslash --
    // so the source looked nothing like the output and had to be kept in step by eye. It wasn't:
    // the two `install` flags below were left at column 7 instead of aligned under the
    // description column, and nothing catches that but running the binary.
    println!(
        r#"nestwatch {VERSION} — home remote control (LAN only)

USAGE:
  nestwatch install       set password + TLS cert, install the SYSTEM service
                          --port N        listen on a port other than 8443
                          --fix           apply pre-flight fixes without asking
                          --reset-config  replace an unreadable config.json
  nestwatch uninstall     remove the service, firewall rule and files; fails if any
                          remain, naming them (--purge also removes settings + history)
  nestwatch doctor        check the install and report anything wrong
  nestwatch pair          show a QR code to sign in another phone or laptop
  nestwatch fingerprint   print the TLS cert SHA-256 (to verify a new device)
  nestwatch version       print this build's version
  nestwatch remote-setup  print a script enabling remote admin (--off to undo)
  nestwatch run           run the HTTPS server in the foreground (dev)

Internal (invoked automatically):
  nestwatch service-run            SCM entry point for the service
  nestwatch helper --capture PATH  capture a screenshot in the user session
                                   (add --tier preview for the small live-view size)
  nestwatch helper --watch         measure which app has focus (runs while signed in)
"#
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        std::iter::once("nestwatch")
            .chain(parts.iter().copied())
            .map(str::to_string)
            .collect()
    }

    /// The typo that inverts the operation.
    ///
    /// `remote-setup` prints the teardown script with `--off` and the *setup* script without it.
    /// Ignoring an unrecognised option meant `remote-setup --of > teardown.ps1` produced the
    /// script that enables remote administration, under a filename saying the opposite — and the
    /// next step in the guide is to run that file, elevated.
    #[test]
    fn a_mistyped_off_is_refused_rather_than_inverted() {
        assert!(check_flags("remote-setup", &argv(&["remote-setup", "--of"])).is_err());
        assert!(check_flags("remote-setup", &argv(&["remote-setup", "-off"])).is_err());
        assert!(check_flags("remote-setup", &argv(&["remote-setup", "--off"])).is_ok());
        assert!(check_flags("remote-setup", &argv(&["remote-setup"])).is_ok());
    }

    /// A mistyped `--port` used to install on 8443 without a word about it.
    #[test]
    fn a_mistyped_port_is_refused_and_a_real_one_keeps_working() {
        assert!(check_flags("install", &argv(&["install", "--prot", "9000"])).is_err());
        assert!(check_flags("install", &argv(&["install", "--port", "9000"])).is_ok());
        // The value is data and is never examined: a port is not a flag, even if it looks like one.
        assert!(check_flags("install", &argv(&["install", "--port", "--fix"])).is_ok());
        // Every real install flag, together.
        assert!(
            check_flags(
                "install",
                &argv(&[
                    "install",
                    "--fix",
                    "--new-cert",
                    "--reset-config",
                    "--port",
                    "9000"
                ])
            )
            .is_ok()
        );
    }

    /// Commands that take nothing say so, and the message names the command.
    #[test]
    fn a_command_with_no_options_says_so() {
        let err = check_flags("doctor", &argv(&["doctor", "--verbose"])).unwrap_err();
        assert!(
            err.contains("--verbose"),
            "name the offending option: {err}"
        );
        assert!(err.contains("takes no options"), "{err}");

        let err = check_flags("uninstall", &argv(&["uninstall", "--purgee"])).unwrap_err();
        assert!(err.contains("--purge"), "list what it does accept: {err}");
    }

    /// `helper`'s flags are exempt from the table, so its usage message is the only place a
    /// person can learn them — which makes that message load-bearing rather than decorative.
    ///
    /// This exists because `every_flag_the_code_reads_is_listed_in_the_table` skips `run_helper`
    /// for good reasons, and a skip with nothing behind it is how an undocumented flag ships. The
    /// claim in that skip's comment — "their declaration is the usage message" — is checked here
    /// instead of merely asserted there.
    #[test]
    fn every_helper_flag_is_named_in_the_usage_it_prints() {
        let text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"),
        )
        .expect("reading src/lib.rs")
        // Every split below anchors on "\n", so a CRLF checkout makes `split_once("\n}\n")`
        // miss the closing brace: `}` is followed by `\r`, not `\n`. That is not theoretical —
        // it is how the first v0.3.0 tag failed, with 335 tests green and this one panicking
        // "run_helper must end" on a Windows runner and nowhere else. `.gitattributes` now pins
        // LF repo-wide, but it cannot renormalise a working tree that predates it, so this test
        // does not depend on a checkout setting it has no way to see.
        .replace("\r\n", "\n");
        let body = text
            .split_once("\nfn run_helper(")
            .expect("run_helper must exist")
            .1;
        let body = body.split_once("\n}\n").expect("run_helper must end").0;

        // What the function dispatches on, and what it prints when nothing matches.
        //
        // A usage message is gathered from its first line to the `)` that closes the `eprintln!`,
        // because these wrap: the longest one carries `--lock` and `--watch` on a continuation
        // line that contains no "usage:" of its own. Reading only the first line found exactly
        // that, and reported two documented flags as undocumented.
        let (mut flags, mut usage) = (Vec::new(), String::new());
        let mut in_usage = false;
        for line in body.lines() {
            in_usage = (in_usage || line.contains("usage:")) && !line.contains(");");
            if in_usage || line.contains("usage:") {
                usage.push_str(line);
            }
            for idiom in ["Some(\"--", "a == \"--"] {
                if let Some((_, rest)) = line.split_once(idiom)
                    && let Some((flag, _)) = rest.split_once('"')
                {
                    flags.push(format!("--{flag}"));
                }
            }
        }

        assert!(
            flags.len() >= 4,
            "found only {} flags in run_helper — the scan drifted and proves nothing: {flags:?}",
            flags.len()
        );
        for flag in &flags {
            assert!(
                usage.contains(flag.as_str()),
                "`{flag}` is accepted by run_helper but named in no usage message, so the only \
                 way to discover it is to read the source"
            );
        }
    }

    /// Commands whose arguments this table must not police.
    #[test]
    fn the_internal_commands_are_left_alone() {
        // `helper` validates its own, and must not be intercepted before it streams JPEG bytes.
        assert!(check_flags("helper", &argv(&["helper", "--capture-stdout"])).is_ok());
        // `service-run` is started by the SCM. Refusing here would mean a service that won't run.
        assert!(check_flags("service-run", &argv(&["service-run", "--whatever"])).is_ok());
    }

    /// The table cannot fall behind the code that reads the flags.
    ///
    /// Every flag is consumed somewhere by `args.iter().any(|a| a == "--x")`, far from here. A
    /// flag added there but not listed above would be rejected the first time anyone used it —
    /// the failure lands on the user, not on CI. So the list is checked against the call sites,
    /// the same way `tests/spawn_paths.rs` checks `syspath` against its own, and for the same
    /// reason: a hand-maintained copy of what the code does is a copy that goes stale.
    #[test]
    fn every_flag_the_code_reads_is_listed_in_the_table() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut known: Vec<&str> = Vec::new();
        for cmd in [
            "install",
            "uninstall",
            "remote-setup",
            "run",
            "doctor",
            "status",
            "pair",
            "fingerprint",
            "version",
            "help",
        ] {
            let a = accepts(cmd).expect("every user-facing command must be in the table");
            known.extend(a.bare);
            known.extend(a.valued);
        }

        let mut checked = 0usize;
        let mut missing = Vec::new();
        for entry in std::fs::read_dir(&src).expect("reading src/").flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("reading a source file");
            // Which top-level function the scan is currently inside. Only `run_helper` matters,
            // and only because the flags it reads belong to `helper` — a command dispatched
            // *before* `check_flags` exists in the call path, and deliberately absent from
            // `accepts` (see `the_internal_commands_are_left_alone`). Demanding its flags be
            // listed in the table would mean adding entries to a table whose whole point is not
            // to police them. Their user-facing declaration is `run_helper`'s own usage message,
            // which is what a person actually sees when they get one wrong.
            //
            // This opens no hole: `run_helper` handles exactly one command, so no user-facing
            // flag can hide in here.
            let mut in_run_helper = false;
            for (n, line) in text.lines().enumerate() {
                if let Some(rest) = line.strip_prefix("fn ").or(line.strip_prefix("pub fn ")) {
                    in_run_helper = rest.starts_with("run_helper");
                }
                if in_run_helper {
                    continue;
                }
                // Prose mentions the idiom without being a call site — the same exclusion
                // `tests/spawn_paths.rs` needs for the same reason.
                if line.trim_start().starts_with("//") {
                    continue;
                }
                // The one idiom every flag is read with: `|a| a == "--flag"`.
                let Some(rest) = line.split_once("a == \"--") else {
                    continue;
                };
                let Some((flag, _)) = rest.1.split_once('"') else {
                    continue;
                };
                checked += 1;
                let flag = format!("--{flag}");
                if !known.contains(&flag.as_str()) {
                    missing.push(format!("  {}:{} — {flag}", path.display(), n + 1));
                }
            }
        }

        assert!(
            checked >= 6,
            "found only {checked} flag reads — the scan pattern has drifted from the code and \
             this test is no longer checking anything"
        );
        assert!(
            missing.is_empty(),
            "these flags are read by the code but absent from `accepts`, so using them would be \
             refused:\n{}",
            missing.join("\n")
        );
    }
}
