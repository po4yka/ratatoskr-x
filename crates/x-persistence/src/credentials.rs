//! Storage of encrypted OAuth credential envelopes and their rotation state.

use crate::database::Database;
use crate::error::PersistenceError;

/// One new credential row to persist.
#[derive(Debug)]
pub struct NewCredential<'a> {
    /// The account the credential belongs to and binds cryptographically.
    pub account_id: sqlx::types::Uuid,
    /// The sealed envelope bytes; empty once scrubbed.
    pub encrypted_payload: &'a [u8],
    /// The granted scopes exactly as observed, in provider order.
    pub granted_scopes: &'a [String],
    /// When the access token expires; `None` when the grant carries no expiry.
    pub expires_at: Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>>,
}

/// One stored credential as read back from the database.
#[derive(Debug, Clone)]
pub struct CredentialRow {
    /// The generated credential identity.
    pub id: sqlx::types::Uuid,
    /// The owning account.
    pub account_id: sqlx::types::Uuid,
    /// The sealed envelope bytes; empty once scrubbed.
    pub encrypted_payload: Vec<u8>,
    /// The granted scopes exactly as recorded.
    pub granted_scopes: Vec<String>,
    /// `active`, `expired`, or `revoked`.
    pub status: String,
    /// The recorded access-token expiry.
    pub expires_at: Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>>,
    /// The SHA-256 hex of the refresh token retired by the latest rotation.
    pub superseded_refresh_hash: Option<String>,
}

/// Persists one credential row with an explicit status vocabulary value.
///
/// # Errors
/// When the insert fails.
pub async fn insert_with_status(
    db: &Database,
    credential: &NewCredential<'_>,
    status: &str,
) -> Result<sqlx::types::Uuid, PersistenceError> {
    let row = sqlx::query_as::<_, (sqlx::types::Uuid,)>(
        r"
        INSERT INTO x_archive.credentials
            (account_id, encrypted_payload, granted_scopes, status, expires_at)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id
        ",
    )
    .bind(credential.account_id)
    .bind(credential.encrypted_payload)
    .bind(credential.granted_scopes)
    .bind(status)
    .bind(credential.expires_at)
    .fetch_one(db.pool())
    .await
    .map_err(PersistenceError::Query)?;
    Ok(row.0)
}

/// Reads the most recent credential of an account regardless of status.
///
/// # Errors
/// When the query fails.
pub async fn latest_for_account(
    db: &Database,
    account_id: sqlx::types::Uuid,
) -> Result<Option<CredentialRow>, PersistenceError> {
    let row = sqlx::query(
        r"
        SELECT id, account_id, encrypted_payload, granted_scopes, status,
               expires_at, superseded_refresh_hash
          FROM x_archive.credentials
         WHERE account_id = $1
         ORDER BY created_at DESC, id DESC
         LIMIT 1
        ",
    )
    .bind(account_id)
    .fetch_optional(db.pool())
    .await
    .map_err(PersistenceError::Query)?;
    row.as_ref()
        .map(credential_from_row)
        .transpose()
        .map_err(PersistenceError::Query)
}

/// Reads one account's connection state vocabulary value, if the row exists.
///
/// # Errors
/// When the query fails.
pub async fn account_state(
    db: &Database,
    account_id: sqlx::types::Uuid,
) -> Result<Option<String>, PersistenceError> {
    let state: Option<Option<String>> =
        sqlx::query_scalar("SELECT state FROM x_archive.accounts WHERE id = $1")
            .bind(account_id)
            .fetch_optional(db.pool())
            .await
            .map_err(PersistenceError::Query)?;
    Ok(state.flatten())
}

/// Moves one account to a connection-state vocabulary value at the given instant.
///
/// # Errors
/// When the update fails.
pub async fn set_account_state(
    db: &Database,
    account_id: sqlx::types::Uuid,
    state: &str,
    now: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>,
) -> Result<(), PersistenceError> {
    let _ = sqlx::query("UPDATE x_archive.accounts SET state = $2, updated_at = $3 WHERE id = $1")
        .bind(account_id)
        .bind(state)
        .bind(now)
        .execute(db.pool())
        .await
        .map_err(PersistenceError::Query)?;
    Ok(())
}

/// Locks and reads the account's newest active credential row with
/// `SELECT … FOR UPDATE` inside the caller's transaction.
///
/// # Errors
/// When the query fails.
pub async fn lock_active_for_account(
    conn: &mut sqlx::PgConnection,
    account_id: sqlx::types::Uuid,
) -> Result<Option<CredentialRow>, PersistenceError> {
    let row = sqlx::query(
        r"
        SELECT id, account_id, encrypted_payload, granted_scopes, status,
               expires_at, superseded_refresh_hash
          FROM x_archive.credentials
         WHERE account_id = $1 AND status = 'active'
         ORDER BY created_at DESC, id DESC
         LIMIT 1
         FOR UPDATE
        ",
    )
    .bind(account_id)
    .fetch_optional(conn)
    .await
    .map_err(PersistenceError::Query)?;
    row.as_ref()
        .map(credential_from_row)
        .transpose()
        .map_err(PersistenceError::Query)
}

