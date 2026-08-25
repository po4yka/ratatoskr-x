//! RFC 7636 PKCE derivation of challenges and generation of verifiers and states.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::Digest as _;
use sha2::Sha256;

/// Derives the S256 code challenge for one code verifier.
#[must_use]
pub fn challenge_from_verifier(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

/// The entropy behind one code verifier: 64 bytes encode to 86 base64url characters.
const VERIFIER_BYTES: usize = 64;

/// The entropy behind one state value: 32 bytes encode to 43 base64url characters.
const STATE_BYTES: usize = 32;

/// Generates a fresh PKCE code verifier from the OS random source.
///
/// # Panics
/// Never in practice: the OS random source is unavailable only when the whole
/// process environment is broken.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "the OS random source failing leaves the whole process unusable"
)]
pub fn generate_verifier() -> String {
    let mut bytes = [0u8; VERIFIER_BYTES];
    getrandom::fill(&mut bytes).expect("the OS random source provides verifier entropy");
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Generates a fresh OAuth state value from the OS random source.
///
/// # Panics
/// Never in practice: the OS random source is unavailable only when the whole
/// process environment is broken.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "the OS random source failing leaves the whole process unusable"
)]
pub fn generate_state() -> String {
    let mut bytes = [0u8; STATE_BYTES];
    getrandom::fill(&mut bytes).expect("the OS random source provides state entropy");
    URL_SAFE_NO_PAD.encode(bytes)
}
