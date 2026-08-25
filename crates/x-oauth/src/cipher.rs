//! The AES-256-GCM token cipher and its self-describing envelope layout.

use aes_gcm::aead::Aead;
use aes_gcm::aead::consts::U12;
use aes_gcm::{Aes256Gcm, KeyInit as _, Nonce};

use crate::error::CipherError;

/// The leading format marker: ASCII `v` plus the envelope format version.
const VERSION_PREFIX: [u8; 2] = [0x76, 0x01];

/// The GCM nonce length in bytes.
const NONCE_LEN: usize = 12;

/// The GCM authentication tag length in bytes.
const TAG_LEN: usize = 16;

/// The authenticated-data purpose an envelope is bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// Credential envelopes bind the account UUID.
    Credential,
    /// Authorization-intent verifier envelopes bind the requesting internal-user UUID.
    IntentVerifier,
}

impl Purpose {
    /// The namespaced label carried inside the authenticated envelope body.
    #[must_use]
    pub fn label(self) -> &'static [u8] {
        match self {
            Self::Credential => b"ratatoskr/x/credential/v1",
            Self::IntentVerifier => b"ratatoskr/x/intent-verifier/v1",
        }
    }

    /// The authenticated inner prefix for one owner binding: the namespaced
    /// purpose label followed by the 16 owner UUID bytes.
    #[must_use]
    pub fn binding_prefix(self, owner: uuid::Uuid) -> Vec<u8> {
        let mut data = Vec::with_capacity(self.label().len() + 16);
        data.extend_from_slice(self.label());
        data.extend_from_slice(owner.as_bytes());
        data
    }
}

/// Recognizes which purpose label prefixes `inner`, returning the label length.
fn matched_label(inner: &[u8]) -> Option<(Purpose, usize)> {
    for purpose in [Purpose::Credential, Purpose::IntentVerifier] {
        let label = purpose.label();
        if inner.get(..label.len()) == Some(label) {
            return Some((purpose, label.len()));
        }
    }
    None
}

/// Seals and opens secret payloads under one 32-byte AES-256-GCM key.
#[derive(Clone)]
pub struct TokenCipher {
    key: Vec<u8>,
}

impl std::fmt::Debug for TokenCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TokenCipher([redacted])")
    }
}

impl TokenCipher {
    /// The exact key length AES-256 requires.
    const KEY_LEN: usize = 32;

    /// Builds a cipher from exactly 32 raw key bytes.
    ///
    /// # Errors
    /// When the slice is not exactly 32 bytes.
    pub fn new(key: &[u8]) -> Result<Self, CipherError> {
        if key.len() != Self::KEY_LEN {
            return Err(CipherError::Key);
        }
        Ok(Self { key: key.to_vec() })
    }

    /// Builds a cipher from configured key material.
    ///
    /// # Errors
    /// When the material does not decode to exactly 32 bytes of base64url.
    pub fn from_secret_key(key: &x_core::config::SecretKey) -> Result<Self, CipherError> {
        let decoded = key.decoded_key().ok_or(CipherError::Key)?;
        Ok(Self {
            key: decoded.to_vec(),
        })
    }

    /// Seals plaintext into a versioned envelope bound to `owner` and `purpose`.
    ///
    /// # Panics
    /// Never in practice: the OS random source is unavailable only when the whole
    /// process environment is broken, and the key length is validated at
    /// construction.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "the key is length-validated at construction and the OS random \
                  source failing leaves the whole process unusable"
    )]
    pub fn seal(&self, owner: uuid::Uuid, purpose: Purpose, plaintext: &[u8]) -> Vec<u8> {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).expect("the OS random source provides a nonce");
        let gcm = Aes256Gcm::new_from_slice(&self.key).expect("the key length is validated");
        let mut inner = purpose.binding_prefix(owner);
        inner.extend_from_slice(plaintext);
        let ciphertext = gcm
            .encrypt(&Nonce::<U12>::from(nonce), inner.as_slice())
            .expect("sealing a fresh envelope under a validated key succeeds");
        let mut envelope = Vec::with_capacity(VERSION_PREFIX.len() + NONCE_LEN + ciphertext.len());
        envelope.extend_from_slice(&VERSION_PREFIX);
        envelope.extend_from_slice(&nonce);
        envelope.extend_from_slice(&ciphertext);
        envelope
    }

    /// Opens an envelope bound to `owner` and `purpose`, returning the plaintext.
    ///
    /// # Errors
    /// When the envelope version, authentication, or binding fails.
    ///
    /// # Panics
    /// Never in practice: the key length is validated at construction.
    #[expect(
        clippy::expect_used,
        reason = "the key is length-validated at construction"
    )]
    pub fn open(
        &self,
        owner: uuid::Uuid,
        purpose: Purpose,
        envelope: &[u8],
    ) -> Result<Vec<u8>, CipherError> {
        if envelope.get(..2) != Some(&VERSION_PREFIX[..]) {
            return Err(CipherError::Version);
        }
        let body_start = VERSION_PREFIX.len() + NONCE_LEN;
        if envelope.len() < body_start + TAG_LEN {
            return Err(CipherError::Auth);
        }
        let gcm = Aes256Gcm::new_from_slice(&self.key).expect("the key length is validated");
        let nonce_bytes = envelope
            .get(VERSION_PREFIX.len()..body_start)
            .ok_or(CipherError::Auth)?;
        let nonce: &Nonce<U12> = nonce_bytes.try_into().map_err(|_| CipherError::Auth)?;
        let ciphertext = envelope.get(body_start..).ok_or(CipherError::Auth)?;
        let inner = gcm
            .decrypt(nonce, ciphertext)
            .map_err(|_| CipherError::Auth)?;
        let (sealed_purpose, label_len) = matched_label(&inner).ok_or(CipherError::Binding)?;
        let owner_bytes = inner
            .get(label_len..label_len + 16)
            .ok_or(CipherError::Binding)?;
        let sealed_owner = uuid::Uuid::from_slice(owner_bytes).map_err(|_| CipherError::Binding)?;
        if sealed_purpose != purpose || sealed_owner != owner {
            return Err(CipherError::Binding);
        }
        Ok(inner.get(label_len + 16..).unwrap_or_default().to_vec())
    }
}
