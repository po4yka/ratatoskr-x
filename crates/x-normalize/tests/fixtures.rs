//! Every committed fixture decodes into the envelope type, and the two
//! structural-violation fixtures fail validation with typed errors naming the
//! offending object and member.
//!
//! Fixture payloads are synthetic and shaped strictly after the official X API
//! v2 documentation; see `fixtures/x-api/`.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::path::PathBuf;

use serde::de::DeserializeOwned;
use x_normalize::dto::Envelope;
use x_normalize::error::NormalizeError;

/// Resolves a fixture file below the repository-root `fixtures/x-api` folder.
fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/x-api")
        .join(name)
}

/// Decodes a fixture file into any DTO shape the suite needs.
fn decode<T: DeserializeOwned>(name: &str) -> T {
    let raw = std::fs::read_to_string(fixture_path(name))
        .unwrap_or_else(|e| panic!("fixture {name} must be readable: {e}"));
    serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("fixture {name} must decode into its envelope type: {e}"))
}

#[test]
fn every_committed_fixture_deserializes_into_the_envelope_type() {
    for name in [
        "tweet_single.json",
        "thread.json",
        "quote_dangling_target.json",
        "retweet.json",
        "media_post.json",
        "article_backed_longform.json",
        "deleted_author.json",
        "unknown_field.json",
    ] {
        let envelope: Envelope = decode(name);
        envelope
            .validate()
            .unwrap_or_else(|e| panic!("fixture {name} must pass required-member validation: {e}"));
    }

    let missing_id: Envelope = decode("missing_id.json");
    match missing_id.validate() {
        Err(NormalizeError::MissingMember { object, member }) => {
            assert_eq!(object, "post", "the post object is the offender");
            assert_eq!(member, "id", "the provider id is the absent member");
        }
        other => panic!("missing_id.json must refuse validation, got {other:?}"),
    }

    let bad_timestamp: Envelope = decode("bad_timestamp.json");
    match bad_timestamp.validate() {
        Err(NormalizeError::InvalidMember { object, member }) => {
            assert_eq!(object, "post", "the post object is the offender");
            assert_eq!(
                member, "created_at",
                "the publication timestamp is unparseable"
            );
        }
        other => panic!("bad_timestamp.json must refuse validation, got {other:?}"),
    }
}
