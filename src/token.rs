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
///
/// **Panics if the OS random source fails**, which is deliberate and is what the previous
/// `OsRng::fill_bytes` did too. The two callers mint a pairing token that authenticates a device
/// and a time code that buys screen time; carrying on with predictable bytes would hand the child
/// both. There is no safe fallback here, so there is no fallback.
///
/// # What this actually calls on Windows, and why the change was an upgrade
///
/// `getrandom` is named directly rather than reached through `argon2::password_hash::rand_core`,
/// which is how the secrets on the managed PC used to be drawn — a re-export of a re-export of a
/// password-hashing crate, three crates away from anything that documents a backend.
///
/// On Windows 10 and later, `getrandom 0.4` calls **`ProcessPrng`**, which Microsoft's own RNG
/// whitepaper calls the primary interface to the user-mode per-processor PRNGs, and which needs
/// only `bcryptprimitives.dll`. The old path (`rand_core 0.6` → `getrandom 0.2`) used
/// `RtlGenRandom` — deprecated, reached through `advapi32.dll` under the undecorated name
/// `SystemFunction036`, and itself a thin wrapper around `ProcessPrng`. So this is one fewer DLL,
/// one fewer deprecated entry point, and the same bytes from the same generator.
///
/// The floor is Windows 10, and this crate's floor is 1903 (see `preflight`), so nothing is lost
/// on any machine this tool supports.
pub fn random(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    getrandom::fill(&mut bytes).expect("the OS random source must be available");
    bytes
        .iter()
        .map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char)
        .collect()
}

/// Compare two secrets without an early exit.
///
/// `==` on `str` short-circuits at the first differing byte, and in both callers the attacker
/// controls one side entirely — in principle a prefix oracle.
///
/// It lives **here**, beside `random` and `normalize`, because both secrets it guards are minted
/// by `random`: keeping the comparison next to the mint is what stops one caller getting it and
/// the other not. That is not hypothetical. Pairing digests were compared this way, with a comment
/// explaining why, while time codes — the secret the *child* has a motive to guess — were compared
/// with `==`, because the reasoning lived in the module that had it rather than with the thing
/// being reasoned about.
///
/// The length check is deliberately not constant-time: both callers compare fixed-length values,
/// so a length is not a secret here.
pub fn eq_ct(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
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
    fn eq_ct_agrees_with_equality_on_every_case_that_matters() {
        assert!(eq_ct("ABC123", "ABC123"));
        assert!(!eq_ct("ABC123", "ABC124"), "differs in the last byte");
        assert!(!eq_ct("ABC123", "XBC123"), "differs in the first byte");
        assert!(!eq_ct("ABC123", "ABC12"), "shorter");
        assert!(!eq_ct("ABC12", "ABC123"), "longer");
        assert!(eq_ct("", ""));

        // Two differences that cancel. Every case above differs in exactly ONE byte, and on a
        // single differing byte an OR-fold and an XOR-fold give the same answer — so none of them
        // can tell the accumulator apart from one that XORs. Mutation testing found that: replacing
        // `|` with `^` in the fold left this test green while `eq_ct` accepted any two strings
        // whose byte differences cancel out, a transposition among them.
        //
        // `ABCD1234` against `BACD1234` is not an abstract input. `normalize` produces exactly this
        // shape — eight alphanumerics — and a time code is the secret the child has a motive to
        // guess, which is the argument this function's own doc comment makes for existing.
        assert!(
            !eq_ct("ABCD1234", "BACD1234"),
            "two bytes transposed: the differences cancel under XOR, so an accumulator that XORs \
             instead of ORing would call these equal"
        );
        assert!(
            !eq_ct("AB", "BA"),
            "the same cancellation at the shortest length that can show it"
        );
    }

    #[test]
    fn normalize_accepts_the_forms_a_child_might_type() {
        assert_eq!(normalize("abcd-1234"), "ABCD1234");
        assert_eq!(normalize("ABCD 1234"), "ABCD1234");
        assert_eq!(normalize("  abcd1234  "), "ABCD1234");
        assert_eq!(normalize(""), "");
    }
}
