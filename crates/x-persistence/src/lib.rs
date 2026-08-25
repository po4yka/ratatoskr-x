//! The `PostgreSQL` pool and the embedded `x_archive` schema for `ratatoskr-x`.

pub mod database;
pub mod error;

#[cfg(feature = "test-support")]
pub mod test_support;
