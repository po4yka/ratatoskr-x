//! Fixed request-budget windows charged before any provider call happens.
//!
//! The gate computes its target window from an injected clock and delegates every
//! statement here; this module keeps all SQL for `x_archive.api_budget_windows`.

use sqlx::types::Uuid;
use sqlx::types::chrono::{DateTime, Utc};

use crate::error::PersistenceError;

/// Creates the account's window row when absent, leaving any existing row alone.
const ENSURE_WINDOW: &str = "insert into x_archive.api_budget_windows \
     (account_id, window_start, window_seconds, request_cap, used_requests) \
     values ($1, $2, $3, $4, 0) \
     on conflict (account_id, window_start) do nothing";

/// Reads back the usage of the row the transaction just targeted, holding the
/// row's lock so concurrent charges serialize behind it.
const READ_USAGE: &str = "select used_requests \
     from x_archive.api_budget_windows \
     where account_id = $1 and window_start = $2 \
     for update";

/// Adds the reserved cost to the persisted usage.
const BUMP_USAGE: &str = "update x_archive.api_budget_windows \
     set used_requests = used_requests + $3 \
     where account_id = $1 and window_start = $2";

/// Releases refunded cost from the persisted usage, clamping at zero in the same
/// atomic statement.
const REFUND_USAGE: &str = "update x_archive.api_budget_windows \
     set used_requests = greatest(used_requests - $3, 0) \
     where account_id = $1 and window_start = $2";

/// Reads one account's window usage without taking any lock.
const PEEK_USAGE: &str = "select used_requests \
     from x_archive.api_budget_windows \
     where account_id = $1 and window_start = $2";

/// The outcome of one reservation attempt against a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowCharge {
    /// The cost was charged and committed.
    Accepted {
        /// The persisted usage after the charge.
        used_requests: i32,
    },
    /// The window cannot admit the cost; the transaction rolled back with nothing
    /// charged.
    Exhausted,
}

/// Charges `cost` request units against the account's named window inside one
/// transaction: the row is created when absent, read back, compared against the cap,
/// and either bumped and committed or refused with nothing charged.
///
/// # Errors
/// When any statement fails or the transaction cannot commit or roll back.
pub async fn charge(
    pool: &sqlx::PgPool,
    account_id: Uuid,
    window_start: DateTime<Utc>,
    window_seconds: i64,
    request_cap: i64,
    cost: i64,
) -> Result<WindowCharge, PersistenceError> {
    let mut transaction = pool.begin().await.map_err(PersistenceError::Query)?;
    sqlx::query(ENSURE_WINDOW)
        .bind(account_id)
        .bind(window_start)
        .bind(window_seconds)
        .bind(request_cap)
        .execute(&mut *transaction)
        .await
        .map_err(PersistenceError::Query)?;
    let used_requests: i32 = sqlx::query_scalar(READ_USAGE)
        .bind(account_id)
        .bind(window_start)
        .fetch_one(&mut *transaction)
        .await
        .map_err(PersistenceError::Query)?;
    if i64::from(used_requests) + cost > request_cap {
        transaction
            .rollback()
            .await
            .map_err(PersistenceError::Query)?;
        return Ok(WindowCharge::Exhausted);
    }
    sqlx::query(BUMP_USAGE)
        .bind(account_id)
        .bind(window_start)
        .bind(cost)
        .execute(&mut *transaction)
        .await
        .map_err(PersistenceError::Query)?;
    transaction
        .commit()
        .await
        .map_err(PersistenceError::Query)?;
    Ok(WindowCharge::Accepted { used_requests })
}

/// Releases `cost` previously charged to the account's window in one atomic
/// statement; a refund larger than the current usage clamps at zero instead of
/// driving the usage negative. A window without a row is left as it is.
///
/// # Errors
/// When the statement fails.
pub async fn refund(
    pool: &sqlx::PgPool,
    account_id: Uuid,
    window_start: DateTime<Utc>,
    cost: i64,
) -> Result<(), PersistenceError> {
    sqlx::query(REFUND_USAGE)
        .bind(account_id)
        .bind(window_start)
        .bind(cost)
        .execute(pool)
        .await
        .map_err(PersistenceError::Query)?;
    Ok(())
}

/// Reads the persisted usage of one account's window, or [`None`] when the window
/// has no row yet.
///
/// # Errors
/// When the query fails.
pub async fn window_usage(
    pool: &sqlx::PgPool,
    account_id: Uuid,
    window_start: DateTime<Utc>,
) -> Result<Option<i32>, PersistenceError> {
    sqlx::query_scalar(PEEK_USAGE)
        .bind(account_id)
        .bind(window_start)
        .fetch_optional(pool)
        .await
        .map_err(PersistenceError::Query)
}
