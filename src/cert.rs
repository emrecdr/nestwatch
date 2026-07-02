//! Self-signed TLS certificate management.
//!
//! Generated once (via `rcgen`, `ring` backend) and cached on disk. The browser will show
//! a one-time "not trusted" warning — expected for a self-signed cert on a home LAN; the
//! point is that the password and screenshots travel encrypted, not that a CA vouches for us.

use std::path::Path;

use anyhow::{Context, Result};

/// Ensure a cert/key pair exists at the given paths, generating one if absent.
pub fn ensure_cert(cert_path: &Path, key_path: &Path) -> Result<()> {
    if cert_path.exists() && key_path.exists() {
        return Ok(());
    }
    if let Some(parent) = cert_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut sans = vec!["localhost".to_string()];
    if let Some(host) = hostname() {
        sans.push(host);
    }

    let certified = rcgen::generate_simple_self_signed(sans)
        .context("failed to generate self-signed certificate")?;
    std::fs::write(cert_path, certified.cert.pem())?;
    std::fs::write(key_path, certified.signing_key.serialize_pem())?;
    tracing::info!("generated self-signed certificate at {}", cert_path.display());
    Ok(())
}

/// Best-effort machine hostname, added as a SAN for convenience. Never fails hard.
fn hostname() -> Option<String> {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|h| !h.is_empty())
}
