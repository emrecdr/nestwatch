//! The built `nestwatch` program, run the way a person or the service runs it.
//!
//! Every other test calls the library. Nothing ran the program itself, so the layer that turns a
//! command line into an action — `main`, `run_cli`, `run_helper` and the `helper` subcommands —
//! could do nothing at all and the suite still passed (`O109`). Each run here gets its own data
//! folder through `NESTWATCH_DATA_DIR`, which only a debug build honours, so nothing touches this
//! machine's real settings; and each is killed at a deadline, so a hang fails rather than stalls.

mod common;

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::ScratchDir;

/// Long enough for a debug build to start, capture and encode. A hang is what it is for.
const DEADLINE: Duration = Duration::from_secs(60);

/// What one run of the program left behind.
struct Ran {
    /// `None` only if a signal ended it.
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: String,
}

impl Ran {
    fn out(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// `nestwatch args…` with `data` as its data folder. `RUST_LOG` is removed so a developer's own
/// filter cannot hide a line a test reads.
fn command(data: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nestwatch"));
    cmd.args(args)
        .env("NESTWATCH_DATA_DIR", data)
        .env_remove("RUST_LOG")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Run `cmd` to completion, killing it at the deadline.
fn finish(mut cmd: Command, what: &str) -> Ran {
    let mut child = cmd.spawn().expect("the binary starts");
    // Drained on threads: a full-size JPEG is larger than a pipe's buffer, and a child blocked
    // writing to one would look exactly like the hang the deadline is for.
    let out = drain(child.stdout.take().unwrap());
    let err = drain(child.stderr.take().unwrap());
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("waiting on the binary") {
            break status;
        }
        if started.elapsed() > DEADLINE {
            let _ = child.kill();
            panic!("`nestwatch {what}` was still running after {DEADLINE:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Ran {
        code: status.code(),
        stdout: out.join().unwrap(),
        stderr: String::from_utf8_lossy(&err.join().unwrap()).into_owned(),
    }
}

fn drain(mut pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes)
            .expect("reading the binary's output");
        bytes
    })
}

fn nestwatch(data: &Path, args: &[&str]) -> Ran {
    finish(command(data, args), &args.join(" "))
}

/// The program answers for its own build. Any command would show that the binary dispatches at
/// all; this one also has nothing else to get wrong.
#[test]
fn the_version_names_this_build() {
    let dir = ScratchDir::new("cli-version");
    let ran = nestwatch(dir.path(), &["--version"]);
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    assert_eq!(
        ran.out(),
        format!("nestwatch {}\n", env!("CARGO_PKG_VERSION"))
    );
}

/// `remote-setup` prints the script for this machine's name, and `--off` the one that undoes it.
///
/// Neither ran before: a setup script carrying no name, or the teardown printed for both, passed.
/// The second is the inversion `check_flags` exists to stop — the script that *enables* remote
/// administration, saved under a name saying the opposite and then run elevated.
#[test]
fn remote_setup_prints_the_script_for_this_machine_and_off_prints_its_undoing() {
    let dir = ScratchDir::new("cli-remote");
    let mut cmd = command(dir.path(), &["remote-setup"]);
    cmd.env_remove("COMPUTERNAME")
        .env("HOSTNAME", "nw-cli-host");
    let on = finish(cmd, "remote-setup");
    assert_eq!(on.code, Some(0), "{}", on.stderr);
    assert!(
        on.out().contains("enable remote administration"),
        "{}",
        on.out()
    );
    assert!(on.out().contains("for: nw-cli-host\n"), "{}", on.out());

    let off = nestwatch(dir.path(), &["remote-setup", "--off"]);
    assert_eq!(off.code, Some(0), "{}", off.stderr);
    assert!(off.out().contains("turn remote administration back off"));
    assert!(!off.out().contains("enable remote administration"));
}

/// `fingerprint` reads the installed certificate, and says so when there is none.
#[test]
fn the_fingerprint_is_the_installed_certificates_and_a_missing_one_is_an_error() {
    let dir = ScratchDir::new("cli-fingerprint");
    let missing = nestwatch(dir.path(), &["fingerprint"]);
    assert_eq!(missing.code, Some(1), "{}", missing.out());
    assert!(
        missing.stderr.contains("reading cert"),
        "{}",
        missing.stderr
    );

    let fp = nestwatch::cert::generate(
        &dir.join("cert.pem"),
        &dir.join("key.pem"),
        &["localhost".to_string()],
    )
    .unwrap();
    let ran = nestwatch(dir.path(), &["fingerprint"]);
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    assert_eq!(
        ran.out(),
        format!("TLS certificate SHA-256 fingerprint:\n{fp}\n")
    );
    // 32 bytes as colon-separated hex, so the comparison above is not two empty strings agreeing.
    assert_eq!(fp.split(':').count(), 32, "{fp}");
}

/// `run` with nothing installed says what to do instead of exiting as if it had served.
#[test]
fn run_without_settings_says_to_install_first() {
    let dir = ScratchDir::new("cli-run-bare");
    let ran = nestwatch(dir.path(), &["run"]);
    assert_eq!(ran.code, Some(1), "{}", ran.out());
    assert!(
        ran.stderr.contains("run `nestwatch install` first"),
        "{}",
        ran.stderr
    );
}

/// Everything here runs the platform controller, which off Windows is the fake: on a Windows
/// runner `--lock` would lock the runner, `--watch` would never return, and `pair` would depend
/// on whether the runner's console is elevated.
#[cfg(not(windows))]
mod off_windows {
    use std::io::BufRead;
    use std::sync::mpsc;

