//! The `PostgreSQL` pool and the embedded `x_archive` schema for `ratatoskr-x`.

pub mod bookmark_write_authorizations;
pub mod budget_windows;
pub mod credentials;
pub mod database;
pub mod error;
pub mod oauth_intents;

#[cfg(feature = "test-support")]
pub mod test_support;
