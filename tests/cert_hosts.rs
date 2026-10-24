//! The machine name `reachable_hosts` reports comes from the environment, so this is its own test
//! binary: environment variables are process-wide, and a second test reading them meanwhile would
//! race this one — the cause of the flaky password test fixed on 2026-10-23.
//!
//! No test set the variables before (`docs/OPEN-FINDINGS.md` O109), so a `hostname` that reported
//! nothing, or a made-up name, or kept only empty ones, passed. The name matters: it is a SAN on
//! the certificate and the stable half of the address `install` prints.

use nestwatch::cert::reachable_hosts;

/// The last entry `reachable_hosts` returns. The first, when there is one, is this machine's LAN
/// address, which depends on the network the test runs on.
fn named() -> Option<String> {
    reachable_hosts()
        .into_iter()
        .rfind(|h| h.parse::<std::net::IpAddr>().is_err())
}

#[test]
fn the_machine_name_comes_from_the_environment_and_an_empty_one_is_no_name() {
    // SAFETY: the only test in this binary, so nothing else reads the environment meanwhile.
    unsafe {
        std::env::remove_var("COMPUTERNAME");
        std::env::set_var("HOSTNAME", "kitchen-pc");
    }
    assert_eq!(named().as_deref(), Some("kitchen-pc"));

    // SAFETY: as above.
    unsafe { std::env::set_var("COMPUTERNAME", "STUDY-PC") };
    assert_eq!(
        named().as_deref(),
        Some("STUDY-PC"),
        "Windows' own variable is the one to trust where it is set"
    );

    // SAFETY: as above.
    unsafe {
        std::env::remove_var("COMPUTERNAME");
        std::env::set_var("HOSTNAME", "");
    }
    assert_eq!(named(), None, "an empty name is no name, not a SAN of \"\"");
}
