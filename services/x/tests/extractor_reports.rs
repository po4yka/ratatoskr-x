//! Linked-article captures complete from the extractor's operation reports (XR-021 CONTRACTS.md
//! S04 and S10): the command leaves through the relay, the report returns through the
//! Edge-provisioned durable, and reports that are not ours are acknowledged and ignored.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::sync::Arc;
use std::time::Duration;

use async_nats::jetstream;
use ratatoskr_x::extractor_reports::{
    FILTER_SUBJECT, consume_extractor_reports, ensure_reports_consumer,
};
use x_capture::relay::{JetStreamPublisher, OutboxRelay};
use x_persistence::database::Database;
use x_persistence::test_support::TestDatabase;
use x_sync::ArticleCaptureService;

struct Fixture {
    test: TestDatabase,
    context: jetstream::Context,
    events: jetstream::stream::Stream,
    durable: String,
}

#[expect(
    clippy::disallowed_methods,
    reason = "the integration binary chooses its isolated JetStream endpoint"
)]
fn nats_url() -> String {
    std::env::var("X_TEST_NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:14224".to_owned())
}

async fn fixture() -> Fixture {
    let test = TestDatabase::create().await.expect("a disposable database");
    let context = jetstream::new(
        async_nats::connect(nats_url())
            .await
            .expect("the test broker connects"),
    );
    for (name, subject) in [
        ("ratatoskr_commands", "cmd.>"),
        ("ratatoskr_events", "evt.>"),
    ] {
        context
            .get_or_create_stream(jetstream::stream::Config {
                name: name.to_owned(),
                subjects: vec![subject.to_owned()],
                ..jetstream::stream::Config::default()
            })
            .await
            .expect("the stream exists");
    }
    let events = context
        .get_stream("ratatoskr_events")
        .await
        .expect("the events stream");
    let durable = format!("x_extractor_reports_{}", uuid::Uuid::now_v7().simple());
    events
        .create_consumer(jetstream::consumer::pull::Config {
            durable_name: Some(durable.clone()),
            filter_subject: FILTER_SUBJECT.to_owned(),
            ack_policy: jetstream::consumer::AckPolicy::Explicit,
            ack_wait: Duration::from_secs(30),
            deliver_policy: jetstream::consumer::DeliverPolicy::New,
            ..jetstream::consumer::pull::Config::default()
        })
        .await
        .expect("Edge provisions the durable");
    Fixture {
        test,
        context,
        events,
        durable,
    }
}

/// An account, a social source and a queued article capture; returns the capture and its owner.
async fn queued_article_capture(test: &TestDatabase) -> (uuid::Uuid, String, uuid::Uuid) {
    let account = test
        .seed_account("extractor-report-owner")
        .await
        .expect("the account seeds");
    let author: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.users (provider_id, parser_version) \
         values ('report-author', 1) returning id",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("the author seeds");
    let post: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.posts (provider_id, author_user_id, parser_version) \
         values ('report-post', $1, 1) returning id",
    )
    .bind(author)
    .fetch_one(test.database.pool())
    .await
    .expect("the post seeds");
    let source: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.social_sources (account_id, post_id, current_content_digest, \
         acquisition, saved_authority, captured_at) \
         values ($1, $2, '{}'::jsonb, 'official_api', 'authoritative_platform_state', now()) \
         returning social_source_id",
    )
    .bind(account)
    .bind(post)
    .fetch_one(test.database.pool())
    .await
    .expect("the source seeds");
    ArticleCaptureService::new(test.database.clone())
        .capture_expanded_links(account, source, &["https://example.test/report".to_owned()])
        .await
        .expect("the article capture is queued");
    let (capture, correlation, owner): (uuid::Uuid, String, uuid::Uuid) = sqlx::query_as(
        "select capture.id, capture.correlation_id, account.internal_user_id \
         from x_archive.article_captures capture \
         join x_archive.accounts account on account.id = capture.account_id",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("the capture is readable");
    (capture, correlation, owner)
}

