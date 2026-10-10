//! The public capture worker: resolve, retry, publish the owner-scoped source, report once
//! (XR-021 CONTRACTS.md S10 CD2, CD5, CD6).

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

mod outcomes;
mod retry;
mod support;

/// The explicit lane must never read a table that another tenant's sync writes.
#[test]
fn the_worker_module_never_reads_account_keyed_tables() {
    let source = include_str!("../../src/public_capture.rs");
    for table in ["posts", "users", "accounts", "bookmarks", "social_sources"] {
        assert!(
            !source.contains(&format!("x_archive.{table}")),
            "the public capture worker must not read x_archive.{table}"
        );
    }
}
