//! The public-surface guard: an empty-but-valid envelope document is refused
//! through the typed error rather than silently producing an empty archive
//! batch.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use x_normalize::dto::Envelope;
use x_normalize::error::NormalizeError;
use x_normalize::normalize::normalize;

#[test]
fn empty_envelope_document_is_refused_with_typed_error() {
    let envelope: Envelope =
        serde_json::from_str("{}").expect("an empty object is a well-formed document");
    let outcome = normalize(&envelope);
    match outcome {
        Err(NormalizeError::MissingMember { object, member }) => {
            assert_eq!(object, "envelope", "the envelope itself is the offender");
            assert_eq!(member, "data", "the absent member is the post array");
        }
        other => panic!("an empty envelope must refuse with the typed error, got {other:?}"),
    }
}
