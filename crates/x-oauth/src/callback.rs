//! Callback resolution against persisted authorization intents.
//!
//! Four outcomes are distinct: acceptance (exactly once), expiry, replay, and an
//! unmatchable state. Every timestamp written here is a bind parameter derived
//! from the injected clock, never a `DEFAULT now()`.

use x_persistence::database::Database;
use x_persistence::oauth_intents::{self, ConsumeOutcome};

use crate::cipher::{Purpose, TokenCipher};
use crate::clock::Clock;
use crate::error::{CipherError, FlowError};
use crate::intent::IntentPurpose;

/// What became of one presented callback state.
#[derive(Debug)]
pub enum CallbackResolution {
    /// The intent was live and this presentation consumed it.
    Accepted(AcceptedCallback),
    /// The intent exists but its expiry has passed; it stays unconsumed.
    Expired,
    /// The intent was already consumed by an earlier acceptance.
    Replayed,
    /// No intent carries this state's digest.
    Unmatched,
}

/// The binding and verifier exposed by exactly one acceptance.
#[derive(Debug, Clone)]
pub struct AcceptedCallback {
    /// The persisted single-use intent whose binding was accepted.
    pub intent_id: uuid::Uuid,
    /// The internal user the accepted intent belongs to.
    pub internal_user_id: uuid::Uuid,
    /// The authorization purpose and any existing-account binding.
    pub purpose: IntentPurpose,
    /// The exact provider scopes requested by the accepted intent.
    pub requested_scopes: Vec<String>,
    /// The decrypted PKCE verifier proving possession of the challenge.
    pub code_verifier: String,
}

/// The lowercase SHA-256 hex digest that keys intent lookup.
#[must_use]
pub fn state_digest(state: &str) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(state.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = std::fmt::Write::write_fmt(&mut hex, format_args!("{byte:02x}"));
    }
    hex
}

/// Resolves one presented callback state against persisted intents.
///
/// # Errors
/// When the database or the verifier envelope fails underneath the resolution.
pub async fn resolve_callback(
    db: &Database,
    cipher: &TokenCipher,
    clock: &dyn Clock,
    presented_state: &str,
) -> Result<CallbackResolution, FlowError> {
    let row = oauth_intents::find_intent_by_state_hash(db, &state_digest(presented_state))
        .await
        .map_err(FlowError::Persistence)?;
    let Some(intent) = row else {
        return Ok(CallbackResolution::Unmatched);
    };
    if clock.now() >= intent.expires_at {
        return Ok(CallbackResolution::Expired);
    }
    let consumed = oauth_intents::consume_intent(db, intent.id, clock.now())
        .await
        .map_err(FlowError::Persistence)?;
    if consumed == ConsumeOutcome::AlreadyConsumed {
        return Ok(CallbackResolution::Replayed);
    }
    let verifier_bytes = cipher
        .open(
            intent.internal_user_id,
            Purpose::IntentVerifier,
            &intent.code_verifier_encrypted,
        )
        .map_err(FlowError::Cipher)?;
    let purpose = match (intent.purpose.as_str(), intent.account_id) {
        ("read_connection", None) => IntentPurpose::ReadConnection,
        ("bookmark_write", Some(account_id)) => IntentPurpose::BookmarkWrite { account_id },
        _ => return Err(FlowError::Configuration),
    };
    Ok(CallbackResolution::Accepted(AcceptedCallback {
        intent_id: intent.id,
        internal_user_id: intent.internal_user_id,
        purpose,
        requested_scopes: intent.requested_scopes,
        code_verifier: String::from_utf8(verifier_bytes)
            .map_err(|_| FlowError::Cipher(CipherError::Auth))?,
    }))
}
