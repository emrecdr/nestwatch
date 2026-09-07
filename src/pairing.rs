//! One-time pairing tokens — scan a QR code and land in the dashboard already signed in.
//!
//! Typing `https://192.168.1.42:8443` plus a passphrase on a phone keyboard is the single
//! biggest piece of friction in first-time setup, and mistyping the IP is the most likely way
//! to end up staring at a router's admin page instead. A pairing token removes both: `install`
//! prints a QR, the parent scans it, and the redirect target logs them in.
//!
//! **Why a file.** `install` and `nestwatch pair` run in a *different process* from the service
//! (and, on Windows, as a different user), so the token can't live in shared memory. It's handed
//! over through a small file in the ACL-locked data dir, which SYSTEM and Administrators can
//! read but the child cannot.
//!
//! **What's stored.** Only a SHA-256 of the token, so the file on disk never grants access even
//! if it's somehow read. Tokens are single-use — redeeming deletes the file — and expire after
//! [`TTL_SECS`]. Both matter because the token is printed on a console *on the child's own PC*:
//! the exposure window is "while the parent is standing at the machine", and it closes the
//! instant they scan. See `docs/SECURITY.md`.

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::token;

/// Token length in characters — 16 × 5 bits = 80 bits of entropy. Far beyond brute-forcing
/// within the 15-minute window, while still short enough to type if the QR won't scan.
const TOKEN_LEN: usize = 16;

/// How long a freshly minted token stays valid.
pub const TTL_SECS: u64 = 15 * 60;

/// Serializes [`redeem`]'s read → verify → consume so a token can be spent exactly once.
///
/// An earlier version relied on `remove_file` being the arbiter — "whoever unlinks it wins".
/// That is **false**: measured on macOS, 8 threads racing `remove_file` on one path all get
/// `Ok` in the majority of rounds, so every concurrent scan of the same QR was granted a
/// session. This is the same defect, and the same fix, as the time-code double-redeem
/// (see [`crate::timecode`]): hold a lock across the whole sequence.
///
/// A process-local lock is sufficient because only the service ever redeems; `install` and
/// `pair` merely write the file.
static REDEEM_GATE: Mutex<()> = Mutex::new(());

/// What redeeming a token is allowed to produce.
///
/// **Chosen when the QR is minted, not derived from the fact of pairing** — and that is the
/// whole design, arrived at by trying the other way first. A mark meaning "this session came
/// from a QR" would have described `nestwatch-mobile` too: it redeems this same one-time token
/// through its own pairing screen, and it is a full parent dashboard whose paths are most of
/// what such a mark would refuse. Deriving authority from the redemption would not narrow an
/// integration; it would disable that client. So the *minting* side says what the credential is
/// for, and redemption only carries it across.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Scope {
    /// Everything an authenticated parent can do. What `nestwatch pair` mints, what a password
    /// login is worth, and what the browser and the Android app both need.
    Dashboard,
    /// One integration, allowed only to push earned time as `source` and to read today's total
    /// back. Cannot reach the rest of the API, and cannot grant as anyone but itself.
    Integration { source: String },
}

