//! The outbox relay publishes complete envelopes to `JetStream` (XR-021 CONTRACTS.md S02).

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_nats::jetstream;
use uuid::Uuid;
use x_capture::relay::{
    JetStreamPublisher, OutboxPublisher, OutboxRelay, PublishFailure, RelayError, subject_for,
};
use x_persistence::test_support::TestDatabase;

const ALLOWED: [&str; 5] = [
    "platform.operation.reported.v1",
    "social.source.captured.v1",
    "social.source.updated.v1",
    "social.source.removed.v1",
    "content.capture.requested.v1",
];

async fn insert_row(test: &TestDatabase, event_type: &str, ordinal: u32) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into x_archive.outbox_events \
           (id, aggregate, event_type, payload, created_at) \
         values ($1, $2, $3, $4::jsonb, \
                 '2026-08-27T12:00:00Z'::timestamptz + make_interval(secs => $5::int))",
    )
    .bind(id)
    .bind(format!("operation:{id}"))
    .bind(event_type)
    .bind(serde_json::json!({"ordinal": ordinal, "event_type": event_type}).to_string())
    .bind(i32::try_from(ordinal).expect("a small ordinal"))
    .execute(test.database.pool())
    .await
    .expect("the outbox row seeds");
    id
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    published: bool,
    attempts: i32,
    last_error: Option<String>,
    retry_in_future: bool,
}

async fn row(test: &TestDatabase, id: Uuid) -> Row {
    let (published, attempts, last_error, retry_in_future): (bool, i32, Option<String>, bool) =
        sqlx::query_as(
            "select published_at is not null, attempt_count, last_error, next_attempt_at > now() \
             from x_archive.outbox_events where id = $1",
        )
        .bind(id)
        .fetch_one(test.database.pool())
        .await
        .expect("the row is readable");
    Row {
        published,
        attempts,
        last_error,
        retry_in_future,
    }
}

/// Records every publish, fails the scripted ids, and notes whether the row was still unmarked
/// at publish time (a row is marked only after the acknowledgement).
struct ScriptedPublisher {
    database: x_persistence::database::Database,
    failing: Mutex<HashSet<Uuid>>,
    calls: Mutex<Vec<(String, String, bool)>>,
}

impl OutboxPublisher for ScriptedPublisher {
    fn publish<'a>(
        &'a self,
        subject: &'a str,
        message_id: &'a str,
        _payload: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<(), PublishFailure>> + Send + 'a>> {
        Box::pin(async move {
            let id = Uuid::parse_str(message_id).expect("the message id is the row id");
            let unmarked: bool = sqlx::query_scalar(
                "select published_at is null from x_archive.outbox_events where id = $1",
            )
            .bind(id)
            .fetch_one(self.database.pool())
            .await
            .expect("the row is readable");
            self.calls.lock().expect("not poisoned").push((
                subject.to_owned(),
                message_id.to_owned(),
                unmarked,
            ));
            if self.failing.lock().expect("not poisoned").contains(&id) {
                return Err(PublishFailure {
                    class: "ack_timeout",
                });
            }
            Ok(())
        })
    }
}

#[test]
fn only_the_five_x_event_types_map_to_a_subject() {
    for event_type in ALLOWED {
        let expected = if event_type == "content.capture.requested.v1" {
            format!("cmd.{event_type}")
        } else {
            format!("evt.{event_type}")
        };
        assert_eq!(subject_for(event_type), Some(expected));
    }
    for other in [
        "bogus.v1",
        "content.capture.requested",
        "social.source.removed.v2",
        "evt.social.source.captured.v1",
        "",
    ] {
        assert_eq!(subject_for(other), None, "{other:?}");
    }
}

