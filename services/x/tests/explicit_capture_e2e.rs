//! One X permalink end to end (XR-021 CONTRACTS.md S10): a Platform-shaped command is consumed,
//! resolved through the app-only API, and the relay leaves the broker holding the queued report,
//! the owner-scoped social fact and the terminal report, in that order, or the honest
//! `unavailable` report and no social fact at all.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream;
use ratatoskr_event_envelope::EventEnvelope;
use ratatoskr_operation_contracts::{OperationReported, OperationStatus};
use ratatoskr_social_contracts::SocialSourceCaptured;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};
use x_budget::gate::SystemClock;
use x_capture::consume_browser_commands;
use x_capture::relay::{JetStreamPublisher, OutboxRelay};
use x_persistence::test_support::TestDatabase;
use x_sync::{AppBearerResolver, CapturePolicy, PublicCaptureWorker};

/// The scenarios share the `cmd.x.capture.requested.v1` subject, so they run one at a time.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const POST: &str = "1900000000000000007";

#[expect(
    clippy::disallowed_methods,
    reason = "the integration binary chooses its isolated JetStream endpoint"
)]
fn nats_url() -> String {
    std::env::var("X_TEST_NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:14224".to_owned())
}

fn command(owner: uuid::Uuid, command_id: uuid::Uuid, operation: uuid::Uuid) -> String {
    serde_json::json!({
        "command_id": command_id,
        "command_type": "social.capture.requested.v1",
        "issued_at": "2026-08-27T10:00:00Z",
        "producer": "ratatoskr-platform",
        "aggregate_id": format!("operation:{operation}"),
        "correlation_id": format!("operation:{operation}"),
        "tenant_id": format!("user:{owner}"),
        "schema_version": 1,
        "payload": {
            "operation_id": operation,
            "idempotency_key": {"algorithm": "sha256", "hex": "0000000000000000000000000000000000000000000000000000000000000000"},
            "original_permalink": format!("https://x.com/ada/status/{POST}"),
            "captured_at": "2026-08-27T10:00:00Z",
            "provider": "x",
            "acquisition": "browser_extension",
            "saved_authority": "explicit_user_capture"
        }
    })
    .to_string()
}

async fn x_api(response: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/2/tweets"))
        .and(query_param("ids", POST))
        .respond_with(response)
        .mount(&server)
        .await;
    server
}

fn public_post() -> serde_json::Value {
    serde_json::json!({
        "data": [{
            "id": POST,
            "text": "One public post, end to end.",
            "author_id": "42",
            "created_at": "2026-08-16T09:30:00.000Z"
        }],
        "includes": {"users": [{"id": "42", "name": "Ada Example", "username": "ada"}]}
    })
}

fn not_authorized() -> serde_json::Value {
    serde_json::json!({"errors": [{
        "value": POST,
        "detail": "Sorry, you are not authorized to see the Tweet with id.",
        "title": "Authorization Error",
        "resource_type": "tweet",
        "parameter": "ids",
        "resource_id": POST,
        "type": "https://api.twitter.com/2/problems/not-authorized-for-resource"
    }]})
}

/// Provisions what Edge and Platform provision: both streams and this scenario's X durable.
async fn provision(
    context: &jetstream::Context,
) -> (jetstream::stream::Stream, jetstream::stream::Stream, String) {
    let commands = context
        .get_or_create_stream(jetstream::stream::Config {
            name: "ratatoskr_commands".to_owned(),
            subjects: vec!["cmd.>".to_owned()],
            ..jetstream::stream::Config::default()
        })
        .await
        .expect("the command stream");
    let events = context
        .get_or_create_stream(jetstream::stream::Config {
            name: "ratatoskr_events".to_owned(),
            subjects: vec!["evt.>".to_owned()],
            ..jetstream::stream::Config::default()
        })
        .await
        .expect("the event stream");
    let durable = format!("x_e2e_capture_{}", uuid::Uuid::now_v7().simple());
    commands
        .create_consumer(jetstream::consumer::pull::Config {
            durable_name: Some(durable.clone()),
            filter_subject: x_capture::COMMAND_SUBJECT.to_owned(),
            ack_policy: jetstream::consumer::AckPolicy::Explicit,
            deliver_policy: jetstream::consumer::DeliverPolicy::New,
            ..jetstream::consumer::pull::Config::default()
        })
        .await
        .expect("Platform preprovisions the X durable");
    (commands, events, durable)
}

