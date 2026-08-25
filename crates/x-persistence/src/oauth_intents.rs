//! Insertion and lookup of one-time PKCE authorization intents.
//!
//! Every timestamp is a bind parameter supplied by the caller's clock: the schema
//! carries no `DEFAULT now()` on intent columns and no statement relies on one.

use crate::database::Database;
use crate::error::PersistenceError;

/// One new authorization-intent row to persist.
#[derive(Debug)]
pub struct NewIntent<'a> {
    /// The internal user requesting the connection.
    pub internal_user_id: sqlx::types::Uuid,
    /// The lowercase SHA-256 hex digest of the state string.
    pub state_hash: &'a str,
    /// The sealed PKCE verifier envelope bytes.
    pub code_verifier_encrypted: &'a [u8],
    /// A fresh nonce recorded beside the sealed verifier.
    pub nonce: &'a str,
    /// The redirect URI bound into the flow.
    pub redirect_uri: &'a str,
    /// The requested scope list, in request order.
    pub requested_scopes: &'a [String],
    /// Creation time from the injected clock.
    pub created_at: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>,
    /// Expiry time from the injected clock.
    pub expires_at: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>,
}

/// One stored authorization intent as read back from the database.
#[derive(Debug, Clone)]
pub struct IntentRow {
    /// The generated intent identity.
    pub id: sqlx::types::Uuid,
    /// The internal user the intent belongs to.
    pub internal_user_id: sqlx::types::Uuid,
    /// The state digest the row is keyed by.
    pub state_hash: String,
    /// The still-sealed verifier envelope bytes.
    pub code_verifier_encrypted: Vec<u8>,
    /// The recorded nonce.
    pub nonce: String,
    /// The redirect URI bound into the flow.
    pub redirect_uri: String,
    /// The requested scope list, in request order.
    pub requested_scopes: Vec<String>,
    /// Creation time as written.
    pub created_at: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>,
    /// Expiry time as written.
    pub expires_at: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>,
    /// When (if ever) the intent was consumed.
    pub consumed_at: Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>>,
}

/// Persists one unconsumed authorization intent, returning its generated id.
///
/// # Errors
/// When the insert fails, including a duplicate state digest.
pub async fn insert_intent(
    db: &Database,
    intent: &NewIntent<'_>,
) -> Result<sqlx::types::Uuid, PersistenceError> {
    let row = sqlx::query_as::<_, (sqlx::types::Uuid,)>(
        r"
        INSERT INTO x_archive.oauth_intents
            (internal_user_id, state_hash, code_verifier_encrypted, nonce,
             redirect_uri, requested_scopes, created_at, expires_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING id
        ",
    )
    .bind(intent.internal_user_id)
    .bind(intent.state_hash)
    .bind(intent.code_verifier_encrypted)
    .bind(intent.nonce)
    .bind(intent.redirect_uri)
    .bind(intent.requested_scopes)
    .bind(intent.created_at)
    .bind(intent.expires_at)
    .fetch_one(db.pool())
    .await
    .map_err(PersistenceError::Query)?;
    Ok(row.0)
}

/// Finds an intent by the digest of its state string.
///
/// # Errors
/// When the query fails.
pub async fn find_intent_by_state_hash(
    db: &Database,
    state_hash: &str,
) -> Result<Option<IntentRow>, PersistenceError> {
    let row = sqlx::query(
        r"
        SELECT id, internal_user_id, state_hash, code_verifier_encrypted, nonce,
               redirect_uri, requested_scopes, created_at, expires_at, consumed_at
          FROM x_archive.oauth_intents
         WHERE state_hash = $1
        ",
    )
    .bind(state_hash)
    .fetch_optional(db.pool())
    .await
    .map_err(PersistenceError::Query)?;

    row.as_ref()
        .map(intent_from_row)
        .transpose()
        .map_err(PersistenceError::Query)
}

/// Decodes one `oauth_intents` row into its typed representation.
fn intent_from_row(row: &sqlx::postgres::PgRow) -> Result<IntentRow, sqlx::error::Error> {
    use sqlx::Row as _;
    Ok(IntentRow {
        id: row.try_get("id")?,
        internal_user_id: row.try_get("internal_user_id")?,
        state_hash: row.try_get("state_hash")?,
        code_verifier_encrypted: row.try_get("code_verifier_encrypted")?,
        nonce: row.try_get("nonce")?,
        redirect_uri: row.try_get("redirect_uri")?,
        requested_scopes: row.try_get("requested_scopes")?,
        created_at: row.try_get("created_at")?,
        expires_at: row.try_get("expires_at")?,
        consumed_at: row.try_get("consumed_at")?,
    })
}

/// The outcome of trying to consume one intent exactly once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumeOutcome {
    /// This call consumed the intent; the callback may proceed.
    Consumed,
    /// A previous call already consumed the intent: the presentation is a replay.
    AlreadyConsumed,
}

/// Marks one intent consumed at the given instant, atomically refusing a second consumption.
///
/// # Errors
/// When the update fails.
pub async fn consume_intent(
    db: &Database,
    id: sqlx::types::Uuid,
    now: sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>,
) -> Result<ConsumeOutcome, PersistenceError> {
    let result = sqlx::query(
        r"
        UPDATE x_archive.oauth_intents
           SET consumed_at = $2
         WHERE id = $1 AND consumed_at IS NULL
        ",
    )
    .bind(id)
    .bind(now)
    .execute(db.pool())
    .await
    .map_err(PersistenceError::Query)?;
    if result.rows_affected() == 0 {
        Ok(ConsumeOutcome::AlreadyConsumed)
    } else {
        Ok(ConsumeOutcome::Consumed)
    }
}