/// Swaps one active credential's envelope for the rotated pair, moving the retired
/// token's hash into `superseded_refresh_hash`.
///
/// The update is guarded: it only lands while the row is still `active` and its
/// stored superseded hash still equals `expected_superseded`. Returns whether the
/// guarded update affected the row.
///
/// # Errors
/// When the update fails.
pub async fn rotate_guarded(
    db: &Database,
    credential_id: sqlx::types::Uuid,
    new_encrypted_payload: &[u8],
    new_expires_at: Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>>,
    retired_hash: &str,
    expected_superseded: Option<&str>,
) -> Result<bool, PersistenceError> {
    let result = sqlx::query(
        r"
        UPDATE x_archive.credentials
           SET encrypted_payload = $2,
               expires_at = $3,
               superseded_refresh_hash = $4
         WHERE id = $1
           AND status = 'active'
           AND superseded_refresh_hash IS NOT DISTINCT FROM $5
        ",
    )
    .bind(credential_id)
    .bind(new_encrypted_payload)
    .bind(new_expires_at)
    .bind(retired_hash)
    .bind(expected_superseded)
    .execute(db.pool())
    .await
    .map_err(PersistenceError::Query)?;
    Ok(result.rows_affected() == 1)
}

/// Swaps one credential's envelope for the rotated pair and retires the prior
/// refresh hash at the given instant.
///
/// # Errors
/// When the update fails.
pub async fn rotate(
    db: &Database,
    id: sqlx::types::Uuid,
    new_encrypted_payload: &[u8],
    retired_refresh_hash: &str,
    expires_at: Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>>,
) -> Result<(), PersistenceError> {
    let _ = sqlx::query(
        r"
        UPDATE x_archive.credentials
           SET encrypted_payload = $2,
               superseded_refresh_hash = $3,
               expires_at = $4
         WHERE id = $1
        ",
    )
    .bind(id)
    .bind(new_encrypted_payload)
    .bind(retired_refresh_hash)
    .bind(expires_at)
    .execute(db.pool())
    .await
    .map_err(PersistenceError::Query)?;
    Ok(())
}

/// Transaction-bound variant of [`rotate_guarded`] for flows that hold the row
/// lock across classification and the provider call.
///
/// # Errors
/// When the update fails.
pub async fn rotate_guarded_tx(
    conn: &mut sqlx::PgConnection,
    credential_id: sqlx::types::Uuid,
    new_encrypted_payload: &[u8],
    new_expires_at: Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>>,
    retired_hash: &str,
    expected_superseded: Option<&str>,
) -> Result<bool, PersistenceError> {
    let result = sqlx::query(
        r"
        UPDATE x_archive.credentials
           SET encrypted_payload = $2,
               expires_at = $3,
               superseded_refresh_hash = $4
         WHERE id = $1
           AND status = 'active'
           AND superseded_refresh_hash IS NOT DISTINCT FROM $5
        ",
    )
    .bind(credential_id)
    .bind(new_encrypted_payload)
    .bind(new_expires_at)
    .bind(retired_hash)
    .bind(expected_superseded)
    .execute(&mut *conn)
    .await
    .map_err(PersistenceError::Query)?;
    Ok(result.rows_affected() == 1)
}

/// Empties one credential's payload and marks it revoked inside the caller's
/// transaction: the reuse-detection and revocation scrub.
///
/// # Errors
/// When the update fails.
pub async fn scrub_revoke_tx(
    conn: &mut sqlx::PgConnection,
    credential_id: sqlx::types::Uuid,
) -> Result<(), PersistenceError> {
    let _ = sqlx::query(
        r"
        UPDATE x_archive.credentials
           SET encrypted_payload = ''::bytea,
               status = 'revoked'
         WHERE id = $1
        ",
    )
    .bind(credential_id)
    .execute(&mut *conn)
    .await
    .map_err(PersistenceError::Query)?;
    Ok(())
}

/// Moves one account's connection state inside the caller's transaction.
///
/// # Errors
/// When the update fails.
pub async fn set_account_state_tx(
    conn: &mut sqlx::PgConnection,
    account_id: sqlx::types::Uuid,
    state: &str,
    now: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>,
) -> Result<(), PersistenceError> {
    let _ = sqlx::query("UPDATE x_archive.accounts SET state = $2, updated_at = $3 WHERE id = $1")
        .bind(account_id)
        .bind(state)
        .bind(now)
        .execute(&mut *conn)
        .await
        .map_err(PersistenceError::Query)?;
    Ok(())
}

/// Marks one credential expired inside the caller's transaction while retaining
/// its payload as evidence.
///
/// # Errors
/// When the update fails.
pub async fn mark_expired_tx(
    conn: &mut sqlx::PgConnection,
    credential_id: sqlx::types::Uuid,
) -> Result<(), PersistenceError> {
    let _ = sqlx::query("UPDATE x_archive.credentials SET status = 'expired' WHERE id = $1")
        .bind(credential_id)
        .execute(&mut *conn)
        .await
        .map_err(PersistenceError::Query)?;
    Ok(())
}

/// Decodes one `credentials` row into its typed representation.
fn credential_from_row(row: &sqlx::postgres::PgRow) -> Result<CredentialRow, sqlx::error::Error> {
    use sqlx::Row as _;
    Ok(CredentialRow {
        id: row.try_get("id")?,
        account_id: row.try_get("account_id")?,
        encrypted_payload: row.try_get("encrypted_payload")?,
        granted_scopes: row.try_get("granted_scopes")?,
        status: row.try_get("status")?,
        expires_at: row.try_get("expires_at")?,
        superseded_refresh_hash: row.try_get("superseded_refresh_hash")?,
    })
}
