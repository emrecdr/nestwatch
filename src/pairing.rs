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

#[derive(Serialize, Deserialize)]
struct Pending {
    /// Hex SHA-256 of the token — never the token itself.
    hash: String,
    /// Unix seconds after which the token is refused.
    expires_at: u64,
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

/// Compare two hex digests without an early exit.
///
/// `==` on `str` short-circuits at the first differing byte, and the attacker controls one side
/// entirely — in principle a prefix oracle that recovers the stored hash. Not reachable for this
/// adversary through TLS, `spawn_blocking` jitter and a 5/60s throttle, but it's one line, and
/// the password path already promises constant-time comparison.
fn digests_match(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Mint a pairing token, persist its hash to `path`, and return the plaintext to display.
///
/// Replaces any previous pending token, so minting again invalidates the earlier QR — running
/// `nestwatch pair` twice can never leave two live tokens outstanding.
pub fn mint(path: &Path) -> Result<String> {
    let plaintext = token::random(TOKEN_LEN);
    let pending = Pending {
        hash: digest(&plaintext),
        expires_at: now_secs() + TTL_SECS,
    };
    let json = serde_json::to_string(&pending).context("serializing pairing token")?;
    crate::config::write_atomic(path, json.as_bytes())
        .with_context(|| format!("writing pairing token to {}", path.display()))?;
    Ok(plaintext)
}

/// Consume `token` if it matches the pending, unexpired pairing. Single-use: a successful
/// redemption deletes the file, as does encountering an expired one.
///
/// Returns `false` for anything else (no pending token, mismatch, expired, unreadable). Callers
/// must not distinguish these to the client — see the handler in `web.rs`.
pub fn redeem(path: &Path, supplied: &str) -> bool {
    let supplied = token::normalize(supplied);
    if supplied.is_empty() {
        return false;
    }
    // Held across read → verify → consume. Without it, concurrent scans each observe the token
    // as valid and each get a session (see REDEEM_GATE).
    let _gate = REDEEM_GATE.lock().unwrap_or_else(|p| p.into_inner());
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(pending) = serde_json::from_str::<Pending>(&raw) else {
        // Corrupt file: clear it so a stuck token can't block future pairings.
        let _ = std::fs::remove_file(path);
        return false;
    };
    if now_secs() >= pending.expires_at {
        let _ = std::fs::remove_file(path);
        return false;
    }
    // Compare digests, not the tokens themselves — the stored side is a hash by design.
    if !digests_match(&digest(&supplied), &pending.hash) {
        return false;
    }
    // Consume inside the gate, so the next scan of this QR finds nothing. It is [`REDEEM_GATE`]
    // — *not* the unlink — that makes this single-use; `remove_file` is not exclusive.
    std::fs::remove_file(path).is_ok()
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

/// The full pairing URL to encode: `https://<host>:<port>/p/<TOKEN>`.
pub fn pair_url(host: &str, port: u16, token: &str) -> String {
    format!("https://{host}:{port}/p/{token}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("nw-pair-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("pairing.json")
    }

    #[test]
    fn mint_then_redeem_once() {
        let path = tmp("once");
        let t = mint(&path).unwrap();
        assert_eq!(t.chars().count(), TOKEN_LEN);
        assert!(redeem(&path, &t), "the freshly minted token must pair");
        assert!(!redeem(&path, &t), "a pairing token must be single-use");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_plaintext_token_is_never_written_to_disk() {
        let path = tmp("nostore");
        let t = mint(&path).unwrap();
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            !on_disk.contains(&t),
            "stored file must hold only a hash, got: {on_disk}"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn wrong_and_empty_tokens_are_refused_without_consuming() {
        let path = tmp("wrong");
        let t = mint(&path).unwrap();
        assert!(!redeem(&path, "WRONGWRONGWRONG1"));
        assert!(!redeem(&path, ""));
        // A failed attempt must not burn the real token.
        assert!(redeem(&path, &t));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn typed_form_is_normalized() {
        let path = tmp("normalize");
        let t = mint(&path).unwrap();
        let typed = format!("{}-{}", &t[..8], &t[8..]).to_lowercase();
        assert!(
            redeem(&path, &typed),
            "hyphenated lowercase must still pair"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn expired_tokens_are_refused_and_cleaned_up() {
        let path = tmp("expired");
        let t = mint(&path).unwrap();
        // Rewrite the stored record with an expiry in the past.
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut pending: Pending = serde_json::from_str(&raw).unwrap();
        pending.expires_at = now_secs() - 1;
        std::fs::write(&path, serde_json::to_string(&pending).unwrap()).unwrap();

        assert!(!redeem(&path, &t), "an expired token must not pair");
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

        let path = tmp("race");
        for round in 0..50 {
            let token = mint(&path).unwrap();
            let wins = Arc::new(AtomicUsize::new(0));
            let start = Arc::new(std::sync::Barrier::new(8));
            std::thread::scope(|s| {
                for _ in 0..8 {
                    let (p, t) = (path.clone(), token.clone());
                    let (w, b) = (wins.clone(), start.clone());
                    s.spawn(move || {
                        b.wait(); // maximize overlap
                        if redeem(&p, &t) {
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
        let path = tmp("missing");
        let _ = std::fs::remove_file(&path);
        assert!(!redeem(&path, "ANYTHINGATALL123"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
