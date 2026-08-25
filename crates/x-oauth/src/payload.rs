//! The decrypted credential payload and its at-rest byte encoding.

/// The plaintext pair a successful authorization or rotation stores.
///
/// The `Debug` rendering never exposes the token values.
#[derive(Clone, PartialEq, Eq)]
pub struct CredentialPayload {
    /// The bearer access token presented to provider APIs.
    pub access_token: String,
    /// The rotating refresh token that outlives the access token.
    pub refresh_token: String,
}

impl std::fmt::Debug for CredentialPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CredentialPayload([redacted])")
    }
}

impl CredentialPayload {
    /// Encodes the pair into the byte form sealed into envelopes.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes =
            Vec::with_capacity(4 + self.access_token.len() + 4 + self.refresh_token.len());
        append_length_prefixed(&mut bytes, self.access_token.as_bytes());
        append_length_prefixed(&mut bytes, self.refresh_token.as_bytes());
        bytes
    }

    /// Decodes the byte form back into a payload pair.
    ///
    /// # Errors
    /// When the bytes are not two length-prefixed UTF-8 strings.
    pub fn decode(bytes: &[u8]) -> Result<Self, PayloadDecodeError> {
        let (access, rest) = take_length_prefixed(bytes)?;
        let (refresh, tail) = take_length_prefixed(rest)?;
        if !tail.is_empty() {
            return Err(PayloadDecodeError);
        }
        Ok(Self {
            access_token: String::from_utf8(access.to_vec()).map_err(|_| PayloadDecodeError)?,
            refresh_token: String::from_utf8(refresh.to_vec()).map_err(|_| PayloadDecodeError)?,
        })
    }
}

/// The payload bytes were not a well-formed encoded pair.
#[derive(Debug, thiserror::Error)]
#[error("the credential payload bytes are malformed")]
#[non_exhaustive]
pub struct PayloadDecodeError;

fn append_length_prefixed(out: &mut Vec<u8>, value: &[u8]) {
    let len = u32::try_from(value.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(value);
}

fn take_length_prefixed(bytes: &[u8]) -> Result<(&[u8], &[u8]), PayloadDecodeError> {
    let prefix = bytes.get(..4).ok_or(PayloadDecodeError)?;
    let mut raw_len = [0u8; 4];
    raw_len.copy_from_slice(prefix);
    let len = u32::from_be_bytes(raw_len) as usize;
    let value = bytes.get(4..4 + len).ok_or(PayloadDecodeError)?;
    let rest = bytes.get(4 + len..).ok_or(PayloadDecodeError)?;
    Ok((value, rest))
}
