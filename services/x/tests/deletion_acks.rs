//! A removal X published is kept and re-published until Knowledge acknowledges it (XR-021
//! CONTRACTS.md R5-11 rule 4): the relay sends the same bytes again under the same `Nats-Msg-Id`,
//! the consumer of the Edge-provisioned durable `ratatoskr_x_deletion_acks` ends the resend, and a
//! message that cannot be attributed to Knowledge is terminated.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use async_nats::jetstream;
use ratatoskr_x::bus::connect_bus;
use ratatoskr_x::deletion_acks::{
    AcksSummary, FILTER_SUBJECT, consume_deletion_acks, ensure_acks_consumer,
};
use support::{
    ACK_SUBJECT, Answer, Enforcing, KNOWLEDGE_INBOX_PREFIX, Keys, OTHER_OWNER, OWNER, REMOVAL,
    removal_envelope,
};
use tokio::task::JoinHandle;
use uuid::Uuid;
use x_capture::relay::{JetStreamPublisher, OutboxRelay};
use x_core::config::{BusConfig, LogFormat};
use x_persistence::database::Database;
use x_persistence::outbox::ResendPolicy;
use x_persistence::test_support::TestDatabase;

mod support;

const EVENTS_STREAM: &str = "ratatoskr_events";

/// The tests that share the anonymous broker's `evt.knowledge.deletion.acknowledged.v1` subject run
/// one at a time, so one test's acknowledgements are never another test's ignored messages.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[expect(
    clippy::disallowed_methods,
    reason = "the integration binary chooses its isolated JetStream endpoint"
)]
fn nats_url() -> String {
    std::env::var("X_TEST_NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:14224".to_owned())
}

async fn insert_removal(database: &Database) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into x_archive.outbox_events (id, aggregate, event_type, payload) \
         values ($1, $2, $3, $4::jsonb)",
    )
    .bind(id)
    .bind(format!("social_source:{id}"))
    .bind(REMOVAL)
    .bind(removal_envelope(id))
    .execute(database.pool())
    .await
    .expect("the removal row seeds");
    id
}

/// A running consumer whose summary is read when it stops.
struct Running {
    stop: tokio::sync::watch::Sender<bool>,
    handle: JoinHandle<Result<AcksSummary, ratatoskr_x::deletion_acks::AcksError>>,
}

impl Running {
    fn start(context: &jetstream::Context, database: &Database, durable: &str) -> Self {
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let (context, database, durable) = (context.clone(), database.clone(), durable.to_owned());
        let handle = tokio::spawn(async move {
            consume_deletion_acks(&context, &database, EVENTS_STREAM, &durable, async move {
                let _ = stopped.wait_for(|requested| *requested).await;
            })
            .await
        });
        Self { stop, handle }
    }

    async fn finish(self) -> AcksSummary {
        let _ = self.stop.send(true);
        self.handle
            .await
            .expect("the consumer task does not panic")
            .expect("the consumer stops cleanly")
    }
}

