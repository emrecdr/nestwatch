//! Self-signed TLS certificate management.
//!
//! The browser will show a one-time "not trusted" warning — expected for a self-signed cert
//! on a home LAN; the point is that the password and screenshots travel encrypted. To let
//! you tell the *real* server from a LAN impostor, `install` prints the cert's SHA-256
//! fingerprint so you can verify it once (trust-on-first-use).
//!
//! Certs include the machine hostname and its primary LAN IP as SANs (so connecting by IP
//! doesn't add a name-mismatch error on top of the trust warning). Validity is capped at
//! 825 days: Apple (Safari/iOS) hard-rejects any server cert with a longer lifetime — even
//! a manually trusted one — with no click-through, so a longer cert would be unusable on an
//! iPhone/Mac. `install` regenerates the cert, so this window is refreshed on every reinstall.

use std::net::UdpSocket;
use std::path::Path;

use anyhow::{Context, Result};
use rcgen::{CertificateParams, ExtendedKeyUsagePurpose, KeyPair};
use sha2::{Digest, Sha256};

/// Ensure a cert/key pair exists, generating one if absent. Used by the server at startup.
pub fn ensure_cert(cert_path: &Path, key_path: &Path) -> Result<()> {
    if cert_path.exists() && key_path.exists() {
        return Ok(());
    }
    generate(cert_path, key_path)?;
    Ok(())
}

/// Generate a fresh cert/key pair (overwriting any existing) and return its SHA-256
/// fingerprint (uppercase hex, colon-separated). Used at install time.
pub fn generate(cert_path: &Path, key_path: &Path) -> Result<String> {
    if let Some(parent) = cert_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // CertificateParams::new maps IP-parseable strings to IP SANs and the rest to DNS SANs.
    let mut sans = vec!["localhost".to_string()];
    sans.extend(reachable_hosts());

    let key_pair = KeyPair::generate().context("generating key pair")?;
    let mut params = CertificateParams::new(sans).context("building certificate params")?;
    // 825-day cap — the longest Apple will accept for a TLS server cert (see module docs).
    // Both bounds must be set: Apple measures `not_after - not_before`, and rcgen's default
    // `not_before` is 1975-01-01, which would silently make this a ~51-year cert and defeat the
    // cap entirely. Backdated an hour so mild clock skew between this PC and the phone viewing
    // the dashboard can't render the cert "not yet valid".
    let not_before = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
    params.not_before = not_before;
    params.not_after = not_before + time::Duration::days(VALIDITY_DAYS as i64);
    // Apple also requires the serverAuth EKU on TLS server certs; rcgen omits it by default.
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];

    let cert = params
        .self_signed(&key_pair)
        .context("self-signing certificate")?;

    std::fs::write(cert_path, cert.pem())?;
    std::fs::write(key_path, key_pair.serialize_pem())?;
    tracing::info!(
        "generated self-signed certificate at {}",
        cert_path.display()
    );

    Ok(fingerprint(cert.der()))
}

/// Certificate validity window in days (see [`generate`]).
const VALIDITY_DAYS: u64 = 825;
/// Start warning once the cert is within this many days of expiry. `pub` so `doctor` nags at the
/// same threshold as the service log — two different answers to "is this cert about to lapse?"
/// would have the parent reading a diagnostic that contradicts the warning in their log file.
pub const RENEW_WARN_DAYS: u64 = 30;

/// Whether a cert `age_days` old is close enough to its [`VALIDITY_DAYS`] expiry to warn about.
fn is_expiring(age_days: u64) -> bool {
    age_days + RENEW_WARN_DAYS >= VALIDITY_DAYS
}

/// Best-effort startup check: warn loudly (to the service log) if the installed cert is nearing
/// expiry, so the parent re-runs `install` before Safari/iOS start hard-rejecting it. Uses the
/// cert file's mtime as a proxy for generation time — `install` rewrites the cert, refreshing the
/// mtime — so no cert parser is needed. Never fails the server start.
pub fn warn_if_expiring(cert_path: &Path) {
    let Ok(age_days) = cert_age_days(cert_path) else {
        return;
    };
    if is_expiring(age_days) {
        tracing::warn!(
            "TLS certificate is ~{age_days} days old and nears its {VALIDITY_DAYS}-day expiry — \
             re-run `nestwatch install` to refresh it (the fingerprint will change)"
        );
    }
}

/// Roughly how many days of validity the installed cert has left, for `nestwatch doctor`.
/// Uses the same mtime proxy as [`warn_if_expiring`]; `None` if the cert is missing/unreadable.
/// Saturates at 0 rather than going negative when already expired.
pub fn days_until_expiry(cert_path: &Path) -> Option<u64> {
    let age = cert_age_days(cert_path).ok()?;
    Some(VALIDITY_DAYS.saturating_sub(age))
}

fn cert_age_days(cert_path: &Path) -> std::io::Result<u64> {
    let mtime = std::fs::metadata(cert_path)?.modified()?;
    let age = std::time::SystemTime::now()
        .duration_since(mtime)
        .unwrap_or_default();
    Ok(age.as_secs() / 86_400)
}

