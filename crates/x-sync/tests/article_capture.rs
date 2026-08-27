//! External-article capture and Extractor outcome projection.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use x_budget::gate::{BudgetClass, BudgetGate, Clock};
use x_persistence::test_support::TestDatabase;
use x_sync::{
    ArticleCaptureService, BookmarkPage, BookmarkPageSource, BookmarkSnapshotService,
    BookmarkSourceError, select_external_expanded_urls,
};

#[test]
fn external_expanded_urls_are_selected_once_and_x_urls_are_excluded() {
    let selected = select_external_expanded_urls(&[
        "https://Example.test:443/article?utm_source=x&b=2&a=1#summary".to_owned(),
        "https://example.test/article?a=1&b=2".to_owned(),
        "https://x.com/author/status/123".to_owned(),
        "https://twitter.com/author/status/123".to_owned(),
        "https://t.co/unexpanded".to_owned(),
        "mailto:reader@example.test".to_owned(),
    ]);

    assert_eq!(
        selected.len(),
        1,
        "only one canonical external URL is selected"
    );
    assert_eq!(
        selected[0].normalized_url, "https://example.test/article?a=1&b=2",
        "tracking, fragments, default ports, host case, and query ordering are canonicalized"
    );
}

#[tokio::test]
async fn bookmark_entity_expanded_url_queues_extractor_with_source_provenance() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("bookmark-entity-article-owner")
        .await
        .expect("the account seeds");
    let snapshot = bookmark_snapshot_service(test.database.clone());

    snapshot
        .run(
            account,
            &OneBookmarkPageSource("https://Example.test:443/article?b=2&utm_source=x&a=1#summary"),
            None,
        )
        .await
        .expect("the bookmark snapshot completes");

    let row: Option<(serde_json::Value, String, String)> = sqlx::query_as(
        "select payload, correlation_id, causation_id from x_archive.outbox_events \
         where event_type = 'content.capture.requested.v1'",
    )
    .fetch_optional(test.database.pool())
    .await
    .expect("the extractor outbox entry is readable");
    test.cleanup().await.expect("cleanup drops the database");

    let (payload, correlation_id, causation_id) =
        row.expect("an entity expanded URL queues one extractor command");
    assert_eq!(
        payload["payload"]["url"], "https://example.test/article?a=1&b=2",
        "the provider expanded URL is canonicalized before extraction"
    );
    assert!(
        correlation_id.starts_with("article_capture:"),
        "the command carries the capture correlation"
    );
    assert!(
        causation_id.starts_with("social_source:"),
        "the command traces back to the source post"
    );
}

#[tokio::test]
async fn changed_expanded_entity_updates_source_and_queues_new_capture() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("bookmark-entity-update-owner")
        .await
        .expect("the account seeds");
    let snapshot = bookmark_snapshot_service(test.database.clone());

    snapshot
        .run(
            account,
            &OneBookmarkPageSource("https://example.test/first"),
            None,
        )
        .await
        .expect("the first bookmark snapshot completes");
    snapshot
        .run(
            account,
            &OneBookmarkPageSource("https://example.test/second"),
            None,
        )
        .await
        .expect("the changed bookmark snapshot completes");

    let capture_urls: Vec<String> = sqlx::query_scalar(
        "select payload->'payload'->>'url' from x_archive.outbox_events \
         where event_type = 'content.capture.requested.v1' order by created_at, id",
    )
    .fetch_all(test.database.pool())
    .await
    .expect("the extractor command URLs are readable");
    let updated_events: i64 = sqlx::query_scalar(
        "select count(*) from x_archive.outbox_events where event_type = 'social.source.updated.v1'",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("the SocialSource updates are readable");
    test.cleanup().await.expect("cleanup drops the database");

    assert_eq!(
        capture_urls,
        vec![
            "https://example.test/first".to_owned(),
            "https://example.test/second".to_owned(),
        ],
        "a changed provider expanded entity requests the newly linked article"
    );
    assert_eq!(
        updated_events, 1,
        "the changed entity emits one SocialSource update"
    );
}

#[derive(Debug)]
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        "2026-08-27T12:00:00Z"
            .parse()
            .expect("the fixture clock instant parses")
    }
}

struct OneBookmarkPageSource(&'static str);

impl BookmarkPageSource for OneBookmarkPageSource {
    fn fetch_page<'a>(
        &'a self,
        _continuation: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<BookmarkPage, BookmarkSourceError>> + Send + 'a>> {
        let payload = serde_json::json!({
            "data": [{
                "id": "expanded-url-post",
                "text": "A provider link entity is present.",
                "author_id": "expanded-url-author",
                "created_at": "2026-08-27T11:00:00Z",
                "entities": {"urls": [{
                    "url": "https://t.co/short",
                    "expanded_url": self.0
                }]}
            }],
            "includes": {"users": [{
                "id": "expanded-url-author",
                "name": "Link Author",
                "username": "link_author"
            }]}
        });
        let envelope = serde_json::from_value(payload).expect("the fixture decodes");
        Box::pin(async move { Ok(BookmarkPage::new(envelope, None)) })
    }
}

