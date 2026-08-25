//! The connection pool and the embedded `x_archive` schema application.

use crate::error::PersistenceError;

/// The single editable definition of the owned database shape. There are no migrations while
/// development status forbids them: this file changes in place, and test databases are built
/// from it.
const SCHEMA: &str = include_str!("../../../schema.sql");

/// The advisory-lock key serializing concurrent schema applications.
const SCHEMA_LOCK: i64 = 6_941_023_317_588_530_517;

/// A pooled handle to the service's PostgreSQL database.
#[derive(Debug, Clone)]
pub struct Database {
    pool: sqlx::PgPool,
}

impl Database {
    /// Connects a bounded pool to the given URL.
    ///
    /// # Errors
    /// When the pool cannot be established.
    pub async fn connect(url: &str, max_connections: u32) -> Result<Self, PersistenceError> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(url)
            .await
            .map_err(PersistenceError::Connect)?;
        Ok(Self { pool })
    }

    /// Applies the embedded schema under an advisory lock inside one transaction.
    ///
    /// # Errors
    /// When any statement fails or the transaction cannot commit.
    pub async fn apply_schema(&self) -> Result<(), PersistenceError> {
        let mut transaction = self.pool.begin().await.map_err(PersistenceError::Schema)?;
        sqlx::query("select pg_advisory_xact_lock($1)")
            .bind(SCHEMA_LOCK)
            .execute(&mut *transaction)
            .await
            .map_err(PersistenceError::Schema)?;
        sqlx::raw_sql(SCHEMA)
            .execute(&mut *transaction)
            .await
            .map_err(PersistenceError::Schema)?;
        transaction.commit().await.map_err(PersistenceError::Schema)
    }

    /// Runs one statement and returns the result.
    ///
    /// # Errors
    /// When the query fails.
    pub async fn query_raw(
        &self,
        sql: &str,
    ) -> Result<sqlx::postgres::PgQueryResult, PersistenceError> {
        sqlx::query(sql)
            .execute(&self.pool)
            .await
            .map_err(PersistenceError::Query)
    }

    /// The underlying pool for tests that need bespoke queries.
    #[must_use]
    pub fn pool(&self) -> &sqlx::PgPool {
        &self.pool
    }
}
