//! `JetStream` delivery reaches the durable X browser-capture inbox.

#![allow(clippy::expect_used, clippy::panic, reason = "test assertions")]

use std::time::Duration;

use async_nats::jetstream;
use ratatoskr_event_envelope::CommandEnvelope;
use x_capture::{COMMAND_SUBJECT, consume_browser_commands};
use x_persistence::test_support::TestDatabase;

const X_COMMAND: &str = r#"{
  "command_id": "018f0000-0000-7000-8000-000000000001",
  "command_type": "social.capture.requested.v1",
  "issued_at": "2026-08-27T10:00:00Z",
  "producer": "ratatoskr-platform",
  "aggregate_id": "operation:018f0000-0000-7000-8000-000000000002",
  "correlation_id": "operation:018f0000-0000-7000-8000-000000000002",
  "tenant_id": "user:018f0000-0000-7000-8000-000000000003",
  "schema_version": 1,
  "payload": {
    "operation_id": "018f0000-0000-7000-8000-000000000002",
    "idempotency_key": {"algorithm": "sha256", "hex": "0000000000000000000000000000000000000000000000000000000000000000"},
    "original_permalink": "https://x.com/ratatoskr/status/1",
    "captured_at": "2026-08-27T10:00:00Z",
    "provider": "x",
    "acquisition": "browser_extension",
    "saved_authority": "explicit_user_capture"
  }
}"#;

#[tokio::test]
async fn consumes_the_provider_subject_into_the_durable_x_inbox() {
    let database = TestDatabase::create().await.expect("test database");
    let client = async_nats::connect(nats_url()).await.expect("test NATS");
    let context = jetstream::new(client);
    let stream_name = format!("x_browser_capture_{}", uuid::Uuid::now_v7().simple());
    context
        .create_stream(jetstream::stream::Config {
            name: stream_name.clone(),
            subjects: vec![COMMAND_SUBJECT.to_owned()],
            ..jetstream::stream::Config::default()
        })
        .await
        .expect("command stream");
    let stream = context
        .get_stream(&stream_name)
        .await
        .expect("command stream");
    stream
        .create_consumer(jetstream::consumer::pull::Config {
            durable_name: Some("x_browser_capture_test".to_owned()),
            filter_subject: COMMAND_SUBJECT.to_owned(),
            ack_policy: jetstream::consumer::AckPolicy::Explicit,
            ..jetstream::consumer::pull::Config::default()
        })
        .await
        .expect("Platform preprovisions the X durable");

    let consumer_database = database.database.clone();
    let consumer_context = context.clone();
    let consumer_stream = stream_name.clone();
    let consumer = tokio::spawn(async move {
        consume_browser_commands(
            &consumer_context,
            &consumer_database,
            &consumer_stream,
            "x_browser_capture_test",
            std::future::pending(),
        )
        .await
    });
    let command = CommandEnvelope::from_json(X_COMMAND.as_bytes()).expect("command parses");
    let payload = command.to_canonical_json().expect("command serialises");
    context
        .publish(COMMAND_SUBJECT, payload.into())
        .await
        .expect("command publishes")
        .await
        .expect("command is stored by JetStream");

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let count: i64 = sqlx::query_scalar("select count(*) from x_archive.explicit_captures")
                .fetch_one(database.database.pool())
                .await
                .expect("capture count");
            if count == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("consumer applies the command");

    consumer.abort();
    let _ = consumer.await;
    database.cleanup().await.expect("test database cleanup");
    context
        .delete_stream(&stream_name)
        .await
        .expect("test command stream cleanup");
}

#[expect(
    clippy::disallowed_methods,
    reason = "the integration binary chooses its isolated JetStream endpoint"
)]
fn nats_url() -> String {
    std::env::var("X_TEST_NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:14224".to_owned())
}