fn bookmark_snapshot_service(
    database: x_persistence::database::Database,
) -> BookmarkSnapshotService {
    let clock: Arc<dyn Clock> = Arc::new(FixedClock);
    let budget = BudgetGate::with_clock(
        database.clone(),
        BudgetClass::Read,
        10,
        3_600,
        Arc::clone(&clock),
    )
    .expect("the fixture budget configuration is valid");
    BookmarkSnapshotService::new(database, budget, clock)
}

#[tokio::test]
async fn equivalent_links_across_posts_create_one_capture_command_and_two_links() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("article-capture-owner")
        .await
        .expect("the account seeds");
    let first_source = seed_social_source(&test, account, "article-post-one").await;
    let second_source = seed_social_source(&test, account, "article-post-two").await;
    let service = ArticleCaptureService::new(test.database.clone());

    service
        .capture_expanded_links(
            account,
            first_source,
            &["https://example.test/article?utm_source=x&b=2&a=1".to_owned()],
        )
        .await
        .expect("the first post link is captured");
    service
        .capture_expanded_links(
            account,
            second_source,
            &["https://EXAMPLE.test:443/article?a=1&b=2#summary".to_owned()],
        )
        .await
        .expect("the equivalent second post link is captured");

    let captures: i64 = sqlx::query_scalar("select count(*) from x_archive.article_captures")
        .fetch_one(test.database.pool())
        .await
        .expect("article capture count is readable");
    let links: i64 = sqlx::query_scalar("select count(*) from x_archive.post_article_links")
        .fetch_one(test.database.pool())
        .await
        .expect("post link count is readable");
    let commands: i64 = sqlx::query_scalar(
        "select count(*) from x_archive.outbox_events \
         where event_type = 'content.capture.requested.v1'",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("extractor command count is readable");

    test.cleanup().await.expect("cleanup drops the database");

    assert_eq!(captures, 1, "canonical URL capture is account-deduplicated");
    assert_eq!(
        links, 2,
        "every linked source post keeps its provenance link"
    );
    assert_eq!(
        commands, 1,
        "only the newly-created capture queues Extractor work"
    );
}

#[tokio::test]
async fn correlated_document_ir_outcome_updates_every_linked_post_once() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("article-outcome-owner")
        .await
        .expect("the account seeds");
    let first_source = seed_social_source(&test, account, "outcome-post-one").await;
    let second_source = seed_social_source(&test, account, "outcome-post-two").await;
    let service = ArticleCaptureService::new(test.database.clone());
    let url = "https://example.test/outcome".to_owned();

    service
        .capture_expanded_links(account, first_source, std::slice::from_ref(&url))
        .await
        .expect("the first source creates a capture");
    service
        .capture_expanded_links(account, second_source, std::slice::from_ref(&url))
        .await
        .expect("the second source joins the capture");

    let (capture_id, correlation_id, owner_id): (uuid::Uuid, String, uuid::Uuid) = sqlx::query_as(
        "select capture.id, capture.correlation_id, account.internal_user_id \
         from x_archive.article_captures capture \
         join x_archive.accounts account on account.id = capture.account_id",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("the queued capture is readable");
    let document_id = uuid::Uuid::now_v7();
    let event_id = uuid::Uuid::now_v7();
    let outcome = serde_json::json!({
        "event_id": event_id,
        "event_type": "platform.operation.reported.v1",
        "occurred_at": "2026-08-27T12:00:00Z",
        "producer": "ratatoskr-extractor",
        "aggregate_id": format!("operation:{capture_id}"),
        "correlation_id": correlation_id,
        "tenant_id": format!("user:{owner_id}"),
        "schema_version": 1,
        "payload": {
            "operation_id": capture_id,
            "status": "succeeded",
            "results": [{
                "result_kind": "content.document",
                "target": format!("document:{document_id}"),
                "blob": {
                    "owner_service": "ratatoskr-extractor",
                    "digest": {"algorithm": "sha256", "hex": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},
                    "media_type": "application/json",
                    "length_bytes": 42
                }
            }]
        }
    });

    service
        .consume_extractor_operation_report(&outcome.to_string())
        .await
        .expect("the correlated document result applies");
    service
        .consume_extractor_operation_report(&outcome.to_string())
        .await
        .expect("redelivery is idempotent");

    let linked_results: i64 = sqlx::query_scalar(
        "select count(*) from x_archive.post_article_links link \
         join x_archive.article_captures capture on capture.id = link.article_capture_id \
         where capture.state = 'completed' and capture.document_id = $1 \
           and capture.document_ir_blob is not null",
    )
    .bind(document_id.to_string())
    .fetch_one(test.database.pool())
    .await
    .expect("completed linked posts are readable");
    let inbox_count: i64 =
        sqlx::query_scalar("select count(*) from x_archive.inbox_events where event_id = $1")
            .bind(event_id.to_string())
            .fetch_one(test.database.pool())
            .await
            .expect("outcome inbox evidence is readable");

    test.cleanup().await.expect("cleanup drops the database");

    assert_eq!(
        linked_results, 2,
        "the one capture result is visible through both post links"
    );
    assert_eq!(inbox_count, 1, "the result event is only applied once");
}