#[tokio::test]
async fn rows_publish_in_creation_order_to_their_subjects_with_the_row_id_as_message_id() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let client = async_nats::connect(nats_url()).await.expect("test NATS");
    let context = jetstream::new(client);
    let events = context
        .get_or_create_stream(jetstream::stream::Config {
            name: "ratatoskr_events".to_owned(),
            subjects: vec!["evt.>".to_owned()],
            ..jetstream::stream::Config::default()
        })
        .await
        .expect("the events stream");
    let commands = context
        .get_or_create_stream(jetstream::stream::Config {
            name: "ratatoskr_commands".to_owned(),
            subjects: vec!["cmd.>".to_owned()],
            ..jetstream::stream::Config::default()
        })
        .await
        .expect("the commands stream");
    let mut ids = Vec::new();
    for (ordinal, event_type) in (0_u32..).zip(ALLOWED) {
        ids.push((insert_row(&test, event_type, ordinal).await, event_type));
    }
    let relay = OutboxRelay::new(
        test.database.clone(),
        Arc::new(JetStreamPublisher::new(context.clone())),
    );

    let report = relay.run_once().await.expect("the pass completes");

    assert_eq!((report.published, report.failed), (5, 0));
    let mut sequences = Vec::new();
    for (id, event_type) in &ids {
        let (stream, subject) = if *event_type == "content.capture.requested.v1" {
            (&commands, format!("cmd.{event_type}"))
        } else {
            (&events, format!("evt.{event_type}"))
        };
        let found = find_by_message_id(stream, &id.to_string())
            .await
            .unwrap_or_else(|| panic!("{event_type} reached the broker"));
        assert_eq!(found.1, subject);
        let payload: serde_json::Value =
            serde_json::from_slice(&found.2).expect("the stored document is published");
        assert_eq!(payload["event_type"], *event_type);
        assert!(row(&test, *id).await.published, "marked after the ack");
        sequences.push((stream.cached_info().config.name.clone(), found.0));
    }
    let mut per_stream: HashMap<String, Vec<u64>> = HashMap::new();
    for (stream, sequence) in sequences {
        per_stream.entry(stream).or_default().push(sequence);
    }
    for (stream, sequence) in &per_stream {
        assert!(
            sequence
                .windows(2)
                .all(|pair| pair.first().expect("the element exists")
                    < pair.get(1).expect("the element exists")),
            "{stream}: rows reach the broker in creation order"
        );
    }
    let again = relay.run_once().await.expect("the second pass completes");
    assert_eq!(
        again.published, 0,
        "a published row is never published again"
    );
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn a_failed_publish_marks_nothing_stops_the_pass_and_a_later_pass_skips_the_head() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let head = insert_row(&test, "social.source.captured.v1", 0).await;
    let second = insert_row(&test, "social.source.updated.v1", 1).await;
    let third = insert_row(&test, "platform.operation.reported.v1", 2).await;
    let publisher = Arc::new(ScriptedPublisher {
        database: test.database.clone(),
        failing: Mutex::new(HashSet::from([head])),
        calls: Mutex::new(Vec::new()),
    });
    let relay = OutboxRelay::new(test.database.clone(), publisher.clone());

    let first = relay.run_once().await.expect("the pass completes");
    assert_eq!((first.published, first.failed), (0, 1));
    assert_eq!(
        row(&test, head).await,
        Row {
            published: false,
            attempts: 1,
            last_error: Some("ack_timeout".to_owned()),
            retry_in_future: true,
        },
        "the head records its attempt and backs off"
    );
    for later in [second, third] {
        assert_eq!(
            row(&test, later).await,
            Row {
                published: false,
                attempts: 0,
                last_error: None,
                retry_in_future: false,
            },
            "the pass stopped before touching later rows"
        );
    }

    let next = relay.run_once().await.expect("the next pass completes");
    assert_eq!(
        (next.published, next.failed),
        (2, 0),
        "a failing head row does not starve later rows"
    );
    assert!(!row(&test, head).await.published);
    assert!(row(&test, second).await.published);
    assert!(row(&test, third).await.published);
    let calls = publisher.calls.lock().expect("not poisoned").clone();
    assert!(
        calls.iter().all(|(_, _, unmarked)| *unmarked),
        "a row is still unmarked when it is published: {calls:?}"
    );
    assert_eq!(
        calls
            .iter()
            .map(|(subject, _, _)| subject.as_str())
            .collect::<Vec<_>>(),
        [
            "evt.social.source.captured.v1",
            "evt.social.source.updated.v1",
            "evt.platform.operation.reported.v1",
        ],
        "the failed head was attempted first, then the later rows in order"
    );
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn an_unmappable_event_type_stops_the_relay_instead_of_being_skipped() {
    let test = TestDatabase::create().await.expect("a disposable database");
    sqlx::raw_sql(
        "alter table x_archive.outbox_events drop constraint outbox_events_event_type_allowed",
    )
    .execute(test.database.pool())
    .await
    .expect("the test lifts the schema guard to simulate a programming error");
    let bogus = insert_row(&test, "bogus.thing.happened.v1", 0).await;
    let later = insert_row(&test, "social.source.captured.v1", 1).await;
    let publisher = Arc::new(ScriptedPublisher {
        database: test.database.clone(),
        failing: Mutex::new(HashSet::new()),
        calls: Mutex::new(Vec::new()),
    });
    let relay = OutboxRelay::new(test.database.clone(), publisher.clone());

    let error = relay
        .run_once()
        .await
        .expect_err("an unmappable row is a hard error");

    assert!(
        matches!(&error, RelayError::UnmappableEventType(event_type) if event_type == "bogus.thing.happened.v1"),
        "{error:?}"
    );
    assert!(publisher.calls.lock().expect("not poisoned").is_empty());
    assert!(!row(&test, bogus).await.published);
    assert!(
        !row(&test, later).await.published,
        "nothing publishes past it"
    );
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn the_schema_refuses_an_event_type_outside_the_closed_list() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let refused = sqlx::query(
        "insert into x_archive.outbox_events (id, aggregate, event_type, payload) \
         values ($1, 'operation:x', 'bogus.thing.happened.v1', '{}'::jsonb)",
    )
    .bind(Uuid::now_v7())
    .execute(test.database.pool())
    .await;
    assert!(refused.is_err(), "the closed list is a CHECK constraint");
    test.cleanup().await.expect("cleanup drops the database");
}

/// Finds the stored message carrying `Nats-Msg-Id: message_id`, newest first.
async fn find_by_message_id(
    stream: &jetstream::stream::Stream,
    message_id: &str,
) -> Option<(u64, String, Vec<u8>)> {
    let mut stream = stream.clone();
    let last = stream
        .info()
        .await
        .expect("stream info")
        .state
        .last_sequence;
    for sequence in (last.saturating_sub(2_000).max(1)..=last).rev() {
        let Ok(message) = stream.get_raw_message(sequence).await else {
            continue;
        };
        let matches = message
            .headers
            .get(async_nats::header::NATS_MESSAGE_ID)
            .is_some_and(|value| value.as_str() == message_id);
        if matches {
            return Some((
                message.sequence,
                message.subject.to_string(),
                message.payload.to_vec(),
            ));
        }
    }
    None
}

#[expect(
    clippy::disallowed_methods,
    reason = "the integration binary chooses its isolated JetStream endpoint"
)]
fn nats_url() -> String {
    std::env::var("X_TEST_NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:14224".to_owned())
}
