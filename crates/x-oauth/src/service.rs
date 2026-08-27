//! The connection service: code exchange and activation, rotation-aware refresh,
//! and revocation.

use std::sync::Arc;

use x_persistence::database::Database;
use x_persistence::error::PersistenceError;

use crate::callback::AcceptedCallback;
use crate::cipher::{Purpose, TokenCipher};
use crate::clock::Clock;
use crate::error::FlowError;
use crate::intent::IntentPurpose;
use crate::payload::CredentialPayload;
use crate::scope;
use crate::token_client::TokenClient;

/// One wired connection service over one database, cipher, provider client, clock.
#[derive(Clone)]
pub struct ConnectionService {
    db: Database,
    cipher: TokenCipher,
    client: TokenClient,
    clock: Arc<dyn Clock>,
    oauth: x_core::config::OauthConfig,
}

impl std::fmt::Debug for ConnectionService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConnectionService([redacted])")
    }
}

impl ConnectionService {
    /// Wires the service together.
    #[must_use]
    pub fn new(
        db: Database,
        cipher: TokenCipher,
        client: TokenClient,
        clock: Arc<dyn Clock>,
        oauth: x_core::config::OauthConfig,
    ) -> Self {
        Self {
            db,
            cipher,
            client,
            clock,
            oauth,
        }
    }

    /// Exchanges an accepted intent's authorization code, audits scopes, and
    /// activates the account with an encrypted credential.
    ///
    /// # Errors
    /// Configuration absence, transport failure, scope downgrade, or any
    /// persistence/cipher fault underneath.
    pub async fn exchange_code(
        &self,
        account_id: uuid::Uuid,
        accepted: &AcceptedCallback,
        code: &str,
    ) -> Result<(), FlowError> {
        let redirect_uri = self
            .oauth
            .redirect_uri
            .clone()
            .filter(|uri| !uri.is_empty())
            .ok_or(FlowError::Configuration)?;
        let tokens = self
            .client
            .exchange_code(code, &redirect_uri, &accepted.code_verifier)
            .await?;
        let granted = observed_grant(tokens.scope.as_deref(), &self.oauth.read_scopes);
        let missing = scope::missing_scopes(&self.oauth.read_scopes, &granted);
        if !missing.is_empty() {
            // The observed grant stays auditable in a non-active row even though
            // the connection never activates.
            x_persistence::credentials::insert_with_status(
                &self.db,
                &x_persistence::credentials::NewCredential {
                    account_id,
                    encrypted_payload: &[],
                    granted_scopes: &granted,
                    expires_at: None,
                },
                "expired",
            )
            .await
            .map_err(Self::persistence)?;
            return Err(FlowError::Downgrade {
                missing_scopes: missing,
            });
        }
        let payload = CredentialPayload {
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
        };
        let sealed = self
            .cipher
            .seal(account_id, Purpose::Credential, &payload.encode());
        let expires_at = self.clock.now() + std::time::Duration::from_secs(tokens.expires_in);
        x_persistence::credentials::insert_with_status(
            &self.db,
            &x_persistence::credentials::NewCredential {
                account_id,
                encrypted_payload: &sealed,
                granted_scopes: &granted,
                expires_at: Some(expires_at),
            },
            "active",
        )
        .await
        .map_err(Self::persistence)?;
        let now = self.clock.now();
        x_persistence::credentials::set_account_state(&self.db, account_id, "connected", now)
            .await
            .map_err(Self::persistence)?;
        Ok(())
    }

