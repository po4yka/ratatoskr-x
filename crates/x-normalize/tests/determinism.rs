//! Determinism guard: every normalizable committed fixture produces
//! byte-identical output across repeated runs, with collections emerging in
//! design-mandated sort order regardless of input ordering.

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

/// Serializes the normalized batch of one fixture to a canonical string.
fn serialized(name: &str) -> String {
    let batch =
        normalize(&decode(name)).unwrap_or_else(|e| panic!("fixture {name} must normalize: {e}"));
    serde_json::to_string(&batch).expect("records serialize")
}

#[test]
fn repeated_normalization_is_identical_across_runs() {
    for name in [
        "tweet_single.json",
        "thread.json",
        "quote_dangling_target.json",
        "retweet.json",
        "media_post.json",
        "article_backed_longform.json",
        "unknown_field.json",
    ] {
        let first = serialized(name);
        let second = serialized(name);
        assert_eq!(first, second, "fixture {name} normalizes deterministically");
    }
}

#[test]
fn output_collections_emerge_sorted_by_provider_identity() {
    let batch = normalize(&decode("thread.json")).expect("the thread normalizes");

    let user_ids: Vec<&str> = batch
        .users()
        .iter()
        .map(|u| u.provider_id.as_str())
        .collect();
    let mut sorted_users = user_ids.clone();
    sorted_users.sort_unstable();
    assert_eq!(user_ids, sorted_users, "authors emerge provider-id sorted");

    let post_ids: Vec<&str> = batch
        .posts()
        .iter()
        .map(|p| p.provider_id.as_str())
        .collect();
    let mut sorted_posts = post_ids.clone();
    sorted_posts.sort_unstable();
    assert_eq!(post_ids, sorted_posts, "posts emerge provider-id sorted");

    let relation_keys: Vec<(String, String)> = batch
        .relations()
        .iter()
        .map(|r| {
            (
                r.post_provider_id.clone(),
                r.related_post_provider_id.clone(),
            )
        })
        .collect();
    let mut sorted_relations = relation_keys.clone();
    sorted_relations.sort();
    assert_eq!(
        relation_keys, sorted_relations,
        "relations emerge in D6 order"
    );
}
