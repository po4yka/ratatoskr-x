//! The disposable-database harness. Compiled only under `test-support`, which no service
//! binary enables.

use crate::database::Database;
use crate::error::PersistenceError;

/// The administrative connection the harness uses to create and drop databases.
pub const ADMIN_URL_ENV: &str = "X_TEST_DATABASE_URL";

/// Connections each disposable database pool may hold during a test.
const TEST_POOL_SIZE: u32 = 2;

/// A uniquely named database built from `schema.sql` and removed on [`TestDatabase::cleanup`].
#[derive(Debug)]
pub struct TestDatabase {
    /// A bounded pool already connected to the disposable database.
    pub database: Database,
    name: String,
}

impl TestDatabase {
    /// The administrative URL the harness uses, overridable from the environment.
    ///
    /// # Panics
    /// Never; falls back to the documented loopback default.
    #[must_use]
    #[allow(clippy::disallowed_methods, reason = "test-only environment override")]
    pub fn admin_url() -> String {
        std::env::var(ADMIN_URL_ENV)
            .unwrap_or_else(|_| "postgres://x:x@127.0.0.1:5432/x".to_owned())
    }

    /// Creates a uniquely named database from `template0` with ICU collation, connects to it,
    /// and applies the schema.
    ///
    /// # Errors
    /// When the administrative connection, database creation, pool, or schema application fails.
    pub async fn create() -> Result<Self, PersistenceError> {
        let admin_url = Self::admin_url();
        let base = admin_url
            .rsplit_once('/')
            .map(|(base, _)| base.to_owned())
            .ok_or_else(|| {
                PersistenceError::Connect(sqlx::Error::Configuration(
                    "the administrative URL has no database path".into(),
                ))
            })?;

        let admin = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .map_err(PersistenceError::Connect)?;

        let name = format!("ratatoskr_x_test_{}", uuid::Uuid::now_v7().simple());
        let created = sqlx::query(&format!(
            r#"create database "{name}" template template0 locale_provider icu icu_locale 'und-x-icu' encoding 'UTF8'"#
        ))
        .execute(&admin)
        .await;
        admin.close().await;
        created.map_err(PersistenceError::Query)?;

        let database = Database::connect(&format!("{base}/{name}"), TEST_POOL_SIZE).await?;
        database.apply_schema().await?;
        Ok(Self { database, name })
    }

    /// The generated database name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Closes the pool and drops the database with `FORCE`.
    ///
    /// # Errors
    /// When the drop fails; the pool is closed regardless.
    pub async fn cleanup(self) -> Result<(), PersistenceError> {
        self.database.pool().close().await;
        let admin = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&Self::admin_url())
            .await
            .map_err(PersistenceError::Connect)?;
        let dropped = sqlx::query(&format!(
            r#"drop database if exists "{}" with (force)"#,
            self.name
        ))
        .execute(&admin)
        .await;
        admin.close().await;
        dropped.map_err(PersistenceError::Query)?;
        Ok(())
    }
}
