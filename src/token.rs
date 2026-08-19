//! Human-transcribable random secrets.
//!
//! Shared by time codes (read aloud, written on paper) and pairing tokens (scanned from a QR,
//! but typed as a fallback), so both draw from one audited source of randomness and one
//! alphabet rather than each rolling their own.

/// Crockford base32 — 32 chars, omitting I/L/O/U so `1/I/l`, `0/O` and `V/U` can't be misread
/// when a code is copied off a screen by hand. 32 divides 256 evenly, so mapping a random byte
/// with `% 32` is unbiased (no modulo skew).
///
/// Uppercase also keeps a token inside QR "alphanumeric" mode, which encodes it more densely
/// than byte mode — a smaller, easier-to-scan QR.
pub(crate) const ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A fresh cryptographically-random token of `len` characters (5 bits of entropy each).
pub fn random(len: usize) -> String {
    use argon2::password_hash::rand_core::{OsRng, RngCore};
    let mut bytes = vec![0u8; len];
    OsRng.fill_bytes(&mut bytes);
    bytes
        .iter()
        .map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char)
        .collect()
}

/// Canonicalize typed input to the stored form: uppercase, keeping only alphanumerics, so a
/// child can type `abcd-1234`, `ABCD 1234`, or `abcd1234` and all three match.
pub fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_uses_only_the_alphabet_and_is_the_right_length() {
        for len in [8, 16] {
            let t = random(len);
            assert_eq!(t.chars().count(), len);
            assert!(
                t.bytes().all(|b| ALPHABET.contains(&b)),
                "stray char in {t}"
            );
        }
    }

    #[test]
    fn random_does_not_repeat() {
        // Not a statistical test — just catches a constant/zeroed generator.
        let a = random(16);
        let b = random(16);
        assert_ne!(a, b);
    }

    #[test]
    fn normalize_accepts_the_forms_a_child_might_type() {
        assert_eq!(normalize("abcd-1234"), "ABCD1234");
        assert_eq!(normalize("ABCD 1234"), "ABCD1234");
        assert_eq!(normalize("  abcd1234  "), "ABCD1234");
        assert_eq!(normalize(""), "");
    }
}