/// Runs one command through the consumer, the worker and the relay; returns the outbox rows in
/// creation order with the stored messages the broker holds for them.
async fn run_scenario(
    api: &MockServer,
    owner: uuid::Uuid,
) -> Vec<(uuid::Uuid, String, EventEnvelope)> {
    let test = TestDatabase::create().await.expect("a disposable database");
    let context = jetstream::new(
        async_nats::connect(nats_url())
            .await
            .expect("the test broker connects"),
    );
    let (commands, events, durable) = provision(&context).await;
    let (consumer_database, consumer_context, consumer_durable) =
        (test.database.clone(), context.clone(), durable.clone());
    let consumer = tokio::spawn(async move {
        consume_browser_commands(
            &consumer_context,
            &consumer_database,
            "ratatoskr_commands",
            &consumer_durable,
            std::future::pending(),
        )
        .await
    });

    let (command_id, operation) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    context
        .publish(
            x_capture::COMMAND_SUBJECT,
            command(owner, command_id, operation).into_bytes().into(),
        )
        .await
        .expect("the command publishes")
        .await
        .expect("JetStream stores the command");
    wait_for("the consumer persists the capture", || {
        let database = test.database.clone();
        async move {
            let count: i64 = sqlx::query_scalar("select count(*) from x_archive.explicit_captures")
                .fetch_one(database.pool())
                .await
                .expect("capture count");
            count == 1
        }
    })
    .await;

    let resolver = AppBearerResolver::new(&api.uri(), "end-to-end-token").expect("the resolver");
    let worker = PublicCaptureWorker::new(
        test.database.clone(),
        Arc::new(resolver),
        Arc::new(SystemClock),
        CapturePolicy::new(5, 8),
    );
    let summary = worker
        .run_due_once()
        .await
        .expect("the worker pass completes");
    assert_eq!(summary.claimed, 1);

    let relay = OutboxRelay::new(
        test.database.clone(),
        Arc::new(JetStreamPublisher::new(context.clone())),
    );
    let relayed = relay.run_once().await.expect("the relay pass completes");

    let rows: Vec<(uuid::Uuid, String)> = sqlx::query_as(
        "select id, event_type from x_archive.outbox_events order by created_at, id",
    )
    .fetch_all(test.database.pool())
    .await
    .expect("the outbox is readable");
    assert_eq!(usize::try_from(relayed.published).ok(), Some(rows.len()));
    let mut held = Vec::new();
    for (id, event_type) in rows {
        let stream = if event_type.starts_with("content.") {
            &commands
        } else {
            &events
        };
        let envelope = broker_copy(stream, &id.to_string())
            .await
            .unwrap_or_else(|| panic!("{event_type} reached the broker"));
        held.push((id, event_type, envelope));
    }
    consumer.abort();
    let _ = consumer.await;
    commands
        .delete_consumer(&durable)
        .await
        .expect("test durable cleanup");
    test.cleanup().await.expect("cleanup drops the database");
    held
}

async fn wait_for<F, Fut>(what: &str, mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !condition().await {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The envelope the broker stored under `Nats-Msg-Id: message_id`, newest first.
async fn broker_copy(
    stream: &jetstream::stream::Stream,
    message_id: &str,
) -> Option<EventEnvelope> {
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
        if message
            .headers
            .get(async_nats::header::NATS_MESSAGE_ID)
            .is_some_and(|value| value.as_str() == message_id)
        {
            return EventEnvelope::from_json(&message.payload).ok();
        }
    }
    None
}

fn report(envelope: &EventEnvelope) -> OperationReported {
    envelope.payload_as().expect("an operation report")
}

#[tokio::test]
async fn a_public_post_is_queued_preserved_and_reported_through_the_broker() {
    let _one = ONE_AT_A_TIME.lock().await;
    let api = x_api(ResponseTemplate::new(200).set_body_json(public_post())).await;
    let owner = uuid::Uuid::now_v7();

    let held = run_scenario(&api, owner).await;

    let types: Vec<&str> = held.iter().map(|(_, kind, _)| kind.as_str()).collect();
    assert_eq!(
        types,
        [
            "platform.operation.reported.v1",
            "social.source.captured.v1",
            "platform.operation.reported.v1",
        ],
        "queued report, the social fact, the terminal report"
    );
    for (id, _, envelope) in &held {
        assert_eq!(envelope.event_id.0, *id, "the row id is the event id");
        assert_eq!(envelope.producer.as_str(), "ratatoskr-x");
        assert_eq!(
            envelope.tenant_id.map(|tenant| tenant.to_string()),
            Some(format!("user:{owner}"))
        );
    }
    let queued = report(&held.first().expect("the element exists").2);
    assert_eq!(queued.status, OperationStatus::Queued);
    let captured: SocialSourceCaptured = held
        .get(1)
        .expect("the element exists")
        .2
        .payload_as()
        .expect("a captured fact");
    let snapshot = serde_json::to_value(captured.source).expect("the snapshot serializes");
    assert_eq!(snapshot["owner"], format!("user:{owner}"));
    assert_eq!(snapshot["external_post_id"], POST);
    assert_eq!(snapshot["text"], "One public post, end to end.");
    let succeeded = report(&held.get(2).expect("the element exists").2);
    assert_eq!(succeeded.status, OperationStatus::Succeeded);
    assert_eq!(queued.operation_id, succeeded.operation_id);
    assert_eq!(
        succeeded
            .results
            .first()
            .expect("the element exists")
            .target
            .to_wire(),
        format!(
            "social_source:{}",
            snapshot["social_source_id"].as_str().expect("an id")
        ),
        "the terminal report points at the fact's source"
    );
}

#[tokio::test]
async fn a_post_the_app_may_not_read_is_reported_unavailable_with_no_social_fact() {
    let _one = ONE_AT_A_TIME.lock().await;
    let api = x_api(ResponseTemplate::new(200).set_body_json(not_authorized())).await;
    let owner = uuid::Uuid::now_v7();

    let held = run_scenario(&api, owner).await;

    let types: Vec<&str> = held.iter().map(|(_, kind, _)| kind.as_str()).collect();
    assert_eq!(
        types,
        [
            "platform.operation.reported.v1",
            "platform.operation.reported.v1"
        ],
        "the queued report and the terminal report, and no social fact"
    );
    assert_eq!(
        report(&held.first().expect("the element exists").2).status,
        OperationStatus::Queued
    );
    let terminal = report(&held.get(1).expect("the element exists").2);
    assert_eq!(terminal.status, OperationStatus::Failed);
    let error = terminal.error.expect("a failed report carries its error");
    assert_eq!(error.code.as_str(), "social.source.unavailable");
    assert!(!error.retryable);
}
