//! Record-and-preserve behavior of the DTO layer: unmodeled payload members
//! survive decoding name-and-value intact on every object view.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::path::PathBuf;

use serde_json::{Value, json};
use x_normalize::dto::Envelope;

/// Resolves a fixture file below the repository-root `fixtures/x-api` folder.
fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/x-api")
        .join(name)
}

/// Decodes a committed fixture into the envelope.
fn decode(name: &str) -> Envelope {
    let raw = std::fs::read_to_string(fixture_path(name))
        .unwrap_or_else(|e| panic!("fixture {name} must be readable: {e}"));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("fixture {name} must decode: {e}"))
}

#[test]
fn unmodeled_members_are_preserved_per_object() {
    let envelope = decode("unknown_field.json");

    let post = &envelope.posts()[0];
    assert_eq!(
        post.extension.get("source"),
        Some(&json!("Fixture Client for Web")),
        "unmodeled post members survive verbatim"
    );
    assert_eq!(post.extension.get("community_id"), Some(&json!("777000")));
    assert_eq!(
        post.extension.get("possibly_sensitive"),
        Some(&json!(false))
    );

    let user = envelope
        .included_users()
        .iter()
        .find(|u| u.id.as_deref() == Some("9008"))
        .expect("the fixture author is present");
    assert_eq!(
        user.extension.get("verified_type"),
        Some(&json!("blue")),
        "unmodeled user members survive verbatim"
    );
    assert_eq!(
        user.extension.get("subscription_type"),
        Some(&json!("Premium")),
        "every unmodeled user member survives, not just the first"
    );

    let media = &envelope.included_media()[0];
    let non_public = media
        .extension
        .get("non_public_metrics")
        .expect("media metric variants ride preservation");
    assert_eq!(
        non_public.get("playback_0_count"),
        Some(&json!(10)),
        "nested values inside preserved members stay intact"
    );

    assert_eq!(
        envelope.extension.get("tracking"),
        Some(&json!("unmodeled top-level envelope member")),
        "envelope-level members survive too"
    );
    assert!(
        envelope.included_extension().get("posts").is_some(),
        "newer include containers ride preservation instead of being dropped"
    );
}

#[test]
fn envelope_decoding_tolerates_extra_envelope_members() {
    let payload = r#"{
        "data": [{
            "id": "42",
            "text": "hello",
            "author_id": "7",
            "created_at": "2026-08-01T00:00:00.000Z",
            "some_future_member": {"deep": [1, 2, 3]}
        }],
        "includes": {"users": [{"id": "7", "username": "u"}]},
        "another_envelope_member": true
    }"#;
    let envelope: Envelope =
        serde_json::from_str(payload).expect("unknown members never fail decoding");
    assert_eq!(envelope.posts().len(), 1, "known structure still parses");
    assert_eq!(
        envelope.posts()[0].extension.get("some_future_member"),
        Some(&json!({"deep": [1, 2, 3]})),
        "injected unknowns land in the object extension"
    );
    assert_eq!(
        envelope.extension.get("another_envelope_member"),
        Some(&Value::Bool(true)),
        "envelope-level unknowns land in the envelope extension"
    );
}