/// Read an existing cert PEM and return its SHA-256 fingerprint (same format `install` printed),
/// so a parent can re-check it when adding a new device long after install. Reads the actual
/// cert on disk, so it stays correct even if the cert was regenerated.
pub fn read_fingerprint(cert_path: &Path) -> Result<String> {
    let pem = std::fs::read_to_string(cert_path)
        .with_context(|| format!("reading cert at {}", cert_path.display()))?;
    // Select the CERTIFICATE block specifically — `pem::parse` returns the first block regardless
    // of tag, so a key-first combined PEM would otherwise fingerprint the private key silently,
    // defeating the whole point (verifying you're trusting the right server's cert).
    let block = pem::parse_many(pem.as_bytes())
        .context("parsing cert PEM")?
        .into_iter()
        .find(|b| b.tag() == "CERTIFICATE")
        .with_context(|| format!("no CERTIFICATE block in {}", cert_path.display()))?;
    Ok(fingerprint(block.contents()))
}

/// SHA-256 of the DER cert, formatted `AB:CD:...`.
fn fingerprint(der: &[u8]) -> String {
    let digest = Sha256::digest(der);
    digest
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Addresses this machine is reachable at from the LAN, best-first (IP, then hostname).
///
/// Single source of truth for both the certificate SANs and the URL `install` prints, so the
/// address we tell the parent to visit is always one the cert actually covers — otherwise they'd
/// get a name-mismatch error stacked on top of the expected trust warning. Empty when offline.
pub fn reachable_hosts() -> Vec<String> {
    let mut hosts = Vec::new();
    if let Some(ip) = primary_lan_ip() {
        hosts.push(ip);
    }
    if let Some(host) = hostname() {
        hosts.push(host);
    }
    hosts
}

/// Best-effort machine hostname, added as a SAN for convenience.
fn hostname() -> Option<String> {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|h| !h.is_empty())
}

/// Best-effort primary LAN IP via the "UDP connect to a routable address" trick (no packet
/// is actually sent — it just resolves the outbound interface). `None` if offline.
fn primary_lan_ip() -> Option<String> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    let ip = sock.local_addr().ok()?.ip();
    if ip.is_loopback() || ip.is_unspecified() {
        return None;
    }
    Some(ip.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_expiring_only_near_the_end() {
        assert!(!is_expiring(0));
        assert!(!is_expiring(700));
        assert!(!is_expiring(VALIDITY_DAYS - RENEW_WARN_DAYS - 1));
        assert!(is_expiring(VALIDITY_DAYS - RENEW_WARN_DAYS)); // exactly at the threshold
        assert!(is_expiring(VALIDITY_DAYS));
        assert!(is_expiring(VALIDITY_DAYS + 100)); // already expired
    }

    /// The fingerprint is the one value a parent copies onto paper and compares by eye on a new
    /// device. `read_fingerprint_matches_generate` compares two live computations against each
    /// other, so if the digest implementation ever changed output BOTH sides would move together
    /// and it would still pass — it pins agreement, not the value.
    ///
    /// This pins the value, against the NIST FIPS 180-4 known answer for SHA-256("abc"). It is
    /// what makes a `sha2` major upgrade a real gate rather than a compile check, and it also
    /// pins the `AB:CD:` rendering, since a parent comparing by eye is comparing the format too.
    #[test]
    fn fingerprint_matches_the_nist_known_answer() {
        // FIPS 180-4, B.1: SHA-256("abc")
        // = ba7816bf 8f01cfea 414140de 5dae2223 b00361a3 96177a9c b410ff61 f20015ad
        let expected = "BA:78:16:BF:8F:01:CF:EA:41:41:40:DE:5D:AE:22:23:\
                        B0:03:61:A3:96:17:7A:9C:B4:10:FF:61:F2:00:15:AD"
            .replace(' ', "");
        assert_eq!(fingerprint(b"abc"), expected);
    }

    #[test]
    fn read_fingerprint_matches_generate() {
        let dir = std::env::temp_dir().join(format!("nw-cert-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cert = dir.join("cert.pem");
        let key = dir.join("key.pem");

        // The fingerprint generate() returns must match reading the cert back off disk.
        let at_install = generate(&cert, &key).unwrap();
        let read_back = read_fingerprint(&cert).unwrap();
        assert_eq!(at_install, read_back);
        assert!(read_back.contains(':') && read_back.len() == 95); // 32 bytes → "AB:..:CD"

        // A key-first combined PEM must still fingerprint the CERTIFICATE block, not the key.
        let combined = dir.join("combined.pem");
        let key_pem = std::fs::read_to_string(&key).unwrap();
        let cert_pem = std::fs::read_to_string(&cert).unwrap();
        std::fs::write(&combined, format!("{key_pem}\n{cert_pem}")).unwrap();
        assert_eq!(read_fingerprint(&combined).unwrap(), at_install);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
