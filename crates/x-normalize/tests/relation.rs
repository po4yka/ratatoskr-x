//! Relation fidelity against committed fixtures: dangling quote targets still
//! record, retweets map to repost, and reference types outside the closed
//! vocabulary ride the reserved preservation key instead of vanishing.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::path::PathBuf;

use x_normalize::dto::Envelope;
use x_normalize::normalize::{UNRESOLVED_REFERENCES_KEY, normalize};
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
fn dangling_quote_target_still_records_relation() {
    let envelope = decode("quote_dangling_target.json");
    let batch = normalize(&envelope).expect("the quoting envelope normalizes");

    assert_eq!(batch.posts().len(), 1);
    assert_eq!(batch.relations().len(), 1, "dangling targets survive");
    let relation = &batch.relations()[0];
    assert_eq!(relation.post_provider_id, "400500600");
    assert_eq!(relation.related_post_provider_id, "999888777");
    assert_eq!(relation.relation, Relation::Quote);
}

#[test]
fn retweet_reference_maps_to_repost() {
    let envelope = decode("retweet.json");
    let batch = normalize(&envelope).expect("the retweet envelope normalizes");

    assert_eq!(batch.relations().len(), 1);
    let relation = &batch.relations()[0];
    assert_eq!(relation.post_provider_id, "500600700");
    assert_eq!(relation.related_post_provider_id, "555666777");
    assert_eq!(
        relation.relation,
        Relation::Repost,
        "retweeted references map onto the schema vocabulary"
    );
}

#[test]
fn unrecognized_reference_type_is_preserved_without_a_relation() {
    let payload = r#"{
        "data": [{
            "id": "10",
            "text": "referencing through a type this generation does not know",
            "author_id": "20",
            "created_at": "2026-08-10T00:00:00.000Z",
            "referenced_tweets": [
                {"type": "replied_to", "id": "11"},
                {"type": "hovers_near", "id": "12", "strength": 0.5}
            ]
        }],
        "includes": {"users": [{"id": "20", "username": "u"}]}
    }"#;
    let envelope: Envelope = serde_json::from_str(payload).expect("decodes tolerantly");
    let batch = normalize(&envelope).expect("normalizes");

    let known: Vec<_> = batch
        .relations()
        .iter()
        .map(|r| (r.post_provider_id.as_str(), r.relation))
        .collect();
    assert_eq!(
        known,
        vec![("10", Relation::Reply)],
        "only the mapped reference emits a row"
    );

    let preserved = batch.posts()[0]
        .extension
        .get(UNRESOLVED_REFERENCES_KEY)
        .expect("unmapped references land under the reserved key");
    let entry = preserved
        .get(0)
        .and_then(|v| v.get("type"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    assert_eq!(
        entry,
        serde_json::json!("hovers_near"),
        "the unknown type survives name and value intact"
    );
}
