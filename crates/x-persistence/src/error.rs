//! Typed failures raised while talking to PostgreSQL.

/// Why a persistence operation failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PersistenceError {
    /// The connection pool could not be established.
    #[error("the database connection could not be established")]
    Connect(#[source] sqlx::Error),
    /// The owned schema could not be applied or verified.
    #[error("the x_archive schema could not be applied")]
    Schema(#[source] sqlx::Error),
    /// A query against the owned schema failed.
    #[error("a database query failed")]
    Query(#[source] sqlx::Error),
}