#[derive(Serialize, Deserialize)]
struct Pending {
    /// Hex SHA-256 of the token — never the token itself.
    hash: String,
    /// Unix seconds after which the token is refused.
    expires_at: u64,
    /// What this token is allowed to become.
    ///
    /// **Deliberately not `#[serde(default)]`.** A file written before this field existed fails
    /// to parse, which [`redeem`] already treats as corrupt: it clears the file and refuses. That
    /// is the fail-closed direction, and it costs nothing real — a pending token lives fifteen
    /// minutes and is minted by a parent standing at the machine, so the whole remedy is to run
    /// `nestwatch pair` again. A default would have silently promoted an unknown token to full
    /// authority, which is the exact class of bug this field exists to close.
    scope: Scope,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn digest(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Mint a pairing token, persist its hash to `path`, and return the plaintext to display.
///
/// Replaces any previous pending token, so minting again invalidates the earlier QR — running
/// `nestwatch pair` twice can never leave two live tokens outstanding.
pub fn mint(path: &Path, scope: Scope) -> Result<String> {
    let plaintext = token::random(TOKEN_LEN);
    let pending = Pending {
        hash: digest(&plaintext),
        expires_at: now_secs() + TTL_SECS,
        scope,
    };
    let json = serde_json::to_string(&pending).context("serializing pairing token")?;
    crate::config::write_atomic(path, json.as_bytes())
        .with_context(|| format!("writing pairing token to {}", path.display()))?;
    Ok(plaintext)
}

/// Consume `token` if it matches the pending, unexpired pairing, returning **what it is worth**.
/// Single-use: a successful redemption deletes the file, as does encountering an expired one.
///
/// Returns `None` for anything else (no pending token, mismatch, expired, unreadable, or written
/// before tokens carried a [`Scope`]). Callers must not distinguish these to the client — see the
/// handler in `web.rs`.
///
/// Returning the scope rather than a bool is what makes the authority decision impossible to
/// forget: there is no success value that does not say what was granted.
pub fn redeem(path: &Path, supplied: &str) -> Option<Scope> {
    let supplied = token::normalize(supplied);
    if supplied.is_empty() {
        return None;
    }
    // Held across read → verify → consume. Without it, concurrent scans each observe the token
    // as valid and each get a session (see REDEEM_GATE).
    let _gate = REDEEM_GATE.lock().unwrap_or_else(|p| p.into_inner());
    let Ok(raw) = std::fs::read_to_string(path) else {
        return None;
    };
    let Ok(pending) = serde_json::from_str::<Pending>(&raw) else {
        // Corrupt file, or one written before tokens carried a scope: clear it so a stuck token
        // can't block future pairings, and refuse. Refusing an unreadable token is the only safe
        // reading — the alternative is guessing what authority it meant.
        let _ = std::fs::remove_file(path);
        return None;
    };
    if now_secs() >= pending.expires_at {
        let _ = std::fs::remove_file(path);
        return None;
    }
    // Compare digests, not the tokens themselves — the stored side is a hash by design.
    if !token::eq_ct(&digest(&supplied), &pending.hash) {
        return None;
    }
    // Consume inside the gate, so the next scan of this QR finds nothing. It is [`REDEEM_GATE`]
    // — *not* the unlink — that makes this single-use; `remove_file` is not exclusive.
    std::fs::remove_file(path).ok().map(|()| pending.scope)
}

/// Discard any pending token. Used by `uninstall`; a successful [`redeem`] already unlinks.
pub fn clear(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Render `url` as a QR code using Unicode half-blocks, sized for a console.
///
/// Returns `None` if the URL won't fit in a QR code (absurdly long host), so the caller can
/// fall back to printing the plain URL — never a reason to fail an install.
pub fn qr_code(url: &str) -> Option<String> {
    use qrcode::QrCode;
    use qrcode::render::unicode;

    let code = QrCode::new(url).ok()?;
    Some(
        code.render::<unicode::Dense1x2>()
            // Explicit light/dark: the default renders assuming a light terminal, which comes
            // out inverted (and unscannable) in the dark consoles most people use.
            .dark_color(unicode::Dense1x2::Light)
            .light_color(unicode::Dense1x2::Dark)
            .quiet_zone(true)
            .build(),
    )
}

/// Render `url` as an inline SVG QR code, for a browser rather than a console.
///
/// [`qr_code`] above renders Unicode half-blocks, which is right for the terminal `nestwatch pair`
/// prints to and unscannable from a web page. This is the same matrix drawn as one `<path>`.
///
/// **Black on white, whatever the dashboard's theme is, and that is not an oversight.** A QR needs
/// dark-on-light contrast and a quiet zone to decode; a themed code that inverted in dark mode
/// would render beautifully and scan for nobody. The white ground is painted explicitly for the
/// same reason — a transparent SVG would sit on whatever the card behind it is.
///
/// The URL is encoded as QR *data*, never interpolated into markup: the only text that reaches the
/// SVG source is the generated path. So a caller may put this in a markup sink without the token
/// or the hostname being able to close a tag.
///
/// `None` on the same condition as [`qr_code`] — a URL too long to encode — so a caller falls back
/// to showing the link as text rather than failing the mint over a picture.
pub fn qr_svg(url: &str) -> Option<String> {
    use std::fmt::Write as _;

    use qrcode::{Color, QrCode};

    let code = QrCode::new(url).ok()?;
    let width = code.width();
    // Four modules, which is what the spec requires; a code flush to the edge of its container
    // decodes far less reliably.
    const QUIET: usize = 4;
    let side = width + QUIET * 2;

    let mut path = String::new();
    for (i, color) in code.to_colors().into_iter().enumerate() {
        if color == Color::Dark {
            let x = i % width + QUIET;
            let y = i / width + QUIET;
            // One 1×1 square per dark module, all in a single path: an SVG with a few hundred
            // <rect> elements costs the same picture and far more DOM.
            //
            // `write!` rather than `push_str(&format!(...))`: this runs once per dark module —
            // several hundred times for a pairing URL — and the `format!` form allocates a
            // throwaway `String` each pass only to copy it in and drop it. Writing straight into
            // the buffer is also the idiom every other incremental string build in this crate
            // already uses (`web.rs`, `doctor.rs`, `install.rs`, `preflight.rs`). Infallible for
            // a `String` sink, hence the discarded result.
            let _ = write!(path, "M{x} {y}h1v1h-1z");
        }
    }

    Some(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {side} {side}\" \
         shape-rendering=\"crispEdges\" role=\"img\" aria-label=\"Pairing QR code\">\
         <rect width=\"{side}\" height=\"{side}\" fill=\"#ffffff\"/>\
         <path d=\"{path}\" fill=\"#000000\"/></svg>"
    ))
}

/// The full pairing URL to encode: `https://<host>:<port>/p/<TOKEN>#fp=<FINGERPRINT>`.
///
/// **The fingerprint rides in a fragment, and that is the whole reason this is safe to add.**
/// A fragment is never sent to the server, so the flow this URL already serves — parent's
/// camera opens it, [`crate::auth::pair`] redeems the token — cannot tell the difference.
/// Nothing routes on it, no handler reads it, and a browser strips it before the request. Only
/// a client that reads the QR *itself*, rather than handing it to a browser, ever sees it.
///
/// What it buys is the difference between trust-on-first-use and **verified** first use. A
/// pinning client that learns the fingerprint here got it over a channel the network was never
/// on — a photograph of a console — so its very first handshake is checked against a value
/// nobody on the LAN could have supplied. Without it, such a client has to display what it saw
/// and ask a parent to compare 95 characters by eye against `nestwatch fingerprint`, which is
/// a check people reliably decline to actually perform.
///
/// `None` when the certificate could not be read. The URL is then exactly what it always was,
/// and a client that wanted the fingerprint falls back to comparing by eye — degraded, not
/// broken. See [`crate::install::print_access_block`] for why that must not be an error.
///
/// **Format is [`crate::cert::read_fingerprint`]'s verbatim** — uppercase hex, colon-separated.
/// Measured, that costs one QR version against a colon-less spelling (version 7 rather than 6;
/// 53 columns rather than 49, both comfortably inside an 80-column console). It buys being
/// byte-identical to what `nestwatch fingerprint` prints and what a parent compares by eye, so
/// there is one spelling of a fingerprint in this project rather than two. Uppercase is *not*
/// worth anything on its own here: measured, upper- and lowercase hex produce the identical
/// version and width, so any argument resting on QR alphanumeric mode is empty.
pub fn pair_url(host: &str, port: u16, token: &str, fingerprint: Option<&str>) -> String {
    // Built by appending to the old string rather than as one format!, so the pre-fragment
    // prefix is byte-identical to what this returned before by construction, not by care.
    let base = format!("https://{host}:{port}/p/{token}");
    match fingerprint {
        Some(fp) => format!("{base}#fp={fp}"),
        None => base,
    }
}

/// What a pairing request resolves to before any token exists.
///
/// **A named outcome rather than an `Option`, because the failure had a wrong answer available.**
/// `api::pair_provider` shipped with `reachable_hosts().into_iter().next().unwrap_or_else(||
/// "localhost".to_string())` — which is what an `Option` invites: there is always something
/// plausible to substitute, and `localhost` resolves on the child's PC and nowhere else, so the
/// QR scanned, the phone failed to connect, and nothing said why. Making "there is no address" a
/// variant means the caller has to answer it rather than fill it in.
///
/// Borrowed from the caller's list rather than owned: both callers already hold the `Vec` and
/// neither needs it to outlive the decision.
#[derive(Debug, PartialEq, Eq)]
pub enum PairingAddress<'a> {
    /// Advertise this host. The first entry of [`crate::cert::reachable_hosts`] — the LAN address
    /// the certificate also covers, so the parent never gets a name mismatch stacked on the trust
    /// warning.
    Advertise(AdvertisedHost<'a>),
    /// This machine has no address worth putting in a link. Callers must refuse: the install
    /// console prints how to get back on the network, and the API answers `400`.
    Offline,
}

/// Which address a pairing link should carry, given what the machine can be reached at.
///
/// One line, and it exists anyway — for the reason `O93` records. The property worth pinning is
/// not the lookup but the **refusal**: that an empty list produces no address at all rather than a
/// loopback one, and that a caller therefore cannot mint a token it has nowhere to point. Both
/// callers ask through here so the console and the dashboard cannot answer it differently, which
/// is the same argument [`link_forms`] makes one function down.
pub fn address_for(hosts: &[String]) -> PairingAddress<'_> {
    match hosts.first() {
        Some(host) => PairingAddress::Advertise(AdvertisedHost(host)),
        None => PairingAddress::Offline,
    }
}

/// A host [`address_for`] decided is worth advertising.
///
/// **A newtype with a private field, which is the whole mechanism.** The only way to obtain one is
/// `address_for` returning [`PairingAddress::Advertise`], and [`link_forms`] takes this rather than
/// a `&str` — so a caller cannot hand a pairing link a host it invented. Reintroducing the shipped
/// defect (`.unwrap_or("localhost")`) stops being a test failure and becomes a compile error, which
/// matters because the test could only ever watch the one path it was pointed at: a handler that
/// bypassed `address_for` and inlined the fallback passed the whole suite, measured 2026-09-06.
///
/// The same argument [`Scope`] makes about redemption — there is no success value that does not say
/// what it is worth — applied to the other half of the same link.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct AdvertisedHost<'a>(&'a str);

impl std::fmt::Display for AdvertisedHost<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// The two forms one pairing link takes: `(scanned, typed)`.
///
/// **A pairing link is two strings, and this is the one place that says so.** The QR carries the
/// certificate fingerprint; the address printed or displayed underneath it does not. They are read
/// by different things — only a client that scans can act on a fingerprint, while anyone reading
/// the second one is going to type it into a browser, which cannot check one and would just be
/// handed 95 characters of noise. Measured on the console form: including it takes the line from
/// 46 columns to 145, which wraps on the 80-column terminal a person copies from.
///
/// Extracted because the rule had already been broken once. `install::print_access_block` built
/// both forms correctly and `api::pair_provider`, written later against it, built one — putting the
/// fingerprint-bearing URL in the "or type this address" field. Two callers agreeing about what a
/// link *is* only by both remembering the same paragraph is the arrangement that produced that,
/// and a shared function is what ends it.
pub fn link_forms(
    host: AdvertisedHost<'_>,
    port: u16,
    token: &str,
    fingerprint: Option<&str>,
) -> (String, String) {
    let host = host.0;
    (
        pair_url(host, port, token, fingerprint),
        pair_url(host, port, token, None),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An offline machine yields no address — not a plausible-looking one.
    ///
    /// **This is `O93`'s regression test, and the assertion that matters is the second.** The
    /// shipped defect was not a missing check; it was a fallback that produced `localhost`, which
    /// type-checks, reads fine, and builds a QR a phone cannot use. A test asserting only
    /// "returns Offline" would still pass if someone reintroduced the substitution one layer up,
    /// so this also states the thing that must never come back.
    #[test]
    fn an_offline_machine_gets_no_pairing_address_at_all() {
        assert_eq!(address_for(&[]), PairingAddress::Offline);
        for wrong in ["localhost", "127.0.0.1", "::1", "0.0.0.0"] {
            assert_ne!(
                address_for(&[]),
                PairingAddress::Advertise(AdvertisedHost(wrong)),
                "an empty host list must refuse, never substitute {wrong} — a phone on the LAN \
                 cannot reach any of these, and the QR would scan and then fail silently"
            );
        }
    }

    /// The first entry is the one advertised, because it is the one the certificate covers.
    #[test]
    fn the_first_reachable_host_is_the_one_advertised() {
        let hosts = vec!["10.0.0.5".to_string(), "childpc".to_string()];
        assert_eq!(
            address_for(&hosts),
            PairingAddress::Advertise(AdvertisedHost("10.0.0.5"))
        );
    }

    /// The fingerprint belongs in exactly one of the two forms.
    ///
    /// Asserted in both directions on purpose: "the QR has it" alone stays true if the typed form
    /// gains it too, which is precisely the defect this function was extracted to prevent.
    #[test]
    fn the_fingerprint_rides_the_scanned_form_and_not_the_typed_one() {
        let (scanned, typed) =
            link_forms(AdvertisedHost("10.0.0.5"), 9443, "TOKEN123", Some("AA:BB"));
        assert!(
            scanned.contains("#fp=AA:BB"),
            "the QR carries it: {scanned}"
        );
        assert!(
            !typed.contains("#fp="),
            "the address a person types must not: {typed}"
        );
    }

    /// Both forms address the same install and redeem the same token — they differ only in the
    /// fragment, which no server ever sees.
    #[test]
    fn both_forms_are_the_same_link() {
        let (scanned, typed) = link_forms(AdvertisedHost("host.local"), 8443, "ABC", Some("FF"));
        assert_eq!(scanned, format!("{typed}#fp=FF"));
        assert!(typed.contains("/p/ABC"));
    }

    /// An unreadable certificate costs the QR its fragment and nothing else — the two forms
    /// collapse to one string rather than the caller losing a link.
    #[test]
    fn without_a_fingerprint_the_two_forms_are_identical() {
        let (scanned, typed) = link_forms(AdvertisedHost("10.0.0.5"), 9443, "T", None);
        assert_eq!(scanned, typed);
    }

    /// Returns the guard alongside the path: dropping it deletes the directory, so a caller that
    /// kept only the `PathBuf` would be handed a path into a directory that no longer exists.
    fn tmp(name: &str) -> (std::path::PathBuf, crate::testutil::ScratchDir) {
        let dir = crate::testutil::ScratchDir::new(&format!("pair-{name}"));
        (dir.join("pairing.json"), dir)
    }

    /// A realistic fingerprint, in the exact shape `cert::read_fingerprint` returns.
    const FP: &str = "AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:\
                      AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89";

    /// Adding the fragment must not disturb one byte of what came before it.
    ///
    /// Anything already reading this URL — a parent typing it, a note taped to a router, the
    /// browser flow itself — must see exactly what it saw before. Asserted against the
    /// fingerprint-less form rather than a hardcoded string, so the two spellings cannot drift.
    #[test]
    fn the_fragment_is_appended_and_changes_nothing_before_it() {
        let plain = pair_url("192.168.1.42", 8443, "EG629F4DQDDHS44V", None);
        let pinned = pair_url("192.168.1.42", 8443, "EG629F4DQDDHS44V", Some(FP));

        assert_eq!(plain, "https://192.168.1.42:8443/p/EG629F4DQDDHS44V");
        assert_eq!(pinned, format!("{plain}#fp={FP}"));
        assert!(
            pinned.starts_with(&plain),
            "the pre-fragment prefix changed: {pinned}"
        );
    }

    /// The fragment must never be mistaken for part of the token.
    ///
    /// `/p/{token}` routes on the path segment, and a fragment never leaves the client, so this
    /// holds today for two independent reasons. It is asserted anyway because the failure would
    /// be silent: a refactor that passed a whole URL where a token is expected would not error,
    /// it would simply stop matching, and pairing would quietly never work again.
    #[test]
    fn the_fragment_is_not_part_of_the_token() {
        let (path, _dir) = tmp("fragment");
        let t = mint(&path, Scope::Dashboard).unwrap();
        let url = pair_url("192.168.1.42", 8443, &t, Some(FP));

        // What axum hands `pair` as `{token}`: the path segment, fragment already gone.
        let segment = url.split("/p/").nth(1).unwrap().split('#').next().unwrap();
        assert_eq!(
            segment, t,
            "the path segment must be the token and nothing else"
        );
        assert!(
            redeem(&path, segment).is_some(),
            "the token from a pinned URL must pair"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The denser payload must still render a QR a phone can actually read.
    ///
    /// The fingerprint roughly triples the URL, and an unscannable QR is worse than no change
    /// at all — the feature would be invisible in code review and broken on a kitchen table.
    /// Measured at the worst case `reachable_hosts` can produce, which is a long hostname
    /// rather than an IP.
    #[test]
    fn a_pinned_url_still_fits_a_console_qr() {
        let host = "DESKTOP-A1B2C3D4E5F6G7H";
        let url = pair_url(host, 8443, "EG629F4DQDDHS44V", Some(FP));
        let qr = qr_code(&url).expect("a pinned pairing URL must still encode as a QR");

        let widest = qr.lines().map(|l| l.chars().count()).max().unwrap_or(0);
        assert!(
            widest <= 80,
            "the QR wrapped at {widest} columns, which makes it unscannable"
        );
    }

    #[test]
    fn mint_then_redeem_once() {
        let (path, _dir) = tmp("once");
        let t = mint(&path, Scope::Dashboard).unwrap();
        assert_eq!(t.chars().count(), TOKEN_LEN);
        assert!(
            redeem(&path, &t).is_some(),
            "the freshly minted token must pair"
        );
        assert!(
            redeem(&path, &t).is_none(),
            "a pairing token must be single-use"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A token is worth what it was minted for, and redemption is the only thing that says so.
    ///
    /// The scope is the authority, so a mint/redeem round trip that lost it would hand an
    /// integration the dashboard — silently, since every other assertion here would still pass.
    #[test]
    fn a_token_redeems_to_the_scope_it_was_minted_with() {
        let (path, _dir) = tmp("scope");
        let t = mint(&path, Scope::Dashboard).unwrap();
        assert_eq!(redeem(&path, &t), Some(Scope::Dashboard));

        let t = mint(
            &path,
            Scope::Integration {
                source: "studygo".into(),
            },
        )
        .unwrap();
        assert_eq!(
            redeem(&path, &t),
            Some(Scope::Integration {
                source: "studygo".into()
            }),
            "an integration token must not redeem to dashboard authority"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A pairing file written before tokens carried a scope is refused, not promoted.
    ///
    /// This is the migration, at the one place it can be tested directly. Fifteen minutes is the
    /// whole exposure, but "unknown authority" must never resolve to "all authority".
    #[test]
    fn a_scopeless_pairing_file_is_refused_and_cleared() {
        let (path, _dir) = tmp("legacy");
        let legacy = format!(
            r#"{{"hash":"{}","expires_at":{}}}"#,
            digest("LEGACYTOKEN12345"),
            now_secs() + 600
        );
        std::fs::write(&path, legacy).unwrap();
        assert!(
            redeem(&path, "LEGACYTOKEN12345").is_none(),
            "a token with no recorded authority must not pair"
        );
        assert!(
            !path.exists(),
            "and it must be cleared, so it cannot block a fresh pairing"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_plaintext_token_is_never_written_to_disk() {
        let (path, _dir) = tmp("nostore");
        let t = mint(&path, Scope::Dashboard).unwrap();
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            !on_disk.contains(&t),
            "stored file must hold only a hash, got: {on_disk}"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn wrong_and_empty_tokens_are_refused_without_consuming() {
        let (path, _dir) = tmp("wrong");
        let t = mint(&path, Scope::Dashboard).unwrap();
        assert!(redeem(&path, "WRONGWRONGWRONG1").is_none());
        assert!(redeem(&path, "").is_none());
        // A failed attempt must not burn the real token.
        assert!(redeem(&path, &t).is_some());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn typed_form_is_normalized() {
        let (path, _dir) = tmp("normalize");
        let t = mint(&path, Scope::Dashboard).unwrap();
        let typed = format!("{}-{}", &t[..8], &t[8..]).to_lowercase();
        assert!(
            redeem(&path, &typed).is_some(),
            "hyphenated lowercase must still pair"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn expired_tokens_are_refused_and_cleaned_up() {
        let (path, _dir) = tmp("expired");
        let t = mint(&path, Scope::Dashboard).unwrap();
        // Rewrite the stored record with an expiry in the past.
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut pending: Pending = serde_json::from_str(&raw).unwrap();
        pending.expires_at = now_secs() - 1;
        std::fs::write(&path, serde_json::to_string(&pending).unwrap()).unwrap();

        assert!(
            redeem(&path, &t).is_none(),
            "an expired token must not pair"
        );
        assert!(!path.exists(), "an expired token must be cleaned up");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Regression: concurrent scans of the same QR must yield exactly ONE session.
    ///
    /// This failed before [`REDEEM_GATE`] existed — the code relied on `remove_file` being an
    /// exclusive operation, and it isn't: 8 racing threads all got `Ok` in most rounds, so every
    /// concurrent request paired. Repeated rounds because the race is timing-dependent; a single
    /// round passes by luck often enough to be useless as a guard.
    #[test]
    fn concurrent_redeems_grant_exactly_one_session() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (path, _dir) = tmp("race");
        for round in 0..50 {
            let token = mint(&path, Scope::Dashboard).unwrap();
            let wins = Arc::new(AtomicUsize::new(0));
            let start = Arc::new(std::sync::Barrier::new(8));
            std::thread::scope(|s| {
                for _ in 0..8 {
                    let (p, t) = (path.clone(), token.clone());
                    let (w, b) = (wins.clone(), start.clone());
                    s.spawn(move || {
                        b.wait(); // maximize overlap
                        if redeem(&p, &t).is_some() {
                            w.fetch_add(1, Ordering::SeqCst);
                        }
                    });
                }
            });
            assert_eq!(
                wins.load(Ordering::SeqCst),
                1,
                "round {round}: a single-use pairing token was spent more than once"
            );
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn redeem_with_no_pending_token_is_false() {
        let (path, _dir) = tmp("missing");
        let _ = std::fs::remove_file(&path);
        assert!(redeem(&path, "ANYTHINGATALL123").is_none());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