/// Waits until the durable has acknowledged every message up to stream sequence `sequence`.
async fn wait_until_consumed(
    context: &jetstream::Context,
    durable: &str,
    sequence: u64,
) -> jetstream::consumer::Info {
    let consumer: jetstream::consumer::Consumer<jetstream::consumer::pull::Config> = context
        .get_consumer_from_stream(durable, EVENTS_STREAM)
        .await
        .expect("the durable exists");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let mut handle = consumer.clone();
        let info = handle.info().await.expect("the durable has info").clone();
        if info.ack_floor.stream_sequence >= sequence && info.num_ack_pending == 0 {
            return info;
        }
        assert!(
            Instant::now() < deadline,
            "the consumer never reached stream sequence {sequence}: {info:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn row_state(database: &Database, id: Uuid) -> (bool, Option<String>, i32) {
    sqlx::query_as(
        "select acknowledged_at is not null, ack_outcome, resend_count \
           from x_archive.outbox_events where id = $1",
    )
    .bind(id)
    .fetch_one(database.pool())
    .await
    .expect("the row is readable")
}

async fn wait_for_acknowledgement(database: &Database, id: Uuid) -> (bool, Option<String>, i32) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let state = row_state(database, id).await;
        if state.0 {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "the removal was never acknowledged"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Edge's side on the anonymous broker: the stream and one fresh durable of the right shape.
struct Fixture {
    test: TestDatabase,
    context: jetstream::Context,
    durable: String,
}

async fn fixture() -> Fixture {
    let test = TestDatabase::create().await.expect("a disposable database");
    let context = jetstream::new(
        async_nats::connect(nats_url())
            .await
            .expect("the test broker connects"),
    );
    let events = context
        .get_or_create_stream(jetstream::stream::Config {
            name: EVENTS_STREAM.to_owned(),
            subjects: vec!["evt.>".to_owned()],
            ..jetstream::stream::Config::default()
        })
        .await
        .expect("the events stream exists");
    let durable = format!("x_deletion_acks_{}", Uuid::now_v7().simple());
    events
        .create_consumer(durable_config(
            &durable,
            FILTER_SUBJECT,
            Duration::from_secs(30),
        ))
        .await
        .expect("Edge provisions the durable");
    Fixture {
        test,
        context,
        durable,
    }
}

fn durable_config(
    durable: &str,
    filter: &str,
    ack_wait: Duration,
) -> jetstream::consumer::pull::Config {
    jetstream::consumer::pull::Config {
        durable_name: Some(durable.to_owned()),
        filter_subject: filter.to_owned(),
        ack_policy: jetstream::consumer::AckPolicy::Explicit,
        ack_wait,
        deliver_policy: jetstream::consumer::DeliverPolicy::New,
        ..jetstream::consumer::pull::Config::default()
    }
}

async fn publish(context: &jetstream::Context, subject: &str, body: String) -> u64 {
    context
        .publish(subject.to_owned(), body.into())
        .await
        .expect("the message publishes")
        .await
        .expect("the broker stores the message")
        .sequence
}

/// Captures the log lines of the process-global subscriber the refusal test installs.
#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("not poisoned")
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl LogBuffer {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("not poisoned")).into_owned()
    }
}

fn install_telemetry() -> (LogBuffer, x_telemetry::TelemetryGuard) {
    let buffer = LogBuffer::default();
    let writer = buffer.clone();
    let guard = x_telemetry::build_with_writer(LogFormat::Json, "info", move || writer.clone())
        .expect("telemetry builds")
        .install()
        .expect("this test binary installs telemetry once");
    (buffer, guard)
}