#[tokio::test]
async fn foreign_or_malformed_outcomes_leave_capture_unapplied() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("article-outcome-rejection-owner")
        .await
        .expect("the account seeds");
    let source = seed_social_source(&test, account, "outcome-rejection-post").await;
    let service = ArticleCaptureService::new(test.database.clone());
    service
        .capture_expanded_links(
            account,
            source,
            &["https://example.test/rejected".to_owned()],
        )
        .await
        .expect("the capture queues");
    let (capture_id, correlation_id): (uuid::Uuid, String) =
        sqlx::query_as("select id, correlation_id from x_archive.article_captures")
            .fetch_one(test.database.pool())
            .await
            .expect("the queued capture is readable");
    let result = serde_json::json!({
        "result_kind": "content.document",
        "target": format!("document:{}", uuid::Uuid::now_v7()),
        "blob": {
            "owner_service": "ratatoskr-extractor",
            "digest": {"algorithm": "sha256", "hex": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"},
            "media_type": "application/json",
            "length_bytes": 42
        }
    });
    let foreign = extractor_outcome(
        uuid::Uuid::now_v7(),
        capture_id,
        &correlation_id,
        uuid::Uuid::now_v7(),
        &result,
    );
    let malformed_result = serde_json::json!({
        "result_kind": "content.document",
        "target": format!("document:{}", uuid::Uuid::now_v7())
    });
    let malformed = extractor_outcome(
        uuid::Uuid::now_v7(),
        capture_id,
        &correlation_id,
        account_owner(&test, account).await,
        &malformed_result,
    );
    let unknown_correlation = extractor_outcome(
        uuid::Uuid::now_v7(),
        capture_id,
        &format!("article_capture:{}", uuid::Uuid::now_v7()),
        account_owner(&test, account).await,
        &result,
    );

    assert!(
        service
            .consume_extractor_operation_report(&foreign.to_string())
            .await
            .is_err(),
        "a foreign tenant report is rejected"
    );
    assert!(
        service
            .consume_extractor_operation_report(&malformed.to_string())
            .await
            .is_err(),
        "a result without a Document IR blob is rejected"
    );
    assert!(
        service
            .consume_extractor_operation_report(&unknown_correlation.to_string())
            .await
            .is_err(),
        "an unknown capture correlation is rejected"
    );

    let state: String = sqlx::query_scalar("select state from x_archive.article_captures")
        .fetch_one(test.database.pool())
        .await
        .expect("capture state is readable");
    let inbox_count: i64 = sqlx::query_scalar("select count(*) from x_archive.inbox_events")
        .fetch_one(test.database.pool())
        .await
        .expect("inbox state is readable");

    test.cleanup().await.expect("cleanup drops the database");

    assert_eq!(
        state, "requested",
        "rejected outcomes cannot complete the capture"
    );
    assert_eq!(
        inbox_count, 0,
        "rejected outcomes leave no applied inbox evidence"
    );
}

fn extractor_outcome(
    event_id: uuid::Uuid,
    capture_id: uuid::Uuid,
    correlation_id: &str,
    owner_id: uuid::Uuid,
    result: &serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "event_id": event_id,
        "event_type": "platform.operation.reported.v1",
        "occurred_at": "2026-08-27T12:00:00Z",
        "producer": "ratatoskr-extractor",
        "aggregate_id": format!("operation:{capture_id}"),
        "correlation_id": correlation_id,
        "tenant_id": format!("user:{owner_id}"),
        "schema_version": 1,
        "payload": {
            "operation_id": capture_id,
            "status": "succeeded",
            "results": [result]
        }
    })
}

async fn account_owner(test: &TestDatabase, account: uuid::Uuid) -> uuid::Uuid {
    sqlx::query_scalar("select internal_user_id from x_archive.accounts where id = $1")
        .bind(account)
        .fetch_one(test.database.pool())
        .await
        .expect("account owner is readable")
}

async fn seed_social_source(
    test: &TestDatabase,
    account: uuid::Uuid,
    provider_post_id: &str,
) -> uuid::Uuid {
    let author: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.users (provider_id, parser_version) values ($1, 1) returning id",
    )
    .bind(format!("{provider_post_id}-author"))
    .fetch_one(test.database.pool())
    .await
    .expect("the post author seeds");
    let post: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.posts (provider_id, author_user_id, parser_version) \
         values ($1, $2, 1) returning id",
    )
    .bind(provider_post_id)
    .bind(author)
    .fetch_one(test.database.pool())
    .await
    .expect("the post seeds");
    sqlx::query_scalar(
        "insert into x_archive.social_sources (account_id, post_id, current_content_digest, \
         acquisition, saved_authority, captured_at) \
         values ($1, $2, '{}'::jsonb, 'official_api', 'authoritative_platform_state', now()) \
         returning social_source_id",
    )
    .bind(account)
    .bind(post)
    .fetch_one(test.database.pool())
    .await
    .expect("the source seeds")
}
