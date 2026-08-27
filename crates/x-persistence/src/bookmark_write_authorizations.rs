//! Atomic activation and lookup of separately consented bookmark-write authority.

use crate::database::Database;
use crate::error::PersistenceError;

/// Account facts locked before replacing its credential family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountBinding {
    /// The Ratatoskr owner of the connected account.
    pub internal_user_id: sqlx::types::Uuid,
    /// The provider identity that the account represents.
    pub provider_user_id: String,
}

/// Reads and locks one account for a write-grant activation transaction.
async fn lock_account(
    connection: &mut sqlx::PgConnection,
    account_id: sqlx::types::Uuid,
) -> Result<Option<AccountBinding>, PersistenceError> {
    let row = sqlx::query_as::<_, (sqlx::types::Uuid, String)>(
        "select internal_user_id, provider_user_id from x_archive.accounts where id = $1 for update",
    )
    .bind(account_id)
    .fetch_optional(&mut *connection)
    .await
    .map_err(PersistenceError::Query)?;
    Ok(
        row.map(|(internal_user_id, provider_user_id)| AccountBinding {
            internal_user_id,
            provider_user_id,
        }),
    )
}

/// The values committed together when a complete matching grant is accepted.
#[derive(Debug)]
pub struct WriteGrantActivation<'a> {
    /// The accepted persisted PKCE intent.
    pub oauth_intent_id: sqlx::types::Uuid,
    /// The existing connected account being extended.
    pub account_id: sqlx::types::Uuid,
    /// The authenticated Ratatoskr owner carried by the intent.
    pub internal_user_id: sqlx::types::Uuid,
    /// Identity returned by X for the new access token.
    pub provider_user_id: &'a str,
    /// Newly sealed credential payload.
    pub encrypted_payload: &'a [u8],
    /// Exact complete provider scope set.
    pub granted_scopes: &'a [String],
    /// Access-token expiry.
    pub expires_at: Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>>,
    /// Activation observation instant.
    pub authorized_at: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>,
}

/// Why an otherwise complete write grant was not activated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationOutcome {
    /// Credential replacement and local write authority committed together.
    Activated,
    /// The account was absent or its owner/provider identity did not match.
    BindingMismatch,
}

/// Replaces the active credential and activates local bookmark-write authority in one transaction.
///
/// # Errors
/// When account lookup or either atomic write fails.
pub async fn activate(
    db: &Database,
    activation: &WriteGrantActivation<'_>,
) -> Result<ActivationOutcome, PersistenceError> {
    let mut transaction = db.pool().begin().await.map_err(PersistenceError::Query)?;
    let Some(account) = lock_account(&mut transaction, activation.account_id).await? else {
        return Ok(ActivationOutcome::BindingMismatch);
    };
    if account.internal_user_id != activation.internal_user_id
        || account.provider_user_id != activation.provider_user_id
    {
        return Ok(ActivationOutcome::BindingMismatch);
    }

    sqlx::query(
        "update x_archive.credentials set status = 'expired' where account_id = $1 and status = 'active'",
    )
    .bind(activation.account_id)
    .execute(&mut *transaction)
    .await
    .map_err(PersistenceError::Query)?;
    sqlx::query(
        "insert into x_archive.credentials \
         (account_id, encrypted_payload, granted_scopes, status, expires_at) \
         values ($1, $2, $3, 'active', $4)",
    )
    .bind(activation.account_id)
    .bind(activation.encrypted_payload)
    .bind(activation.granted_scopes)
    .bind(activation.expires_at)
    .execute(&mut *transaction)
    .await
    .map_err(PersistenceError::Query)?;
    sqlx::query(
        "insert into x_archive.bookmark_write_authorizations \
         (account_id, oauth_intent_id, granted_scopes, status, authorized_at, updated_at) \
         values ($1, $2, $3, 'active', $4, $4) \
         on conflict (account_id) do update set \
             oauth_intent_id = excluded.oauth_intent_id, \
             granted_scopes = excluded.granted_scopes, \
             status = 'active', authorized_at = excluded.authorized_at, \
             revoked_at = null, updated_at = excluded.updated_at",
    )
    .bind(activation.account_id)
    .bind(activation.oauth_intent_id)
    .bind(activation.granted_scopes)
    .bind(activation.authorized_at)
    .execute(&mut *transaction)
    .await
    .map_err(PersistenceError::Query)?;
    transaction
        .commit()
        .await
        .map_err(PersistenceError::Query)?;
    Ok(ActivationOutcome::Activated)
}
