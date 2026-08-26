//! Pure normalization of official X API payloads into `x_archive` row-shaped
//! records: authors, posts, reply/quote/repost relations, and media metadata.
//! The crate performs no network or database access; synchronization layers
//! feed envelopes in and persist the emitted records.

/// Parser generation stamped into every normalized record. Bump this whenever
/// mapping semantics change so retained payloads can be re-parsed selectively.
pub const PARSER_VERSION: i32 = 1;

pub mod dto;
pub mod error;
pub mod normalize;
pub mod records;
