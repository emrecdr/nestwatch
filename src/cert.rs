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

    let mut sans = vec!["localhost".to_string()];
    if let Some(host) = hostname() {
        sans.push(host);
    }
    if let Some(ip) = primary_lan_ip() {
        sans.push(ip); // CertificateParams::new maps IP-parseable strings to IP SANs
    }

    let key_pair = KeyPair::generate().context("generating key pair")?;
    let mut params = CertificateParams::new(sans).context("building certificate params")?;
    // 825-day cap — the longest Apple will accept for a TLS server cert (see module docs).
    params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(825);
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

/// Read an existing cert PEM and return its SHA-256 fingerprint (same format `install` printed),
/// so a parent can re-check it when adding a new device long after install. Reads the actual
/// cert on disk, so it stays correct even if the cert was regenerated.
pub fn read_fingerprint(cert_path: &Path) -> Result<String> {
    let pem = std::fs::read_to_string(cert_path)
        .with_context(|| format!("reading cert at {}", cert_path.display()))?;
    let parsed = pem::parse(pem.as_bytes()).context("parsing cert PEM")?;
    Ok(fingerprint(parsed.contents()))
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

        let _ = std::fs::remove_dir_all(&dir);
    }
}