    use nestwatch::config::Config;

    use super::*;

    /// `config` written where `nestwatch` will read it.
    fn install_config(dir: &ScratchDir, config: &Config) {
        std::fs::write(
            dir.join("config.json"),
            serde_json::to_string(config).unwrap(),
        )
        .unwrap();
    }

    /// Width and height of the JPEG in `bytes`.
    fn size(bytes: &[u8]) -> (u32, u32) {
        let img = image::load_from_memory(bytes).expect("a decodable image");
        (img.width(), img.height())
    }

    /// The service reads the screenshot off the helper's stdout, at the tier it asked for. The
    /// fake draws 1280x720, and a preview of that is 960x540 — the box it is fitted to.
    ///
    /// A `--tier` that was never found, or a value read from beside it, sent every preview at full
    /// size; an empty or one-byte image went down the pipe as a success (`O109`).
    #[test]
    fn the_helper_streams_a_jpeg_at_the_tier_it_was_asked_for() {
        let dir = ScratchDir::new("cli-stdout");
        let full = nestwatch(dir.path(), &["helper", "--capture-stdout"]);
        assert_eq!(full.code, Some(0), "{}", full.stderr);
        assert_eq!(size(&full.stdout), (1280, 720));

        let preview = nestwatch(
            dir.path(),
            &["helper", "--capture-stdout", "--tier", "preview"],
        );
        assert_eq!(preview.code, Some(0), "{}", preview.stderr);
        assert_eq!(size(&preview.stdout), (960, 540));
    }

    /// `--capture <path>` writes the image there and nothing to stdout, with `--tier` after the
    /// path — the form where reading the value beside `--tier` reads the path instead.
    #[test]
    fn the_helper_writes_a_capture_to_the_path_it_is_given() {
        let dir = ScratchDir::new("cli-file");
        let shot = dir.join("shot.jpg");
        let ran = nestwatch(
            dir.path(),
            &[
                "helper",
                "--capture",
                shot.to_str().unwrap(),
                "--tier",
                "preview",
            ],
        );
        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        assert!(ran.stdout.is_empty());
        assert_eq!(
            size(&std::fs::read(&shot).expect("the capture was written")),
            (960, 540)
        );
    }

    /// `--lock` succeeds silently. The service runs it in the child's session, and a usage error
    /// there means a lock that never happens.
    #[test]
    fn the_helper_locks_without_a_word() {
        let dir = ScratchDir::new("cli-lock");
        let ran = nestwatch(dir.path(), &["helper", "--lock"]);
        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        assert!(ran.stdout.is_empty());
    }

    /// Off Windows there is no watcher, and `--watch` says so rather than reporting an empty
    /// desktop forever — or exiting as if it had watched.
    #[test]
    fn the_watcher_refuses_to_run_off_windows() {
        let dir = ScratchDir::new("cli-watch");
        let ran = nestwatch(dir.path(), &["helper", "--watch"]);
        assert_eq!(ran.code, Some(1), "{}", ran.out());
        assert!(ran.stderr.contains("Windows-only"), "{}", ran.stderr);
    }

    /// `service-run` is the Service Control Manager's entry point and nothing else's.
    #[test]
    fn the_service_entry_point_refuses_to_run_off_windows() {
        let dir = ScratchDir::new("cli-service");
        let ran = nestwatch(dir.path(), &["service-run"]);
        assert_eq!(ran.code, Some(1), "{}", ran.out());
        assert!(
            ran.stderr.contains("only supported on Windows"),
            "{}",
            ran.stderr
        );
    }

