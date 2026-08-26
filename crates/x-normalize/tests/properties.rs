//! Property-based confidence over payload shape variation: generated
//! envelopes either normalize successfully or fail with a typed error — never
//! panic — normalization is deterministic under variation, and injected
//! unknown keys always survive into preserved material.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use serde_json::{Value, json};
use x_normalize::dto::Envelope;
use x_normalize::normalize::{UNRESOLVED_REFERENCES_KEY, normalize};

/// Member names this parser generation does not model; safe injection targets
/// because they collide with nothing documented.
const UNKNOWN_KEYS: &[&str; 5] = &[
    "zz_future_alpha",
    "zz_future_beta",
    "zz_future_gamma",
    "zz_future_delta",
    "zz_future_epsilon",
];

const POST_IDS: &[&str; 4] = &["p1", "p2", "p3", "p4"];
const AUTHOR_IDS: &[&str; 3] = &["a1", "a2", "a3"];
const MEDIA_KEYS: &[&str; 3] = &["m1", "m2", "m3"];
const TEXTS: &[&str; 4] = &[
    "plain fixture text",
    "unicode: naïve café 漢字 🦀",
    "",
    "line\nbreak\ttab",
];
const TIMESTAMPS: &[&str; 3] = &[
    "2026-08-01T12:00:00.000Z",
    "2025-01-01T00:00:01.500Z",
    "2024-12-31T23:59:59Z",
];
const LANGS: &[&str; 3] = &["en", "fr", "und"];
const REFERENCE_TYPES: &[&str; 4] = &["replied_to", "quoted", "retweeted", "vibed_with"];

/// Members injected into generated objects, retained by the generator so the
/// survival property knows what to look for.
type InjectedMembers = Vec<(String, Value)>;

/// One generated post object paired with its injected unknown members.
type GeneratedPost = (Value, InjectedMembers);

fn scalar() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::from),
        "[a-z]{0,6}".prop_map(Value::from),
    ]
}

fn unknown_member() -> impl Strategy<Value = (String, Value)> {
    (prop::sample::select(UNKNOWN_KEYS), scalar()).prop_map(|(key, value)| (key.to_owned(), value))
}

fn public_metrics() -> impl Strategy<Value = Value> {
    (
        prop::option::of(any::<i64>()),
        prop::option::of(any::<i64>()),
        prop::option::of(unknown_member()),
    )
        .prop_map(|(likes, impressions, extra)| {
            let mut object = serde_json::Map::new();
            if let Some(likes) = likes {
                object.insert("like_count".to_owned(), Value::from(likes));
            }
            if let Some(impressions) = impressions {
                object.insert("impression_count".to_owned(), Value::from(impressions));
            }
            if let Some((key, value)) = extra {
                object.insert(key, value);
            }
            Value::Object(object)
        })
}

fn post() -> impl Strategy<Value = GeneratedPost> {
    (
        prop::sample::select(POST_IDS),
        prop::sample::select(TEXTS),
        prop::sample::select(AUTHOR_IDS),
        prop::option::of(prop::sample::select(TIMESTAMPS)),
        prop::option::of(prop::sample::select(LANGS)),
        prop::option::of(public_metrics()),
        prop::option::of(prop::collection::vec(
            (
                prop::sample::select(REFERENCE_TYPES),
                prop::sample::select(POST_IDS),
            )
                .prop_map(|(kind, target)| json!({"type": kind, "id": target})),
            0..3,
        )),
        prop::option::of(prop::sample::select(MEDIA_KEYS)),
        prop::collection::vec(unknown_member(), 0..3),
    )
        .prop_map(
            |(id, text, author, created_at, lang, metrics, references, media_key, unknowns)| {
                let mut object = serde_json::Map::new();
                object.insert("id".to_owned(), json!(id));
                object.insert("text".to_owned(), json!(text));
                object.insert("author_id".to_owned(), json!(author));
                if let Some(created_at) = created_at {
                    object.insert("created_at".to_owned(), json!(created_at));
                }
                if let Some(lang) = lang {
                    object.insert("lang".to_owned(), json!(lang));
                }
                if let Some(metrics) = metrics {
                    object.insert("public_metrics".to_owned(), metrics);
                }
                if let Some(references) = references {
                    object.insert("referenced_tweets".to_owned(), Value::Array(references));
                }
                if let Some(media_key) = media_key {
                    object.insert("attachments".to_owned(), json!({"media_keys": [media_key]}));
                }
                let mut injected = BTreeMap::new();
                for (key, value) in unknowns {
                    // JSON objects contain one value per member name. Retain
                    // the final value emitted into the generated payload.
                    injected.insert(key.clone(), value.clone());
                    object.insert(key, value);
                }
                (Value::Object(object), injected.into_iter().collect())
            },
        )
}

fn user() -> impl Strategy<Value = Value> {
    (prop::sample::select(AUTHOR_IDS), prop::option::of(scalar())).prop_map(|(id, extra)| {
        let mut object = serde_json::Map::new();
        object.insert("id".to_owned(), json!(id));
        object.insert("username".to_owned(), json!("user_handle"));
        if let Some(text) = extra.as_ref().and_then(Value::as_str) {
            object.insert("zz_future_place".to_owned(), Value::String(text.to_owned()));
        }
        Value::Object(object)
    })
}

