//! Knowledge request and completion linkage for X social-source revisions.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::sync::Arc;

use tokio::sync::Barrier;
use x_persistence::test_support::TestDatabase;
use x_sync::{KnowledgeAnalysisAdmission, KnowledgeAnalysisService};

mod support;

use support::{OnePost, snapshot_service};

fn post(text: &str) -> OnePost {
    OnePost {
        provider_id: "123456789".to_owned(),
        author_id: "knowledge-author".to_owned(),
        text: text.to_owned(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_identical_observations_emit_one_knowledge_request() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("concurrent-knowledge-request-owner")
        .await
        .expect("the account seeds");
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
    let mut runs = Vec::new();
    for _ in 0..2 {
        let barrier = Arc::clone(&barrier);
        let service = snapshot_service(test.database.clone());
        let source = post("One revision is one request.");
        runs.push(tokio::spawn(async move {
            barrier.wait().await;
            service.run(account, &source, None).await
        }));
    }
    let mut completed = 0_u32;
    for run in runs {
        let outcome = run.await.expect("the snapshot task does not panic");
        // Two overlapping full snapshots may legitimately lose the serializable finalization
        // race; the publication invariant below is what this test is about.
        completed += u32::from(outcome.is_ok());
    }
    assert!(completed >= 1, "at least one snapshot run completes");
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
    snapshot_service(test.database.clone())
        .run(account, &post("Link this exact revision."), None)
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
    snapshot_service(test.database.clone())
        .run(account, &post("First retained revision."), None)
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
    snapshot_service(test.database.clone())
        .run(account, &post("Second retained revision."), None)
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