    /// `pair --integration` checks the name before anything else, and a good name gets past it.
    ///
    /// `parent` is the one name a scoped token must never carry. Finding the flag at the wrong
    /// place, or reading the word beside it, named the token after `pair` or `--integration` —
    /// both valid — and the refusal never came (`O109`).
    #[test]
    fn pair_refuses_a_name_no_integration_may_have_and_passes_a_good_one() {
        let dir = ScratchDir::new("cli-pair");
        let refused = nestwatch(dir.path(), &["pair", "--integration", "parent"]);
        assert_eq!(refused.code, Some(1), "{}", refused.out());
        assert!(
            refused
                .stderr
                .contains("an integration name is 1-32 characters"),
            "{}",
            refused.stderr
        );

        // Nothing is installed here, so a good name runs on into the settings it cannot read.
        for args in [&["pair", "--integration", "studygo"][..], &["pair"][..]] {
            let ran = nestwatch(dir.path(), args);
            assert_eq!(ran.code, Some(1), "{}", ran.out());
            assert!(
                ran.stderr.contains("is nestwatch installed?"),
                "`{}`: {}",
                args.join(" "),
                ran.stderr
            );
        }
    }

    /// `run` serves, and says where, on the console it was started from.
    ///
    /// The service logs to a file instead, because it has no console. Pointing the interactive
    /// commands at that file, or setting up no logging at all, left `run` serving in silence and
    /// passed (`O109`). Port 0 lets the system choose, so this never collides with anything.
    #[test]
    fn run_announces_where_it_is_serving_on_its_console() {
        let dir = ScratchDir::new("cli-run");
        install_config(
            &dir,
            &Config {
                port: 0,
                ..common::test_config()
            },
        );

        let mut child = command(dir.path(), &["run"])
            .spawn()
            .expect("the binary starts");
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
            {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let started = Instant::now();
        let mut seen = Vec::new();
        let announced = loop {
            let left = DEADLINE.saturating_sub(started.elapsed());
            match rx.recv_timeout(left) {
                Ok(line) if line.contains("listening on https://0.0.0.0:0") => break true,
                Ok(line) => seen.push(line),
                Err(_) => break false,
            }
        };
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            announced,
            "`run` never said where it was serving: {seen:#?}"
        );
    }

    /// `doctor` on a machine with nothing installed names what is missing and exits failing. It is
    /// where the README sends a parent when something looks wrong, and nothing ran it (`O109`).
    #[test]
    fn doctor_on_a_bare_machine_names_what_is_missing_and_fails() {
        let dir = ScratchDir::new("cli-doctor-bare");
        let ran = nestwatch(dir.path(), &["doctor"]);
        let out = ran.out();
        assert_eq!(ran.code, Some(1), "{out}");
        assert!(out.contains("not installed — no config found"), "{out}");
        assert!(out.contains("no TLS certificate"), "{out}");
        // Off Windows every console counts as elevated, so nothing may be put down to that.
        assert!(!out.contains("not an elevated console"), "{out}");
        assert!(
            out.contains("running on a non-Windows host (dev mode)"),
            "{out}"
        );
    }

    /// A settings file that is there and does not parse is named as that, not as no install.
    #[test]
    fn doctor_names_a_settings_file_it_cannot_parse() {
        let dir = ScratchDir::new("cli-doctor-corrupt");
        std::fs::write(dir.join("config.json"), "{ not json").unwrap();
        let out = nestwatch(dir.path(), &["doctor"]).out();
        assert!(
            out.contains("config.json at") && out.contains("is unreadable"),
            "{out}"
        );
        assert!(!out.contains("not installed"), "{out}");
    }

    /// The install record is read even when the settings are gone, because it says which build put
    /// them there.
    #[test]
    fn doctor_reads_the_install_record_without_the_settings() {
        let dir = ScratchDir::new("cli-doctor-record");
        let version = env!("CARGO_PKG_VERSION");
        std::fs::write(
            dir.join("installed.json"),
            format!(r#"{{"version":"{version}"}}"#),
        )
        .unwrap();
        let out = nestwatch(dir.path(), &["doctor"]).out();
        assert!(
            out.contains(&format!("installed version {version} matches this binary")),
            "{out}"
        );
    }

    /// A healthy install passes: settings, a port something listens on, a fresh certificate shown
    /// by the ends of its fingerprint, and an exit that says so.
    #[test]
    fn doctor_passes_a_healthy_install() {
        let dir = ScratchDir::new("cli-doctor-healthy");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        install_config(
            &dir,
            &Config {
                port,
                ..common::test_config()
            },
        );
        nestwatch::cert::generate(
            &dir.join("cert.pem"),
            &dir.join("key.pem"),
            &["localhost".to_string()],
        )
        .unwrap();

        let ran = nestwatch(dir.path(), &["doctor"]);
        let out = ran.out();
        // `doctor` exits failing exactly when a line failed, so this is "no problems" too.
        assert_eq!(ran.code, Some(0), "{out}");
        assert!(
            out.contains(&format!("something is listening on port {port}")),
            "{out}"
        );
        assert!(out.contains("valid for about"), "{out}");
        assert!(out.contains("fingerprint starts"), "{out}");
        drop(listener);
    }
}
