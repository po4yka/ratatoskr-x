//! Authorization-intent issuance and the provider authorization URL.

use crate::error::FlowError;
use crate::pkce;

/// Why one PKCE authorization intent is being issued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IntentPurpose {
    /// Establish or replace the default read-only connection.
    ReadConnection,
    /// Extend one existing connected account with bookmark mutation authority.
    BookmarkWrite {
        /// The existing X account whose grant must be extended.
        account_id: uuid::Uuid,
    },
}

impl IntentPurpose {
    /// Returns the durable purpose token and optional existing-account binding.
    #[must_use]
    pub const fn persistence_binding(self) -> (&'static str, Option<uuid::Uuid>) {
        match self {
            Self::ReadConnection => ("read_connection", None),
            Self::BookmarkWrite { account_id } => ("bookmark_write", Some(account_id)),
        }
    }
}

/// One freshly issued authorization intent, ready to be persisted and shown.
#[derive(Debug, Clone)]
pub struct AuthorizationIntent {
    /// The single-use state value binding the callback to this intent.
    pub state: String,
    /// The PKCE verifier; it must never appear in any URL or log line.
    pub code_verifier: String,
    /// The derived S256 challenge published in the authorization URL.
    pub code_challenge: String,
}

impl AuthorizationIntent {
    /// Issues a fresh intent from the OS random source.
    #[must_use]
    pub fn issue() -> Self {
        let code_verifier = pkce::generate_verifier();
        let state = pkce::generate_state();
        let code_challenge = pkce::challenge_from_verifier(&code_verifier);
        Self {
            state,
            code_verifier,
            code_challenge,
        }
    }
}

/// Percent-encodes one query component, leaving RFC 3986 unreserved characters as-is.
pub(crate) fn form_urlencode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(char::from(byte));
            }
            other => {
                let _ = std::fmt::Write::write_fmt(&mut encoded, format_args!("%{other:02X}"));
            }
        }
    }
    encoded
}

/// Builds the provider authorization URL for one issued intent.
///
/// # Errors
/// When the OAuth client identity or redirect target is not configured.
pub fn authorization_url(
    oauth: &x_core::config::OauthConfig,
    intent: &AuthorizationIntent,
    purpose: IntentPurpose,
) -> Result<String, FlowError> {
    let Some(client_id) = oauth.client_id.as_deref().filter(|id| !id.is_empty()) else {
        return Err(FlowError::Configuration);
    };
    let Some(redirect_uri) = oauth.redirect_uri.as_deref().filter(|uri| !uri.is_empty()) else {
        return Err(FlowError::Configuration);
    };
    let mut scopes = oauth.read_scopes.clone();
    if matches!(purpose, IntentPurpose::BookmarkWrite { .. })
        && !scopes.iter().any(|scope| scope == "bookmark.write")
    {
        scopes.push("bookmark.write".to_owned());
    }
    let scopes = scopes.join(" ");
    Ok(format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&code_challenge={}&code_challenge_method=S256",
        oauth.authorize_url,
        form_urlencode(client_id),
        form_urlencode(redirect_uri),
        form_urlencode(&scopes),
        form_urlencode(&intent.state),
        form_urlencode(&intent.code_challenge),
    ))
}