    /// Exchanges an accepted bookmark-write extension for its account-bound credential.
    ///
    /// # Errors
    /// When the accepted callback is not a bookmark-write intent, or when the delegated
    /// provider exchange fails.
    pub async fn exchange_bookmark_write_code(
        &self,
        accepted: &AcceptedCallback,
        code: &str,
    ) -> Result<(), FlowError> {
        let IntentPurpose::BookmarkWrite { account_id } = accepted.purpose else {
            return Err(FlowError::Configuration);
        };
        let mut required_scopes = self.oauth.read_scopes.clone();
        if !required_scopes
            .iter()
            .any(|scope| scope == "bookmark.write")
        {
            required_scopes.push("bookmark.write".to_owned());
        }
        if accepted.requested_scopes != required_scopes {
            return Err(FlowError::Configuration);
        }
        let redirect_uri = self
            .oauth
            .redirect_uri
            .clone()
            .filter(|uri| !uri.is_empty())
            .ok_or(FlowError::Configuration)?;
        let tokens = self
            .client
            .exchange_code(code, &redirect_uri, &accepted.code_verifier)
            .await?;
        let granted = observed_grant(tokens.scope.as_deref(), &required_scopes);
        let missing = scope::missing_scopes(&required_scopes, &granted);
        if !missing.is_empty() {
            x_persistence::credentials::insert_rejected_write_grant(
                &self.db,
                account_id,
                &granted,
                "scope_downgrade",
            )
            .await
            .map_err(Self::persistence)?;
            return Err(FlowError::Downgrade {
                missing_scopes: missing,
            });
        }
        let provider_user_id = self
            .client
            .authenticated_user_id(&tokens.access_token)
            .await?;
        let payload = CredentialPayload {
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
        };
        let sealed = self
            .cipher
            .seal(account_id, Purpose::Credential, &payload.encode());
        let now = self.clock.now();
        let expires_at = now + std::time::Duration::from_secs(tokens.expires_in);
        let activation = x_persistence::bookmark_write_authorizations::WriteGrantActivation {
            oauth_intent_id: accepted.intent_id,
            account_id,
            internal_user_id: accepted.internal_user_id,
            provider_user_id: &provider_user_id,
            encrypted_payload: &sealed,
            granted_scopes: &granted,
            expires_at: Some(expires_at),
            authorized_at: now,
        };
        match x_persistence::bookmark_write_authorizations::activate(&self.db, &activation)
            .await
            .map_err(Self::persistence)?
        {
            x_persistence::bookmark_write_authorizations::ActivationOutcome::Activated => Ok(()),
            x_persistence::bookmark_write_authorizations::ActivationOutcome::BindingMismatch => {
                x_persistence::credentials::insert_rejected_write_grant(
                    &self.db,
                    account_id,
                    &granted,
                    "provider_identity_mismatch",
                )
                .await
                .map_err(Self::persistence)?;
                Err(FlowError::ProviderIdentityMismatch)
            }
        }
    }

    /// Refreshes the account's credential, classifying rotation, reuse, staleness,
    /// upstream invalidation, and transport failure.
    ///
    /// # Errors
    /// Every refusal class of the refresh matrix; see [`FlowError`].
    pub async fn refresh(
        &self,
        account_id: uuid::Uuid,
        presented_refresh_token: Option<&str>,
    ) -> Result<(), FlowError> {
        // One transaction per refresh: the row lock is held across classification,
        // the provider call, and the guarded update, serializing racers per account.
        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .map_err(PersistenceError::Query)
            .map_err(Self::persistence)?;
        let row = x_persistence::credentials::lock_active_for_account(&mut tx, account_id)
            .await
            .map_err(Self::persistence)?
            .ok_or(FlowError::CredentialInactive)?;
        let opened = self
            .cipher
            .open(account_id, Purpose::Credential, &row.encrypted_payload)
            .map_err(FlowError::Cipher)?;
        let stored =
            CredentialPayload::decode(&opened).map_err(|_| FlowError::MalformedProviderResponse)?;
        // A caller presenting no token adopts the one current at its turn under the lock.
        let presented = presented_refresh_token.unwrap_or(&stored.refresh_token);
        if presented != stored.refresh_token {
            let retired = row.superseded_refresh_hash.as_deref().unwrap_or_default();
            if sha256_hex(presented.as_bytes()) == retired {
                // Reuse of the retired family member revokes everything.
                x_persistence::credentials::scrub_revoke_tx(&mut tx, row.id)
                    .await
                    .map_err(Self::persistence)?;
                let now = self.clock.now();
                x_persistence::credentials::set_account_state_tx(
                    &mut tx,
                    account_id,
                    "reauth_required",
                    now,
                )
                .await
                .map_err(Self::persistence)?;
                tx.commit()
                    .await
                    .map_err(x_persistence::error::PersistenceError::Query)
                    .map_err(Self::persistence)?;
                return Err(FlowError::RefreshReplay);
            }
            // A token matching neither current nor retired is refused without
            // disturbing the stored credential; dropping the transaction rolls back.
            return Err(FlowError::StaleRefreshToken);
        }
        let response = match self.client.refresh_token(presented).await {
            Ok(response) => response,
            // A completed provider rejection expires the credential while
            // retaining the evidence payload, and marks reauthorization.
            Err(FlowError::UpstreamInvalidation) => {
                x_persistence::credentials::mark_expired_tx(&mut tx, row.id)
                    .await
                    .map_err(Self::persistence)?;
                let now = self.clock.now();
                x_persistence::credentials::set_account_state_tx(
                    &mut tx,
                    account_id,
                    "reauth_required",
                    now,
                )
                .await
                .map_err(Self::persistence)?;
                tx.commit()
                    .await
                    .map_err(x_persistence::error::PersistenceError::Query)
                    .map_err(Self::persistence)?;
                return Err(FlowError::UpstreamInvalidation);
            }
            Err(other) => return Err(other),
        };
        let rotated = CredentialPayload {
            access_token: response.access_token,
            refresh_token: response.refresh_token,
        };
        let sealed = self
            .cipher
            .seal(account_id, Purpose::Credential, &rotated.encode());
        let retired_hash = sha256_hex(stored.refresh_token.as_bytes());
        let expires_at = self.clock.now() + std::time::Duration::from_secs(response.expires_in);
        let applied = x_persistence::credentials::rotate_guarded_tx(
            &mut tx,
            row.id,
            &sealed,
            Some(expires_at),
            &retired_hash,
            row.superseded_refresh_hash.as_deref(),
        )
        .await
        .map_err(Self::persistence)?;
        if !applied {
            return Err(FlowError::CredentialInactive);
        }
        tx.commit()
            .await
            .map_err(x_persistence::error::PersistenceError::Query)
            .map_err(Self::persistence)?;
        Ok(())
    }

