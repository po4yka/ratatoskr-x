//! Typed refusals: an unresolvable author refuses the whole envelope without
//! partial output, and structural violations name their offender.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::path::PathBuf;

use x_normalize::dto::Envelope;
use x_normalize::error::NormalizeError;
use x_normalize::normalize::normalize;

/// Decodes a committed fixture into the envelope.
fn decode(name: &str) -> Envelope {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/x-api")
        .join(name);
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("fixture {name} must be readable: {e}"));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("fixture {name} must decode: {e}"))
}

#[test]
fn unresolved_author_refuses_whole_envelope() {
    let envelope = decode("deleted_author.json");
    let outcome = normalize(&envelope);

    match outcome {
        Err(NormalizeError::UnresolvedAuthor { post_provider_id }) => {
            assert_eq!(
                post_provider_id, "800900100",
                "the refusal names the affected post"
            );
        }
        other => panic!("the deleted-author edge must refuse with the typed error, got {other:?}"),
    }
}

#[test]
fn structural_violations_are_typed_errors() {
    let missing_id = normalize(&decode("missing_id.json"));
    match missing_id {
        Err(NormalizeError::MissingMember { object, member }) => {
            assert_eq!(object, "post");
            assert_eq!(member, "id");
        }
        other => panic!("a missing provider id must refuse as MissingMember, got {other:?}"),
    }

    let bad_timestamp = normalize(&decode("bad_timestamp.json"));
    match bad_timestamp {
        Err(NormalizeError::InvalidMember { object, member }) => {
            assert_eq!(object, "post");
            assert_eq!(member, "created_at");
        }
        other => panic!("an unparseable timestamp must refuse as InvalidMember, got {other:?}"),
    }
}