#[tokio::test]
async fn an_unacknowledged_removal_is_published_again_until_knowledge_acknowledges_it() {
    let keys = Keys::new();
    let broker = Enforcing::start(&keys);
    let admin = async_nats::ConnectOptions::with_nkey(keys.admin.seed().expect("a seed"))
        .connect(broker.url())
        .await
        .expect("Edge connects");
    let edge = jetstream::new(admin);
    // A duplicate window of 200 ms lets the test see the second copy in the stream; the production
    // window is 120 s, which is why the production resend delay defaults to 600 s.
    let events = edge
        .get_or_create_stream(jetstream::stream::Config {
            name: EVENTS_STREAM.to_owned(),
            subjects: vec!["evt.>".to_owned()],
            duplicate_window: Duration::from_millis(200),
            ..jetstream::stream::Config::default()
        })
        .await
        .expect("the events stream exists");
    events
        .create_consumer(durable_config(
            ratatoskr_x::deletion_acks::DURABLE,
            FILTER_SUBJECT,
            Duration::from_secs(30),
        ))
        .await
        .expect("Edge provisions the durable");
    let seed_path = broker.write_seed("x.nkey", &keys.x);
    let client = connect_bus(&BusConfig {
        url: broker.url(),
        nkey_seed_path: seed_path.display().to_string(),
        stream_name: "ratatoskr_commands".to_owned(),
        consumer_name: "ratatoskr_x_browser_capture".to_owned(),
        events_stream_name: EVENTS_STREAM.to_owned(),
    })
    .await
    .expect("the service connects under the X stanza");
    let context = jetstream::new(client);
    let test = TestDatabase::create().await.expect("a disposable database");
    let id = insert_removal(&test.database).await;
    let relay = OutboxRelay::new(
        test.database.clone(),
        Arc::new(JetStreamPublisher::new(context.clone())),
        ResendPolicy::new(Duration::from_millis(400), Duration::from_secs(4)),
    );
    let consumer = Running::start(
        &context,
        &test.database,
        ratatoskr_x::deletion_acks::DURABLE,
    );

    eprintln!("transcript: relay pass 1, the removal {id} is published");
    let first = relay.run_once().await.expect("the first pass");
    assert_eq!((first.published, first.resent), (1, 0));
    assert_eq!(
        row_state(&test.database, id).await,
        (false, None, 0),
        "the first publish is not an acknowledgement"
    );

    eprintln!("transcript: no acknowledgement; waiting past resend_after and the duplicate window");
    tokio::time::sleep(Duration::from_millis(600)).await;
    let again = relay.run_once().await.expect("the resend pass");
    assert_eq!((again.published, again.resent), (0, 1));
    eprintln!("transcript: relay pass 2 published the stored bytes again");

    let mut stream = edge
        .get_stream(EVENTS_STREAM)
        .await
        .expect("the events stream is readable");
    let info = stream.info().await.expect("the stream has info");
    assert_eq!(info.state.messages, 2, "the stream holds both copies");
    let mut copies = Vec::new();
    for sequence in 1..=2 {
        let raw = stream
            .get_raw_message(sequence)
            .await
            .expect("the stored copy is readable");
        let message_id = raw
            .headers
            .get(async_nats::header::NATS_MESSAGE_ID)
            .expect("the copy carries a Nats-Msg-Id")
            .to_string();
        copies.push((message_id, raw.payload.to_vec()));
    }
    assert_eq!(copies[0].0, id.to_string(), "the message id is the row id");
    assert_eq!(copies[0], copies[1], "same Nats-Msg-Id, same bytes");
    for (sequence, (message_id, _)) in (1..).zip(&copies) {
        eprintln!("transcript: stream sequence {sequence} Nats-Msg-Id {message_id}");
    }

    let knowledge = async_nats::ConnectOptions::with_nkey(keys.knowledge.seed().expect("a seed"))
        .custom_inbox_prefix(KNOWLEDGE_INBOX_PREFIX)
        .connect(broker.url())
        .await
        .expect("Knowledge connects");
    let sequence = publish(
        &jetstream::new(knowledge),
        ACK_SUBJECT,
        Answer::applied(id).document(),
    )
    .await;
    eprintln!("transcript: Knowledge acknowledged at stream sequence {sequence}");
    let (acknowledged, outcome, resends) = wait_for_acknowledgement(&test.database, id).await;
    assert!(acknowledged);
    assert_eq!(outcome.as_deref(), Some("applied"));
    assert_eq!(resends, 1);

    tokio::time::sleep(Duration::from_millis(600)).await;
    sqlx::query("update x_archive.outbox_events set next_resend_at = now() where id = $1")
        .bind(id)
        .execute(test.database.pool())
        .await
        .expect("the resend is made due");
    let after = relay.run_once().await.expect("the pass after the answer");
    assert_eq!(after.resent, 0, "the acknowledgement ended the resend");
    eprintln!("transcript: relay pass 3 resent nothing");
    let info = stream.info().await.expect("the stream has info");
    assert_eq!(info.state.messages, 3, "two copies and the acknowledgement");

    let summary = consumer.finish().await;
    assert_eq!(summary.applied, 1);
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn a_refused_acknowledgement_stops_the_resend_raises_an_error_and_counts() {
    let _serial = ONE_AT_A_TIME.lock().await;
    static TELEMETRY: OnceLock<(LogBuffer, x_telemetry::TelemetryGuard)> = OnceLock::new();
    let (logs, guard) = TELEMETRY.get_or_init(install_telemetry);
    let fixture = fixture().await;
    let id = insert_removal(&fixture.test.database).await;
    let relay = OutboxRelay::new(
        fixture.test.database.clone(),
        Arc::new(JetStreamPublisher::new(fixture.context.clone())),
        ResendPolicy::new(Duration::from_secs(600), Duration::from_secs(3_600)),
    );
    relay.run_once().await.expect("the removal is published");
    let consumer = Running::start(&fixture.context, &fixture.test.database, &fixture.durable);

    let sequence = publish(
        &fixture.context,
        ACK_SUBJECT,
        Answer::refused(id).document(),
    )
    .await;
    let (acknowledged, outcome, _) = wait_for_acknowledgement(&fixture.test.database, id).await;
    wait_until_consumed(&fixture.context, &fixture.durable, sequence).await;
    let summary = consumer.finish().await;

    assert!(acknowledged, "a refusal ends the resend too");
    assert_eq!(outcome.as_deref(), Some("refused"));
    assert_eq!(summary.refused, 1);
    let refusal = logs
        .text()
        .lines()
        .find(|line| line.contains(&id.to_string()) && line.contains("\"ERROR\""))
        .expect("an ERROR line names the refused removal")
        .to_owned();
    assert!(
        !refusal.contains(OWNER) && !refusal.contains("social_source:"),
        "the line names ids only, never the owner or the subject: {refusal}"
    );
    let metrics = guard.render_metrics();
    assert!(
        metrics
            .lines()
            .any(|line| line.trim() == "x_tombstone_refused_total 1"),
        "the refusal is counted once:\n{metrics}"
    );
    fixture
        .test
        .cleanup()
        .await
        .expect("cleanup drops the database");
}

#[tokio::test]
async fn acknowledgements_that_name_no_open_removal_are_acknowledged_and_ignored() {
    let _serial = ONE_AT_A_TIME.lock().await;
    let fixture = fixture().await;
    let id = insert_removal(&fixture.test.database).await;
    let consumer = Running::start(&fixture.context, &fixture.test.database, &fixture.durable);

    let mut last = 0;
    for answer in [
        Answer::applied(Uuid::now_v7()),
        Answer {
            owner: OTHER_OWNER,
            ..Answer::applied(id)
        },
        Answer {
            family: "ai_archive",
            ..Answer::applied(id)
        },
    ] {
        last = publish(&fixture.context, ACK_SUBJECT, answer.document()).await;
    }
    let info = wait_until_consumed(&fixture.context, &fixture.durable, last).await;
    let summary = consumer.finish().await;

    assert_eq!(summary.ignored, 3, "{summary:?}");
    assert_eq!(
        (summary.applied, summary.refused, summary.terminated),
        (0, 0, 0)
    );
    assert_eq!(
        info.num_redelivered, 0,
        "each message was acknowledged once"
    );
    assert_eq!(
        row_state(&fixture.test.database, id).await,
        (false, None, 0),
        "no foreign acknowledgement changed the removal"
    );

    // The right tenant, family and id do end it, and a repeat changes nothing.
    let consumer = Running::start(&fixture.context, &fixture.test.database, &fixture.durable);
    publish(
        &fixture.context,
        ACK_SUBJECT,
        Answer::applied(id).document(),
    )
    .await;
    let last = publish(
        &fixture.context,
        ACK_SUBJECT,
        Answer::refused(id).document(),
    )
    .await;
    wait_until_consumed(&fixture.context, &fixture.durable, last).await;
    let summary = consumer.finish().await;
    assert_eq!((summary.applied, summary.ignored), (1, 1), "{summary:?}");
    assert_eq!(
        row_state(&fixture.test.database, id).await.1.as_deref(),
        Some("applied"),
        "the first answer stands"
    );
    fixture
        .test
        .cleanup()
        .await
        .expect("cleanup drops the database");
}

#[tokio::test]
async fn messages_that_are_not_a_valid_knowledge_acknowledgement_are_terminated() {
    let _serial = ONE_AT_A_TIME.lock().await;
    let fixture = fixture().await;
    let id = insert_removal(&fixture.test.database).await;
    let consumer = Running::start(&fixture.context, &fixture.test.database, &fixture.durable);

    let mut wrong_type: serde_json::Value =
        serde_json::from_str(&Answer::applied(id).document()).expect("a document");
    wrong_type["event_type"] = serde_json::Value::from("platform.operation.reported.v1");
    let mut refused_without_reason: serde_json::Value =
        serde_json::from_str(&Answer::applied(id).document()).expect("a document");
    refused_without_reason["payload"]["outcome"] = serde_json::Value::from("refused");
    let mut other_tenant: serde_json::Value =
        serde_json::from_str(&Answer::applied(id).document()).expect("a document");
    other_tenant["tenant_id"] = serde_json::Value::from(OTHER_OWNER);
    let mut last = 0;
    for body in [
        "not json at all".to_owned(),
        Answer {
            producer: "ratatoskr-chatgpt",
            ..Answer::applied(id)
        }
        .document(),
        wrong_type.to_string(),
        refused_without_reason.to_string(),
        other_tenant.to_string(),
    ] {
        last = publish(&fixture.context, ACK_SUBJECT, body).await;
    }
    let info = wait_until_consumed(&fixture.context, &fixture.durable, last).await;
    let summary = consumer.finish().await;

    assert_eq!(summary.terminated, 5, "{summary:?}");
    assert_eq!((summary.applied, summary.ignored), (0, 0));
    assert_eq!(
        info.num_redelivered, 0,
        "a terminated message is never redelivered"
    );
    assert_eq!(
        row_state(&fixture.test.database, id).await,
        (false, None, 0),
        "nothing that is not Knowledge's changed the removal"
    );
    fixture
        .test
        .cleanup()
        .await
        .expect("cleanup drops the database");
}

#[tokio::test]
async fn a_durable_with_another_filter_or_ack_wait_refuses_to_become_ready() {
    let _serial = ONE_AT_A_TIME.lock().await;
    let context = jetstream::new(
        async_nats::connect(nats_url())
            .await
            .expect("the test broker connects"),
    );
    let events = context
        .get_or_create_stream(jetstream::stream::Config {
            name: EVENTS_STREAM.to_owned(),
            subjects: vec!["evt.>".to_owned()],
            ..jetstream::stream::Config::default()
        })
        .await
        .expect("the events stream exists");
    let suffix = Uuid::now_v7().simple();
    let good = format!("x_deletion_acks_good_{suffix}");
    let other_filter = format!("x_deletion_acks_filter_{suffix}");
    let other_wait = format!("x_deletion_acks_wait_{suffix}");
    for config in [
        durable_config(&good, FILTER_SUBJECT, Duration::from_secs(30)),
        durable_config(&other_filter, "evt.knowledge.>", Duration::from_secs(30)),
        durable_config(&other_wait, FILTER_SUBJECT, Duration::from_secs(10)),
    ] {
        events
            .create_consumer(config)
            .await
            .expect("the durable exists");
    }

    ensure_acks_consumer(&context, EVENTS_STREAM, &good)
        .await
        .expect("a durable of the right shape verifies");
    for wrong in [&other_filter, &other_wait] {
        ensure_acks_consumer(&context, EVENTS_STREAM, wrong)
            .await
            .expect_err("a durable of another shape is refused");
    }
    ensure_acks_consumer(&context, EVENTS_STREAM, &format!("absent_{suffix}"))
        .await
        .expect_err("an absent durable is refused: the client never creates one");

    // The consumer itself refuses to run against the wrong shape.
    let test = TestDatabase::create().await.expect("a disposable database");
    consume_deletion_acks(
        &context,
        &test.database,
        EVENTS_STREAM,
        &other_wait,
        std::future::pending(),
    )
    .await
    .expect_err("the consumer verifies the durable before it reads");
    test.cleanup().await.expect("cleanup drops the database");
}
