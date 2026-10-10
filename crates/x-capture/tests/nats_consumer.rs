//! `JetStream` delivery reaches the durable X browser-capture inbox.

#![allow(clippy::expect_used, clippy::panic, reason = "test assertions")]

use std::time::Duration;

use async_nats::jetstream;
use futures_util::StreamExt as _;
use ratatoskr_event_envelope::CommandEnvelope;
use x_capture::{COMMAND_SUBJECT, ConsumerError, ConsumerReport, consume_browser_commands};
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

/// Both tests publish to `COMMAND_SUBJECT` on the one shared `ratatoskr_commands` stream, so a
/// message published by one would also reach the durable the other created. Holding this for the
/// whole life of a harness runs the two scenarios one at a time.
static SHARED_STREAM: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct Harness {
    _exclusive: tokio::sync::MutexGuard<'static, ()>,
    database: TestDatabase,
    context: jetstream::Context,
    client: async_nats::Client,
    stream: jetstream::stream::Stream,
    stream_name: String,
    durable_name: String,
}

impl Harness {
    async fn start() -> Self {
        let exclusive = SHARED_STREAM.lock().await;
        let database = TestDatabase::create().await.expect("test database");
        let client = async_nats::connect(nats_url()).await.expect("test NATS");
        let context = jetstream::new(client.clone());
        let stream_name = "ratatoskr_commands".to_owned();
        let durable_name = format!("x_browser_capture_test_{}", uuid::Uuid::now_v7().simple());
        let stream = context
            .get_or_create_stream(jetstream::stream::Config {
                name: stream_name.clone(),
                subjects: vec!["cmd.>".to_owned()],
                ..jetstream::stream::Config::default()
            })
            .await
            .expect("command stream");
        stream
            .create_consumer(jetstream::consumer::pull::Config {
                durable_name: Some(durable_name.clone()),
                filter_subject: COMMAND_SUBJECT.to_owned(),
                ack_policy: jetstream::consumer::AckPolicy::Explicit,
                deliver_policy: jetstream::consumer::DeliverPolicy::New,
                ..jetstream::consumer::pull::Config::default()
            })
            .await
            .expect("Platform preprovisions the X durable");
        Self {
            _exclusive: exclusive,
            database,
            context,
            client,
            stream,
            stream_name,
            durable_name,
        }
    }

    fn spawn_consumer(&self) -> tokio::task::JoinHandle<Result<ConsumerReport, ConsumerError>> {
        let database = self.database.database.clone();
        let context = self.context.clone();
        let stream = self.stream_name.clone();
        let durable = self.durable_name.clone();
        tokio::spawn(async move {
            consume_browser_commands(
                &context,
                &database,
                &stream,
                &durable,
                std::future::pending(),
            )
            .await
        })
    }

    async fn publish(&self, payload: Vec<u8>) {
        self.context
            .publish(COMMAND_SUBJECT, payload.into())
            .await
            .expect("command publishes")
            .await
            .expect("command is stored by JetStream");
    }

    async fn finish(
        self,
        consumer: tokio::task::JoinHandle<Result<ConsumerReport, ConsumerError>>,
    ) {
        consumer.abort();
        let _ = consumer.await;
        self.database
            .cleanup()
            .await
            .expect("test database cleanup");
        self.stream
            .delete_consumer(&self.durable_name)
            .await
            .expect("test durable cleanup");
    }
}

#[tokio::test]
async fn consumes_the_provider_subject_into_the_durable_x_inbox() {
    let harness = Harness::start().await;
    let consumer = harness.spawn_consumer();
    let command = CommandEnvelope::from_json(X_COMMAND.as_bytes()).expect("command parses");
    let payload = command.to_canonical_json().expect("command serialises");
    harness.publish(payload.into()).await;

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let count: i64 = sqlx::query_scalar("select count(*) from x_archive.explicit_captures")
                .fetch_one(harness.database.database.pool())
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

    harness.finish(consumer).await;
}

/// CONTRACTS.md S02 rule 7: a permanently invalid command is terminated (`Term`), not just
/// acknowledged, so the broker records the decision and never redelivers it.
#[tokio::test]
async fn a_command_without_a_tenant_is_terminated_not_acknowledged() {
    let harness = Harness::start().await;
    let mut advisories = harness
        .client
        .subscribe(format!(
            "$JS.EVENT.ADVISORY.CONSUMER.MSG_TERMINATED.{}.{}",
            harness.stream_name, harness.durable_name
        ))
        .await
        .expect("advisory subscription");
    let consumer = harness.spawn_consumer();
    let without_tenant = X_COMMAND
        .lines()
        .filter(|line| !line.contains("\"tenant_id\""))
        .collect::<Vec<_>>()
        .join("\n");
    harness.publish(without_tenant.into_bytes()).await;

    let terminated = tokio::time::timeout(Duration::from_secs(5), advisories.next())
        .await
        .expect("the broker records a termination for the poison command");
    assert!(terminated.is_some(), "the advisory subscription stays open");
    let count: i64 = sqlx::query_scalar("select count(*) from x_archive.explicit_captures")
        .fetch_one(harness.database.database.pool())
        .await
        .expect("capture count");
    assert_eq!(count, 0, "a command without a tenant stores nothing");

    harness.finish(consumer).await;
}

#[expect(
    clippy::disallowed_methods,
    reason = "the integration binary chooses its isolated JetStream endpoint"
)]
fn nats_url() -> String {
    std::env::var("X_TEST_NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:14224".to_owned())
}
