//! Thread normalization against the committed thread fixture: replies link to
//! their parents, the conversation value is shared, and the root carries no
//! reply relation.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::path::PathBuf;

use x_normalize::dto::Envelope;
use x_normalize::normalize::normalize;
use x_normalize::records::Relation;

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
fn thread_replies_carry_reply_relations_and_shared_conversation() {
    let envelope = decode("thread.json");
    let batch = normalize(&envelope).expect("the thread normalizes");

    assert_eq!(batch.posts().len(), 3, "root plus two replies");
    assert_eq!(
        batch.relations().len(),
        2,
        "each reply contributes one relation"
    );

    let reply_one = batch
        .relations()
        .iter()
        .find(|r| r.post_provider_id == "300400501")
        .expect("reply one emits its relation");
    assert_eq!(reply_one.related_post_provider_id, "300400500");
    assert_eq!(reply_one.relation, Relation::Reply);

    let reply_two = batch
        .relations()
        .iter()
        .find(|r| r.post_provider_id == "300400502")
        .expect("reply two emits its relation");
    assert_eq!(reply_two.related_post_provider_id, "300400501");
    assert_eq!(reply_two.relation, Relation::Reply);

    let root_has_no_relation = !batch
        .relations()
        .iter()
        .any(|r| r.post_provider_id == "300400500");
    assert!(
        root_has_no_relation,
        "the thread root carries no reply relation"
    );

    let conversations: Vec<Option<&str>> = batch
        .posts()
        .iter()
        .map(|p| p.conversation_provider_id.as_deref())
        .collect();
    assert!(
        conversations.iter().all(|c| *c == Some("300400500")),
        "every thread member shares the root's conversation linkage"
    );
}
