//! PKCE (RFC 7636) verifier / challenge generation.
//!
//! Port of `packages/ai/src/auth/oauth/pkce.ts`. The verifier is 32
//! cryptographically-random bytes encoded as URL-safe base64 (no padding),
//! matching the `generatePKCE` helper upstream uses; the challenge is the
//! SHA-256 of the verifier bytes, also URL-safe base64.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use sha2::{Digest, Sha256};

/// PKCE verifier + S256 challenge pair (`PkcePair`).
#[derive(Debug, Clone)]
pub struct PkcePair {
    /// URL-safe base64 (no padding) verifier, the OAuth `code_verifier`.
    pub verifier: String,
    /// URL-safe base64 (no padding) SHA-256 challenge, the OAuth
    /// `code_challenge`.
    pub challenge: String,
}

/// Generate a fresh PKCE pair (`generatePKCE` upstream).
pub fn generate_pkce() -> PkcePair {
    let mut bytes = [0u8; 32];
    // `getrandom` is in the workspace dep graph and works on every
    // target the OAuth module compiles on (the module is gated
    // `#[cfg(not(target_arch = "wasm32"))]`).
    getrandom::getrandom(&mut bytes).expect("OS RNG must be available on every supported target");
    let verifier = URL_SAFE_NO_PAD.encode(bytes);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(bytes.as_slice()));
    PkcePair { verifier, challenge }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_carries_a_verifier_and_a_distinct_challenge() {
        let pair = generate_pkce();
        // 32 random bytes → 43 base64url chars (no padding).
        assert_eq!(pair.verifier.len(), 43);
        // SHA-256 → 32 bytes → 43 base64url chars.
        assert_eq!(pair.challenge.len(), 43);
        assert_ne!(pair.verifier, pair.challenge);
    }

    #[test]
    fn verifier_is_url_safe_base64_no_padding() {
        let pair = generate_pkce();
        // No `=` padding and no `+` / `/` (URL-safe alphabet).
        assert!(!pair.verifier.contains('='));
        assert!(!pair.verifier.contains('+'));
        assert!(!pair.verifier.contains('/'));
    }

    #[test]
    fn challenge_is_sha256_of_the_verifier_bytes() {
        // Round-trip: decode verifier → SHA-256 → base64url == challenge.
        let pair = generate_pkce();
        let verifier_bytes = URL_SAFE_NO_PAD.decode(pair.verifier.as_bytes()).unwrap();
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier_bytes.as_slice()));
        assert_eq!(expected, pair.challenge);
    }

    #[test]
    fn two_pairs_are_independent() {
        let a = generate_pkce();
        let b = generate_pkce();
        assert_ne!(a.verifier, b.verifier);
        assert_ne!(a.challenge, b.challenge);
    }
}