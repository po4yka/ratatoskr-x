//! Knowledge request and completion linkage for X social-source revisions.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::sync::Arc;

use ratatoskr_event_envelope::CommandEnvelope;
use tokio::sync::Barrier;
use x_persistence::test_support::TestDatabase;
use x_sync::{ExplicitCaptureService, KnowledgeAnalysisAdmission, KnowledgeAnalysisService};

fn capture_command(owner: uuid::Uuid, suffix: &str) -> CommandEnvelope {
    serde_json::from_value(serde_json::json!({
        "command_id": format!("018f0000-0000-7000-8000-0000000007{suffix}"),
        "command_type": "social.capture.requested.v1",
        "issued_at": "2026-08-27T12:00:00Z",
        "producer": "ratatoskr-platform",
        "aggregate_id": "x-post:123456789",
        "correlation_id": format!("operation:018f0000-0000-7000-8000-0000000008{suffix}"),
        "tenant_id": format!("user:{owner}"),
        "schema_version": 1,
        "payload": {
            "operation_id": format!("018f0000-0000-7000-8000-0000000008{suffix}"),
            "idempotency_key": {
                "algorithm": "sha256",
                "hex": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            },
            "original_permalink": "https://x.com/author/status/123456789",
            "captured_at": "2026-08-27T12:00:00Z",
            "provider": "x",
            "acquisition": "browser_extension",
            "saved_authority": "explicit_user_capture"
        }
    }))
    .expect("the capture command decodes")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_identical_observations_emit_one_knowledge_request() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("concurrent-knowledge-request-owner")
        .await
        .expect("the account seeds");
    let owner: uuid::Uuid =
        sqlx::query_scalar("select internal_user_id from x_archive.accounts where id = $1")
            .bind(account)
            .fetch_one(test.database.pool())
            .await
            .expect("the owner is readable");
    let author: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.users (provider_id, parser_version) \
         values ('knowledge-request-author', 1) returning id",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("the author seeds");
    sqlx::query(
        "insert into x_archive.posts (provider_id, author_user_id, text, parser_version) \
         values ('123456789', $1, 'One revision is one request.', 1)",
    )
    .bind(author)
    .execute(test.database.pool())
    .await
    .expect("the post seeds");
    sqlx::raw_sql(
        "create function x_archive.delay_social_source_insert() returns trigger \
         language plpgsql as $$ begin perform pg_sleep(0.2); return new; end $$; \
         create trigger delay_social_source_insert before insert on x_archive.social_sources \
         for each row execute function x_archive.delay_social_source_insert();",
    )
    .execute(test.database.pool())
    .await
    .expect("the deterministic race trigger installs");

    let barrier = Arc::new(Barrier::new(2));
    let first_barrier = Arc::clone(&barrier);
    let first_service = ExplicitCaptureService::new(test.database.clone());
    let first = tokio::spawn(async move {
        first_barrier.wait().await;
        first_service
            .apply(account, capture_command(owner, "01"))
            .await
    });
    let second_service = ExplicitCaptureService::new(test.database.clone());
    let second = tokio::spawn(async move {
        barrier.wait().await;
        second_service
            .apply(account, capture_command(owner, "02"))
            .await
    });
    let (first, second) = tokio::join!(first, second);
    let first = first.expect("the first capture task does not panic");
    let second = second.expect("the second capture task does not panic");

    assert!(
        first.is_ok() && second.is_ok(),
        "concurrent identical observations must converge: first={first:?}, second={second:?}"
    );
    let revisions: i64 =
        sqlx::query_scalar("select count(*) from x_archive.social_source_revisions")
            .fetch_one(test.database.pool())
            .await
            .expect("revision count is readable");
    let requests: i64 = sqlx::query_scalar(
        "select count(*) from x_archive.outbox_events \
         where event_type in ('social.source.captured.v1', 'social.source.updated.v1')",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("request count is readable");

    test.cleanup().await.expect("cleanup drops the database");

    assert_eq!(revisions, 1, "one source digest is retained once");
    assert_eq!(requests, 1, "one source digest requests Knowledge once");
}

#[tokio::test]
async fn completion_redelivery_links_exact_revision_once() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("knowledge-completion-owner")
        .await
        .expect("the account seeds");
    let owner: uuid::Uuid =
        sqlx::query_scalar("select internal_user_id from x_archive.accounts where id = $1")
            .bind(account)
            .fetch_one(test.database.pool())
            .await
            .expect("the owner is readable");
    let author: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.users (provider_id, parser_version) \
         values ('knowledge-completion-author', 1) returning id",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("the author seeds");
    sqlx::query(
        "insert into x_archive.posts (provider_id, author_user_id, text, parser_version) \
         values ('123456789', $1, 'Link this exact revision.', 1)",
    )
    .bind(author)
    .execute(test.database.pool())
    .await
    .expect("the post seeds");
    ExplicitCaptureService::new(test.database.clone())
        .apply(account, capture_command(owner, "03"))
        .await
        .expect("the source revision is captured");
    let (source_id, digest): (uuid::Uuid, serde_json::Value) = sqlx::query_as(
        "select social_source_id, current_content_digest from x_archive.social_sources \
         where account_id = $1",
    )
    .bind(account)
    .fetch_one(test.database.pool())
    .await
    .expect("the source revision is readable");
    let completion = serde_json::json!({
        "event_id": "018f0000-0000-7000-8000-000000000901",
        "event_type": "knowledge.analysis.completed.v1",
        "occurred_at": "2026-08-27T12:05:00Z",
        "producer": "ratatoskr-knowledge",
        "aggregate_id": format!("social_source:{source_id}"),
        "correlation_id": "operation:018f0000-0000-7000-8000-000000000902",
        "tenant_id": format!("user:{owner}"),
        "schema_version": 1,
        "payload": {
            "owner": format!("user:{owner}"),
            "social_source_id": source_id,
            "content_digest": digest,
            "completed_at": "2026-08-27T12:05:00Z"
        }
    })
    .to_string();
    let service = KnowledgeAnalysisService::new(test.database.clone());

    let first = service
        .consume_completion(&completion)
        .await
        .expect("the valid completion links");
    let second = service
        .consume_completion(&completion)
        .await
        .expect("the completion redelivery is accepted");
    let links: i64 = sqlx::query_scalar("select count(*) from x_archive.knowledge_analysis_links")
        .fetch_one(test.database.pool())
        .await
        .expect("link count is readable");
    let receipts: i64 =
        sqlx::query_scalar("select count(*) from x_archive.inbox_events where event_type = $1")
            .bind("knowledge.analysis.completed.v1")
            .fetch_one(test.database.pool())
            .await
            .expect("receipt count is readable");

    test.cleanup().await.expect("cleanup drops the database");

    assert_eq!(first, KnowledgeAnalysisAdmission::LinkedCurrent);
    assert_eq!(second, KnowledgeAnalysisAdmission::Duplicate);
    assert_eq!(links, 1, "the exact source revision links once");
    assert_eq!(receipts, 1, "the event is claimed once");
}

