//! Post and author normalization against the committed single-tweet fixture.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use x_normalize::dto::Envelope;

/// Resolves a fixture file below the repository-root `fixtures/x-api` folder.
fn decode(name: &str) -> Envelope {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/x-api")
        .join(name);
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("fixture {name} must be readable: {e}"));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("fixture {name} must decode: {e}"))
}

#[test]
fn single_tweet_yields_author_and_post_with_content_and_linkage() {
    let envelope = decode("tweet_single.json");
    let batch = x_normalize::normalize::normalize(&envelope).expect("normalization succeeds");

    assert_eq!(batch.users().len(), 1, "exactly one author emerges");
    let user = &batch.users()[0];
    assert_eq!(user.provider_id, "9001");
    assert_eq!(user.username.as_deref(), Some("fixture_author"));
    assert_eq!(user.display_name.as_deref(), Some("Fixture Author"));

    assert_eq!(batch.posts().len(), 1, "exactly one post emerges");
    let post = &batch.posts()[0];
    assert_eq!(post.provider_id, "100200300");
    assert_eq!(post.author_provider_id, "9001");
    assert_eq!(
        post.text, "Fixture post about normalization pipelines.",
        "canonical text is preserved unchanged"
    );
    assert_eq!(post.language.as_deref(), Some("en"));
    assert_eq!(
        post.published_at,
        Some(
            Utc.with_ymd_and_hms(2026, 8, 1, 12, 0, 0)
                .single()
                .expect("the fixture timestamp resolves to one instant")
        ),
        "publication time parses strictly from the payload"
    );
    assert_eq!(
        post.conversation_provider_id.as_deref(),
        Some("100200300"),
        "conversation linkage copies verbatim"
    );
    assert_eq!(post.retweet_count, Some(3));
    assert_eq!(post.reply_count, Some(1));
    assert_eq!(post.like_count, Some(12));
    assert_eq!(post.quote_count, Some(0));
    assert_eq!(post.bookmark_count, Some(4));
    assert_eq!(post.impression_count, Some(500));
}
