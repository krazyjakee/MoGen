//! PKCE S256 + state nonce + base64url helpers.
//!
//! Spec: RFC 7636. Verifier is 64 random bytes → base64url-no-pad
//! (~86 chars, well within the 43–128 char range). Challenge =
//! base64url(SHA256(verifier)).

use rand::RngCore;
use sha2::{Digest, Sha256};

/// PKCE pair plus a CSRF state nonce. `state` is opaque — we use a 16-byte
/// random hex string, sized to fit comfortably inside a URL query param.
#[derive(Debug, Clone)]
pub struct PkcePair {
    pub verifier: String,
    pub challenge: String,
    pub state: String,
}

impl PkcePair {
    /// Generate a fresh verifier/challenge/state triple from the OS RNG.
    pub fn generate() -> Self {
        let mut verifier_bytes = [0u8; 64];
        rand::thread_rng().fill_bytes(&mut verifier_bytes);
        let verifier = base64url_no_pad(&verifier_bytes);

        let challenge = challenge_for(&verifier);

        let mut state_bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut state_bytes);
        let state = hex_lower(&state_bytes);

        Self { verifier, challenge, state }
    }
}

/// Compute the S256 challenge for an existing verifier (test helper +
/// re-use point for the flow).
pub fn challenge_for(verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let digest = hasher.finalize();
    base64url_no_pad(&digest)
}

/// Base64url encoding without padding (RFC 7636 §4.2).
pub fn base64url_no_pad(bytes: &[u8]) -> String {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    URL_SAFE_NO_PAD.encode(bytes)
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7636 Appendix B sample vector. Verifier
    /// `dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk`
    /// must yield S256 challenge `E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM`.
    #[test]
    fn rfc7636_appendix_b_vector() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            challenge_for(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn verifier_is_in_legal_length_range() {
        let pair = PkcePair::generate();
        // base64url(64 bytes, no pad) = 86 chars; legal range 43..=128.
        assert!(pair.verifier.len() >= 43 && pair.verifier.len() <= 128);
        // Challenge of 32-byte digest base64url-no-pad is exactly 43 chars.
        assert_eq!(pair.challenge.len(), 43);
    }

    #[test]
    fn state_is_unique_across_generations() {
        let a = PkcePair::generate();
        let b = PkcePair::generate();
        assert_ne!(a.state, b.state);
        assert_ne!(a.verifier, b.verifier);
    }

    #[test]
    fn base64url_uses_url_safe_alphabet_no_pad() {
        // 0xfb 0xff: in standard alphabet → "+/"; in url-safe → "-_".
        let s = base64url_no_pad(&[0xfb, 0xff]);
        assert!(!s.contains('+'));
        assert!(!s.contains('/'));
        assert!(!s.contains('='));
        assert_eq!(s, "-_8");
    }
}