    /// Revokes the connection at the provider and scrubs local material idempotently.
    ///
    /// # Errors
    /// Only when a transport-class failure prevents even notifying the provider of a
    /// still-active credential; already-revoked and absent credentials succeed.
    pub async fn revoke(&self, account_id: uuid::Uuid) -> Result<(), FlowError> {
        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .map_err(PersistenceError::Query)
            .map_err(Self::persistence)?;
        // Already-revoked or absent credentials succeed silently: no provider
        // contact, nothing to scrub, state stays consistent.
        let Some(row) = x_persistence::credentials::lock_active_for_account(&mut tx, account_id)
            .await
            .map_err(Self::persistence)?
        else {
            tx.commit()
                .await
                .map_err(x_persistence::error::PersistenceError::Query)
                .map_err(Self::persistence)?;
            return Ok(());
        };
        let opened = self
            .cipher
            .open(account_id, Purpose::Credential, &row.encrypted_payload)
            .map_err(FlowError::Cipher)?;
        let stored =
            CredentialPayload::decode(&opened).map_err(|_| FlowError::MalformedProviderResponse)?;
        // Best-effort notification: any completed response confirms it; only a
        // transport failure aborts before local scrubbing.
        self.client.revoke(&stored.access_token).await?;
        x_persistence::credentials::scrub_revoke_tx(&mut tx, row.id)
            .await
            .map_err(Self::persistence)?;
        let now = self.clock.now();
        x_persistence::credentials::set_account_state_tx(&mut tx, account_id, "revoked", now)
            .await
            .map_err(Self::persistence)?;
        tx.commit()
            .await
            .map_err(x_persistence::error::PersistenceError::Query)
            .map_err(Self::persistence)?;
        Ok(())
    }

    /// Maps persistence faults onto the flow error type.
    fn persistence(error: PersistenceError) -> FlowError {
        FlowError::Persistence(error)
    }
}

/// The granted scopes exactly as the provider echoed them. Per RFC 6749 §4.1.4
/// an omitted scope parameter means granted-equals-requested, so the caller
/// substitutes its requested set when this returns [`None`].
fn observed_grant(scope: Option<&str>, requested: &[String]) -> Vec<String> {
    match scope {
        Some(scope_string) => scope_string.split_whitespace().map(str::to_owned).collect(),
        None => requested.to_vec(),
    }
}

/// The lowercase SHA-256 hex digest used to fingerprint retired refresh tokens.
fn sha256_hex(value: &[u8]) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(value);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = std::fmt::Write::write_fmt(&mut hex, format_args!("{byte:02x}"));
    }
    hex
}
