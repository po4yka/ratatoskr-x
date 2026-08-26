//! Long-form and article-backed normalization against the committed fixture:
//! note content lands beside — never over — canonical text, edit history is
//! preserved, and the article wrapper rides record-and-preserve.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::path::PathBuf;

use x_normalize::dto::Envelope;
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
fn longform_note_lands_in_long_text_and_edit_time_stays_unset() {
    let envelope = decode("article_backed_longform.json");
    let batch = normalize(&envelope).expect("the long-form envelope normalizes");

    assert_eq!(batch.posts().len(), 1);
    let post = &batch.posts()[0];
    assert_eq!(
        post.text, "Fixture article-backed post with a truncated teaser link.",
        "canonical short text is never overwritten"
    );
    let long_text = post
        .long_text
        .as_deref()
        .expect("note content reaches the long-text field");
    assert!(
        long_text.starts_with("The full long-form body"),
        "the note body travels intact"
    );

    assert_eq!(
        post.edited_at, None,
        "no exact edit timestamp exists in the payload, so none is invented"
    );
    assert!(
        post.extension.get("article").is_some(),
        "the documented-but-unmodeled article wrapper rides preservation"
    );
    let title = post
        .extension
        .get("article")
        .and_then(|a| a.get("title"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    assert_eq!(
        title,
        serde_json::json!("Fixture Article Title"),
        "article wrapper members survive name and value intact"
    );
}
