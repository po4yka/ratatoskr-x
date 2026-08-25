//! The token cipher envelope behaves like the `oauth-connection` spec says it does:
//! typed key refusals, fresh nonces, authenticated ciphertext, owner-plus-purpose
//! binding, and an enforced format version.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use x_core::config::SecretKey;
use x_oauth::cipher::{Purpose, TokenCipher};
use x_oauth::error::CipherError;

#[test]
fn non_32_byte_key_is_refused() {
    let short = [0u8; 31];
    let error = TokenCipher::new(&short).expect_err("a 31-byte key must be refused");
    assert!(
        matches!(error, CipherError::Key),
        "length refusal is a key error: {error:?}"
    );

    let malformed = SecretKey::from("@@@not-base64url@@@");
    let decoded_error =
        TokenCipher::from_secret_key(&malformed).expect_err("malformed base64url must be refused");
    assert!(
        matches!(decoded_error, CipherError::Key),
        "malformed material is a key error: {decoded_error:?}"
    );

    let valid = [7u8; 32];
    assert!(
        TokenCipher::new(&valid).is_ok(),
        "a valid 32-byte key constructs"
    );
}

#[test]
fn each_sealing_uses_a_fresh_nonce() {
    let key = [11u8; 32];
    let cipher = TokenCipher::new(&key).expect("a valid key");
    let owner = uuid::Uuid::from_u128(0xA);
    let first = cipher.seal(owner, Purpose::Credential, b"payload");
    let second = cipher.seal(owner, Purpose::Credential, b"payload");

    // The 12-byte nonce occupies envelope bytes 2 through 14.
    assert_ne!(
        first.get(2..14),
        second.get(2..14),
        "two sealings of one plaintext must carry different nonces"
    );
}

#[test]
fn tampered_envelope_is_rejected() {
    let key = [12u8; 32];
    let cipher = TokenCipher::new(&key).expect("a valid key");
    let owner = uuid::Uuid::from_u128(0xB);
    let mut envelope = cipher.seal(owner, Purpose::Credential, b"tamper me");

    let last = envelope.len() - 1;
    envelope[last] ^= 0x01;

    let outcome = cipher.open(owner, Purpose::Credential, &envelope);
    let error = outcome.expect_err("flipping a ciphertext bit must break authentication");
    assert!(
        matches!(error, CipherError::Auth),
        "tampering is an authentication error: {error:?}"
    );
}

#[test]
fn round_trip_preserves_payload() {
    let key = [13u8; 32];
    let cipher = TokenCipher::new(&key).expect("a valid key");
    let owner = uuid::Uuid::from_u128(0xC);
    let plaintext = b"access-and-refresh-token-bytes";

    let envelope = cipher.seal(owner, Purpose::Credential, plaintext);
    let opened = cipher
        .open(owner, Purpose::Credential, &envelope)
        .expect("an untampered envelope opens under its binding");

    assert_eq!(
        opened, plaintext,
        "the opened payload equals the original byte for byte"
    );

    let other_key = [14u8; 32];
    let stranger = TokenCipher::new(&other_key).expect("a valid key");
    let wrong_key = stranger.open(owner, Purpose::Credential, &envelope);
    let error = wrong_key.expect_err("a foreign key must fail authentication");
    assert!(
        matches!(error, CipherError::Auth),
        "a wrong key is an authentication error: {error:?}"
    );
}

#[test]
fn envelope_does_not_survive_owner_or_purpose_relocation() {
    let key = [15u8; 32];
    let cipher = TokenCipher::new(&key).expect("a valid key");
    let account_a = uuid::Uuid::from_u128(0xAA);
    let account_b = uuid::Uuid::from_u128(0xBB);
    let user_a = uuid::Uuid::from_u128(0x1AA);
    let user_b = uuid::Uuid::from_u128(0x1BB);

    let credential_envelope = cipher.seal(account_a, Purpose::Credential, b"c");
    let intent_envelope = cipher.seal(user_a, Purpose::IntentVerifier, b"i");

    let relocated = cipher.open(account_b, Purpose::Credential, &credential_envelope);
    let error = relocated.expect_err("a credential envelope must not open for another account");
    assert!(
        matches!(error, CipherError::Binding),
        "owner relocation is a binding error: {error:?}"
    );

    let user_relocated = cipher.open(user_b, Purpose::IntentVerifier, &intent_envelope);
    let error =
        user_relocated.expect_err("an intent-verifier envelope must not open for another user");
    assert!(
        matches!(error, CipherError::Binding),
        "user relocation is a binding error: {error:?}"
    );

    let crossed_purpose = cipher.open(account_a, Purpose::IntentVerifier, &credential_envelope);
    let error = crossed_purpose.expect_err("a credential envelope must not open as an intent");
    assert!(
        matches!(error, CipherError::Binding),
        "purpose confusion is a binding error: {error:?}"
    );

    let crossed_back = cipher.open(user_a, Purpose::Credential, &intent_envelope);
    let error = crossed_back.expect_err("an intent-verifier envelope must not open as credential");
    assert!(
        matches!(error, CipherError::Binding),
        "reverse purpose confusion is a binding error: {error:?}"
    );
}

#[test]
fn unknown_envelope_version_is_refused() {
    let key = [16u8; 32];
    let cipher = TokenCipher::new(&key).expect("a valid key");
    let owner = uuid::Uuid::from_u128(0xDD);

    // A well-formed-length envelope whose leading marker names no implemented version.
    let mut foreign = vec![0x00, 0x02];
    foreign.extend_from_slice(&[0u8; 12]);
    foreign.extend_from_slice(&[0u8; 32]);

    let outcome = cipher.open(owner, Purpose::Credential, &foreign);
    let error = outcome.expect_err("an unknown format version must be refused");
    assert!(
        matches!(error, CipherError::Version),
        "the version must be refused before any decryption: {error:?}"
    );
}