#[tokio::test]
async fn older_digest_completion_is_historical_not_current() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("historical-knowledge-completion-owner")
        .await
        .expect("the account seeds");
    let owner: uuid::Uuid =
        sqlx::query_scalar("select internal_user_id from x_archive.accounts where id = $1")
            .bind(account)
            .fetch_one(test.database.pool())
            .await
            .expect("the owner is readable");
    let author: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.users (provider_id, parser_version) \
         values ('historical-completion-author', 1) returning id",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("the author seeds");
    let post_id: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.posts (provider_id, author_user_id, text, parser_version) \
         values ('123456789', $1, 'First retained revision.', 1) returning id",
    )
    .bind(author)
    .fetch_one(test.database.pool())
    .await
    .expect("the post seeds");
    let capture = ExplicitCaptureService::new(test.database.clone());
    capture
        .apply(account, capture_command(owner, "04"))
        .await
        .expect("the first revision is captured");
    let (source_id, older_digest): (uuid::Uuid, serde_json::Value) = sqlx::query_as(
        "select social_source_id, current_content_digest from x_archive.social_sources \
         where account_id = $1",
    )
    .bind(account)
    .fetch_one(test.database.pool())
    .await
    .expect("the first revision is readable");
    sqlx::query("update x_archive.posts set text = 'Second retained revision.' where id = $1")
        .bind(post_id)
        .execute(test.database.pool())
        .await
        .expect("the normalized post changes");
    capture
        .apply(account, capture_command(owner, "05"))
        .await
        .expect("the second revision is captured");
    let current_digest: serde_json::Value = sqlx::query_scalar(
        "select current_content_digest from x_archive.social_sources where social_source_id = $1",
    )
    .bind(source_id)
    .fetch_one(test.database.pool())
    .await
    .expect("the current revision is readable");
    assert_ne!(older_digest, current_digest, "the source revision changed");
    let completion = serde_json::json!({
        "event_id": "018f0000-0000-7000-8000-000000000903",
        "event_type": "knowledge.analysis.completed.v1",
        "occurred_at": "2026-08-27T12:10:00Z",
        "producer": "ratatoskr-knowledge",
        "aggregate_id": format!("social_source:{source_id}"),
        "correlation_id": "operation:018f0000-0000-7000-8000-000000000904",
        "tenant_id": format!("user:{owner}"),
        "schema_version": 1,
        "payload": {
            "owner": format!("user:{owner}"),
            "social_source_id": source_id,
            "content_digest": older_digest,
            "completed_at": "2026-08-27T12:10:00Z"
        }
    })
    .to_string();

    let admission = KnowledgeAnalysisService::new(test.database.clone())
        .consume_completion(&completion)
        .await
        .expect("a retained historical revision remains linkable");
    let links: i64 = sqlx::query_scalar("select count(*) from x_archive.knowledge_analysis_links")
        .fetch_one(test.database.pool())
        .await
        .expect("link count is readable");

    test.cleanup().await.expect("cleanup drops the database");

    assert_eq!(admission, KnowledgeAnalysisAdmission::LinkedHistorical);
    assert_eq!(links, 1, "the historical revision keeps its own link");
}
