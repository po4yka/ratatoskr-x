//! Parser-version stamping: every emitted record carries the active constant,
//! and the stamp survives serialization round trips unchanged.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::path::PathBuf;

use x_normalize::dto::Envelope;
use x_normalize::normalize::normalize;
use x_normalize::{PARSER_VERSION, records};

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
fn every_record_carries_active_stamp() {
    for name in [
        "tweet_single.json",
        "thread.json",
        "quote_dangling_target.json",
        "retweet.json",
        "media_post.json",
        "article_backed_longform.json",
        "unknown_field.json",
    ] {
        let batch = normalize(&decode(name))
            .unwrap_or_else(|e| panic!("fixture {name} must normalize: {e}"));
        for user in batch.users() {
            assert_eq!(
                user.parser_version, PARSER_VERSION,
                "{name}: author carries the active stamp"
            );
        }
        for post in batch.posts() {
            assert_eq!(
                post.parser_version, PARSER_VERSION,
                "{name}: post carries the active stamp"
            );
        }
        for relation in batch.relations() {
            assert_eq!(
                relation.parser_version, PARSER_VERSION,
                "{name}: relation carries the active stamp"
            );
        }
        for media in batch.media() {
            assert_eq!(
                media.parser_version, PARSER_VERSION,
                "{name}: media carries the active stamp"
            );
        }
    }
}

#[test]
fn round_trip_preserves_the_stamp() {
    let thread = normalize(&decode("thread.json")).expect("the thread normalizes");
    let media_batch = normalize(&decode("media_post.json")).expect("media normalizes");

    let user = thread.users().first().expect("an author exists");
    let user_json = serde_json::to_string(user).expect("author serializes");
    let user_back: records::NormalizedUser =
        serde_json::from_str(&user_json).expect("author deserializes");
    assert_eq!(user_back.parser_version, PARSER_VERSION);
    assert_eq!(*user, user_back, "round trips are lossless");

    let relation = thread.relations().first().expect("a relation exists");
    let relation_json = serde_json::to_string(relation).expect("relation serializes");
    let relation_back: records::NormalizedRelation =
        serde_json::from_str(&relation_json).expect("relation deserializes");
    assert_eq!(relation_back.parser_version, PARSER_VERSION);

    let post = thread.posts().first().expect("a post exists");
    let post_json = serde_json::to_string(post).expect("post serializes");
    let post_back: records::NormalizedPost =
        serde_json::from_str(&post_json).expect("post deserializes");
    assert_eq!(post_back.parser_version, PARSER_VERSION);
    assert_eq!(*post, post_back, "post round trips are lossless");

    let media = media_batch.media().first().expect("a media record exists");
    let media_json = serde_json::to_string(media).expect("media serializes");
    let media_back: records::NormalizedMedia =
        serde_json::from_str(&media_json).expect("media deserializes");
    assert_eq!(media_back.parser_version, PARSER_VERSION);
}
