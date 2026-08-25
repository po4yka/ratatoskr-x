//! Typed failures raised while sealing or opening credential envelopes.
//!
//! Renderings stay value-free: no variant message ever contains key material,
//! plaintext, or envelope bytes.

/// Why an envelope operation failed.
///
/// Renderings are value-free: no variant message ever contains key material,
/// plaintext, or envelope bytes.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CipherError {
    /// The configured key material was absent, undecodable, or not exactly 32 bytes.
    #[error("the token encryption key is missing or is not 32 raw bytes of valid base64url")]
    Key,
    /// Authenticated decryption failed: tampered ciphertext, wrong key, or a
    /// corrupted envelope.
    #[error("the envelope failed authentication")]
    Auth,
    /// The envelope was sealed for a different owner or a different purpose than
    /// the opener supplied.
    #[error("the envelope is bound to a different owner or purpose")]
    Binding,
    /// The leading format marker names no implemented envelope version.
    #[error("the envelope carries an unknown format version")]
    Version,
}

/// Why an OAuth connection flow failed.
///
/// Renderings stay value-free: no variant message ever contains tokens, verifier
/// values, or payload contents.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FlowError {
    /// The OAuth client identity or redirect target is not configured yet.
    #[error("oauth.client_id and oauth.redirect_uri must be configured before connecting")]
    Configuration,
    /// A database operation behind the flow failed.
    #[error("a database operation failed")]
    Persistence(#[source] x_persistence::error::PersistenceError),
    /// An envelope sealing or opening operation failed.
    #[error("an envelope operation failed")]
    Cipher(#[source] CipherError),
    /// The provider granted fewer scopes than the minimized read set requested.
    ///
    /// The missing scope names are carried as data; the rendering stays value-free.
    #[error("the provider granted fewer read scopes than requested")]
    Downgrade {
        /// Every requested read scope the grant omitted.
        missing_scopes: Vec<String>,
    },
    /// The callback state matched no persisted intent.
    #[error("the callback state does not match any intent")]
    UnknownCallbackState,
    /// The callback state belonged to an intent whose expiry has passed.
    #[error("the callback state belongs to an expired intent")]
    ExpiredCallbackIntent,
    /// The callback state was already consumed by an earlier acceptance.
    #[error("the callback state was already consumed")]
    ReplayedCallbackState,
    /// The presented refresh token matches neither the current nor the retired one.
    #[error("the presented refresh token matches neither the stored nor the retired token")]
    StaleRefreshToken,
    /// The presented refresh token is the retired one: reuse of a rotated family.
    #[error("the presented refresh token is retired; the credential family is revoked")]
    RefreshReplay,
    /// The account's stored credential is not active.
    #[error("the stored credential is not active")]
    CredentialInactive,
    /// The provider rejected the current refresh token outright.
    #[error("the provider rejected the current refresh token")]
    UpstreamInvalidation,
    /// The provider could not be reached at all.
    #[error("the provider could not be reached")]
    Transport,
    /// The provider answered with a body outside every expected shape.
    #[error("the provider answered with an unexpected body")]
    MalformedProviderResponse,
}