fn media() -> impl Strategy<Value = Value> {
    (
        prop::sample::select(MEDIA_KEYS),
        prop::option::of(any::<i64>()),
        prop::option::of(any::<i64>()),
        prop::option::of(unknown_member()),
    )
        .prop_map(|(key, width, height, extra)| {
            let mut object = serde_json::Map::new();
            object.insert("media_key".to_owned(), json!(key));
            object.insert("type".to_owned(), json!("photo"));
            if let Some(width) = width {
                object.insert("width".to_owned(), Value::from(width));
            }
            if let Some(height) = height {
                object.insert("height".to_owned(), Value::from(height));
            }
            if let Some((injected_key, injected_value)) = extra {
                object.insert(injected_key, injected_value);
            }
            Value::Object(object)
        })
}

/// Generates whole envelopes plus the per-post injected members.
fn envelope_strategy() -> impl Strategy<Value = (Value, Vec<InjectedMembers>)> {
    (0usize..=3, 0usize..=2)
        .prop_flat_map(|(post_count, media_count)| {
            let posts = prop::collection::vec(post(), post_count..=(post_count.max(1)));
            let users = prop::collection::vec(user(), 0..3);
            let media = prop::collection::vec(media(), 0..media_count.max(1));
            (posts, users, media)
        })
        .prop_map(|(posts, users, media)| {
            // Provider post identity is unique within one API response. Keep
            // the generator faithful to that contract so the property checks
            // preservation rather than accidentally comparing two different
            // payloads that share one provider identity.
            let mut seen_post_ids = BTreeSet::new();
            let mut posts: Vec<GeneratedPost> = posts
                .into_iter()
                .filter(|(payload, _)| {
                    payload
                        .get("id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| seen_post_ids.insert(id.to_owned()))
                })
                .collect();
            // `normalize` orders records by provider identity. Align the
            // paired property expectation with that documented output order.
            posts.sort_by(|(left, _), (right, _)| {
                left.get("id")
                    .and_then(Value::as_str)
                    .cmp(&right.get("id").and_then(Value::as_str))
            });
            let expected_unknowns: Vec<InjectedMembers> =
                posts.iter().map(|(_, injected)| injected.clone()).collect();
            let payload_posts: Vec<Value> = posts.into_iter().map(|(payload, _)| payload).collect();
            let envelope = json!({
                "data": payload_posts,
                "includes": {"users": users, "media": media},
            });
            (envelope, expected_unknowns)
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn shape_variations_never_panic_and_fail_only_typed(
        (envelope, _unknowns) in envelope_strategy()
    ) {
        let decoded: Envelope = match serde_json::from_value(envelope) {
            Ok(decoded) => decoded,
            Err(error) => panic!("tolerant decoding accepts every generated shape: {error}"),
        };
        // Both outcomes are acceptable; panics are not, and the test harness
        // turns them into failures.
        let _ = normalize(&decoded);
    }

    #[test]
    fn varied_shapes_normalize_deterministically(
        (envelope, _unknowns) in envelope_strategy()
    ) {
        let decoded: Envelope = match serde_json::from_value(envelope) {
            Ok(decoded) => decoded,
            Err(error) => panic!("tolerant decoding accepts every generated shape: {error}"),
        };
        let first = normalize(&decoded);
        let second = normalize(&decoded);
        match (first, second) {
            (Ok(batch_a), Ok(batch_b)) => prop_assert_eq!(
                serde_json::to_string(&batch_a).expect("serializes"),
                serde_json::to_string(&batch_b).expect("serializes")
            ),
            (Err(a), Err(b)) => prop_assert_eq!(format!("{a}"), format!("{b}")),
            _ => panic!("the same input must not flip between success and refusal"),
        }
    }

    #[test]
    fn injected_unknown_keys_always_survive(
        (envelope, all_expected) in envelope_strategy()
    ) {
        let decoded: Envelope = match serde_json::from_value(envelope) {
            Ok(decoded) => decoded,
            Err(error) => panic!("tolerant decoding accepts every generated shape: {error}"),
        };
        if let Ok(batch) = normalize(&decoded) {
            prop_assert_eq!(
                batch.posts().len(),
                all_expected.len(),
                "every generated post either refuses the batch or normalizes"
            );
            for (post, expected) in batch.posts().iter().zip(&all_expected) {
                for (key, value) in expected {
                    let stored = post.extension.get(key);
                    prop_assert!(
                        stored == Some(value),
                        "injected member must survive on post {}: {key}",
                        post.provider_id
                    );
                }
                // Reserved-key material only ever appears alongside unmapped
                // reference types, which this generator produces through
                // `vibed_with`; when present it must carry an array.
                let preserved = post.extension.get(UNRESOLVED_REFERENCES_KEY);
                if preserved.is_some() {
                    prop_assert!(
                        preserved.is_some_and(Value::is_array),
                        "unresolved references are preserved as an array"
                    );
                }
            }
        }
    }
}