fn extractor_report(
    capture: uuid::Uuid,
    correlation: &str,
    owner: uuid::Uuid,
    operation: uuid::Uuid,
) -> String {
    let document = uuid::Uuid::now_v7();
    serde_json::json!({
        "event_id": uuid::Uuid::now_v7(),
        "event_type": "platform.operation.reported.v1",
        "occurred_at": "2026-08-27T12:00:00Z",
        "producer": "ratatoskr-extractor",
        "aggregate_id": format!("operation:{capture}"),
        "correlation_id": correlation,
        "tenant_id": format!("user:{owner}"),
        "schema_version": 1,
        "payload": {
            "operation_id": operation,
            "status": "succeeded",
            "results": [{
                "result_kind": "content.document",
                "target": format!("document:{document}"),
                "blob": {
                    "owner_service": "ratatoskr-extractor",
                    "digest": {"algorithm": "sha256", "hex": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
                    "media_type": "application/json",
                    "length_bytes": 42
                }
            }]
        }
    })
    .to_string()
}

async fn publish(context: &jetstream::Context, document: String) {
    context
        .publish(FILTER_SUBJECT, document.into_bytes().into())
        .await
        .expect("the report publishes")
        .await
        .expect("JetStream stores the report");
}

async fn wait_until<F, Fut>(what: &str, mut condition: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !condition().await {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn durable_info(
    events: &jetstream::stream::Stream,
    durable: &str,
) -> jetstream::consumer::Info {
    let mut consumer: jetstream::consumer::Consumer<jetstream::consumer::pull::Config> =
        events.get_consumer(durable).await.expect("the durable");
    consumer.info().await.expect("consumer info").clone()
}

async fn capture_state(database: &Database) -> String {
    sqlx::query_scalar("select state from x_archive.article_captures")
        .fetch_one(database.pool())
        .await
        .expect("the capture is readable")
}

#[tokio::test]
async fn a_relayed_article_capture_completes_from_the_extractor_report() {
    let fixture = fixture().await;
    let (capture, correlation, owner) = queued_article_capture(&fixture.test).await;
    let relay = OutboxRelay::new(
        fixture.test.database.clone(),
        Arc::new(JetStreamPublisher::new(fixture.context.clone())),
    );
    let relayed = relay.run_once().await.expect("the relay pass completes");
    assert_eq!(
        relayed.published, 1,
        "the article command left through the relay"
    );

    let database = fixture.test.database.clone();
    let context = fixture.context.clone();
    let durable = fixture.durable.clone();
    let consumer = tokio::spawn(async move {
        consume_extractor_reports(
            &context,
            &database,
            "ratatoskr_events",
            &durable,
            std::future::pending(),
        )
        .await
    });
    // A report from another producer, one for an unknown operation, and garbage come first: all
    // are acknowledged and ignored, and none of them stops the consumer.
    publish(
        &fixture.context,
        extractor_report(capture, &correlation, owner, capture)
            .replace("ratatoskr-extractor", "ratatoskr-instagram"),
    )
    .await;
    let stranger = uuid::Uuid::now_v7();
    publish(
        &fixture.context,
        extractor_report(
            stranger,
            &format!("article_capture:{stranger}"),
            owner,
            stranger,
        ),
    )
    .await;
    publish(&fixture.context, "not json at all".to_owned()).await;
    publish(
        &fixture.context,
        extractor_report(capture, &correlation, owner, capture),
    )
    .await;

    let database = fixture.test.database.clone();
    wait_until("the capture completes", || {
        let database = database.clone();
        async move { capture_state(&database).await == "completed" }
    })
    .await;
    wait_until("every report is acknowledged", || {
        let (events, durable) = (fixture.events.clone(), fixture.durable.clone());
        async move {
            let info = durable_info(&events, &durable).await;
            info.num_pending == 0 && info.num_ack_pending == 0
        }
    })
    .await;
    assert!(!consumer.is_finished(), "the consumer keeps running");

    consumer.abort();
    let _ = consumer.await;
    fixture
        .events
        .delete_consumer(&fixture.durable)
        .await
        .expect("test durable cleanup");
    fixture
        .test
        .cleanup()
        .await
        .expect("cleanup drops the database");
}

#[tokio::test]
async fn a_database_failure_leaves_the_report_unacknowledged_for_redelivery() {
    let fixture = fixture().await;
    let (capture, correlation, owner) = queued_article_capture(&fixture.test).await;
    let database = fixture.test.database.clone();
    let context = fixture.context.clone();
    let durable = fixture.durable.clone();
    // Closing the pool makes every query fail: the report must be negatively acknowledged.
    database.pool().close().await;
    let consumer = tokio::spawn(async move {
        consume_extractor_reports(
            &context,
            &database,
            "ratatoskr_events",
            &durable,
            std::future::pending(),
        )
        .await
    });
    publish(
        &fixture.context,
        extractor_report(capture, &correlation, owner, capture),
    )
    .await;

    wait_until("the report is delivered and not acknowledged", || {
        let (events, durable) = (fixture.events.clone(), fixture.durable.clone());
        async move {
            let info = durable_info(&events, &durable).await;
            info.delivered.stream_sequence > info.ack_floor.stream_sequence
        }
    })
    .await;
    assert!(
        !consumer.is_finished(),
        "a database failure does not stop the consumer"
    );

    consumer.abort();
    let _ = consumer.await;
    fixture
        .events
        .delete_consumer(&fixture.durable)
        .await
        .expect("test durable cleanup");
    fixture
        .test
        .cleanup()
        .await
        .expect("cleanup drops the database");
}

#[tokio::test]
async fn a_mismatched_durable_is_refused_at_startup() {
    let fixture = fixture().await;
    let wrong = format!("x_wrong_reports_{}", uuid::Uuid::now_v7().simple());
    fixture
        .events
        .create_consumer(jetstream::consumer::pull::Config {
            durable_name: Some(wrong.clone()),
            filter_subject: "evt.social.source.captured.v1".to_owned(),
            ack_policy: jetstream::consumer::AckPolicy::Explicit,
            ack_wait: Duration::from_secs(30),
            ..jetstream::consumer::pull::Config::default()
        })
        .await
        .expect("a wrong durable exists");

    ensure_reports_consumer(&fixture.context, "ratatoskr_events", &fixture.durable)
        .await
        .expect("the correctly provisioned durable verifies");
    ensure_reports_consumer(&fixture.context, "ratatoskr_events", &wrong)
        .await
        .expect_err("a durable with another filter is refused");
    ensure_reports_consumer(&fixture.context, "ratatoskr_events", "x_missing_reports")
        .await
        .expect_err("a missing durable is refused: this client never creates one");

    fixture
        .events
        .delete_consumer(&wrong)
        .await
        .expect("wrong durable cleanup");
    fixture
        .events
        .delete_consumer(&fixture.durable)
        .await
        .expect("test durable cleanup");
    fixture
        .test
        .cleanup()
        .await
        .expect("cleanup drops the database");
}
