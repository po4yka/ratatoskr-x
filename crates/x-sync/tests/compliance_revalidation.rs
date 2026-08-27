//! Upstream authorization revalidation, ledger evidence, and takedown propagation.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use ratatoskr_event_envelope::CommandEnvelope;
use ratatoskr_social_contracts::{RemovalReason, SocialSourceRemoved};
use x_budget::gate::{BudgetClass, BudgetGate, Clock};
use x_persistence::test_support::TestDatabase;
use x_sync::{
    ComplianceAvailability, ComplianceObservation, ComplianceRevalidationService,
    ComplianceRevalidationSource, ComplianceSourceError, ExplicitCaptureService,
    KnowledgeAnalysisService, KnowledgeIntegrationError,
};

#[derive(Debug)]
struct FixedClock(DateTime<Utc>);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

#[derive(Debug)]
struct FakeComplianceSource {
    outcomes: Mutex<VecDeque<Result<ComplianceObservation, ComplianceSourceError>>>,
    calls: Mutex<Vec<String>>,
}

impl FakeComplianceSource {
    fn one(outcome: Result<ComplianceObservation, ComplianceSourceError>) -> Self {
        Self::many([outcome])
    }

    fn many(
        outcomes: impl IntoIterator<Item = Result<ComplianceObservation, ComplianceSourceError>>,
    ) -> Self {
        Self {
            outcomes: Mutex::new(outcomes.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl ComplianceRevalidationSource for FakeComplianceSource {
    fn revalidate<'a>(
        &'a self,
        provider_post_id: &'a str,
    ) -> Pin<
        Box<dyn Future<Output = Result<ComplianceObservation, ComplianceSourceError>> + Send + 'a>,
    > {
        self.calls
            .lock()
            .expect("calls mutex is not poisoned")
            .push(provider_post_id.to_owned());
        let outcome = self
            .outcomes
            .lock()
            .expect("outcomes mutex is not poisoned")
            .pop_front()
            .expect("the fake supplies one outcome");
        Box::pin(async move { outcome })
    }
}

fn capture_command(owner: uuid::Uuid, provider_post_id: &str, marker: u16) -> CommandEnvelope {
    serde_json::from_value(serde_json::json!({
        "command_id": format!("018f0000-0000-7000-8000-{marker:012x}"),
        "command_type": "social.capture.requested.v1",
        "issued_at": "2026-08-27T12:00:00Z",
        "producer": "ratatoskr-platform",
        "aggregate_id": format!("x-post:{provider_post_id}"),
        "correlation_id": format!("operation:018f0000-0000-7000-8001-{marker:012x}"),
        "tenant_id": format!("user:{owner}"),
        "schema_version": 1,
        "payload": {
            "operation_id": format!("018f0000-0000-7000-8001-{marker:012x}"),
            "idempotency_key": {
                "algorithm": "sha256",
                "hex": "1123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            },
            "original_permalink": format!("https://x.com/author/status/{provider_post_id}"),
            "captured_at": "2026-08-27T12:00:00Z",
            "provider": "x",
            "acquisition": "browser_extension",
            "saved_authority": "explicit_user_capture"
        }
    }))
    .expect("the capture command decodes")
}

async fn seed_source(test: &TestDatabase) -> (uuid::Uuid, uuid::Uuid) {
    let account = test
        .seed_account("compliance-revalidation-owner")
        .await
        .expect("the account seeds");
    let owner: uuid::Uuid =
        sqlx::query_scalar("select internal_user_id from x_archive.accounts where id = $1")
            .bind(account)
            .fetch_one(test.database.pool())
            .await
            .expect("the owner is readable");
    seed_post_source(test, account, owner, "223456789", 0x711).await;
    (account, owner)
}

async fn seed_post_source(
    test: &TestDatabase,
    account: uuid::Uuid,
    owner: uuid::Uuid,
    provider_post_id: &str,
    marker: u16,
) {
    let author: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.users (provider_id, parser_version) \
         values ($1, 1) returning id",
    )
    .bind(format!("compliance-author-{marker}"))
    .fetch_one(test.database.pool())
    .await
    .expect("the author seeds");
    sqlx::query(
        "insert into x_archive.posts (provider_id, author_user_id, text, parser_version) \
         values ($1, $2, 'Revalidate this source.', 1)",
    )
    .bind(provider_post_id)
    .bind(author)
    .execute(test.database.pool())
    .await
    .expect("the post seeds");
    ExplicitCaptureService::new(test.database.clone())
        .apply(account, capture_command(owner, provider_post_id, marker))
        .await
        .expect("the source is captured");
}

fn service(database: x_persistence::database::Database) -> ComplianceRevalidationService {
    let clock: Arc<dyn Clock> = Arc::new(FixedClock(
        "2026-08-27T12:30:00Z"
            .parse()
            .expect("the fixture instant parses"),
    ));
    let budget = BudgetGate::with_clock(
        database.clone(),
        BudgetClass::Read,
        10,
        3_600,
        Arc::clone(&clock),
    )
    .expect("the budget configuration is valid");
    ComplianceRevalidationService::new(database, budget, clock)
}

fn completion_event(
    owner: uuid::Uuid,
    source_id: uuid::Uuid,
    content_digest: &serde_json::Value,
) -> String {
    serde_json::json!({
        "event_id": "018f0000-0000-7000-8002-000000000714",
        "event_type": "knowledge.analysis.completed.v1",
        "occurred_at": "2026-08-27T12:45:00Z",
        "producer": "ratatoskr-knowledge",
        "aggregate_id": format!("social_source:{source_id}"),
        "correlation_id": "operation:018f0000-0000-7000-8003-000000000714",
        "tenant_id": format!("user:{owner}"),
        "schema_version": 1,
        "payload": {
            "owner": format!("user:{owner}"),
            "social_source_id": source_id,
            "content_digest": content_digest,
            "completed_at": "2026-08-27T12:45:00Z"
        }
    })
    .to_string()
}

async fn derived_state_counts(test: &TestDatabase) -> (i64, i64, i64, i64, i64) {
    let queries = [
        "select count(*) from x_archive.outbox_events \
         where event_type in ('social.source.captured.v1', 'social.source.updated.v1')",
        "select count(*) from x_archive.social_source_revisions",
        "select count(*) from x_archive.knowledge_analysis_links",
        "select count(*) from x_archive.tombstones",
        "select count(*) from x_archive.outbox_events \
         where event_type = 'social.source.removed.v1'",
    ];
    let mut counts = [0; 5];
    for (count, query) in counts.iter_mut().zip(queries) {
        *count = sqlx::query_scalar(query)
            .fetch_one(test.database.pool())
            .await
            .expect("derived state count is readable");
    }
    let [
        analysis_requests,
        revisions,
        links,
        tombstones,
        removal_events,
    ] = counts;
    (
        analysis_requests,
        revisions,
        links,
        tombstones,
        removal_events,
    )
}

#[tokio::test]
async fn available_due_source_records_one_revalidation_ledger_entry() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let (account, _) = seed_source(&test).await;
    let source = FakeComplianceSource::one(Ok(ComplianceObservation {
        availability: ComplianceAvailability::Available,
        provider_request_id: Some("x-request-available-1".to_owned()),
    }));

    let summary = service(test.database.clone())
        .run_due(
            account,
            &source,
            "2026-08-27T12:00:00Z"
                .parse()
                .expect("the due instant parses"),
            1,
        )
        .await
        .expect("the available observation is processed");
    let ledger: Option<(String, Option<String>)> = sqlx::query_as(
        "select outcome, provider_request_id from x_archive.compliance_revalidation_ledger",
    )
    .fetch_optional(test.database.pool())
    .await
    .expect("the ledger is readable");
    let calls = source
        .calls
        .lock()
        .expect("calls mutex is not poisoned")
        .clone();

    test.cleanup().await.expect("cleanup drops the database");

    assert_eq!(
        ledger,
        Some((
            "available".to_owned(),
            Some("x-request-available-1".to_owned())
        ))
    );
    assert_eq!(calls, vec!["223456789".to_owned()]);
    assert_eq!(summary.checked, 1);
    assert_eq!(summary.takedowns, 0);
    assert!(!summary.budget_exhausted);
}

#[tokio::test]
async fn recent_and_excess_sources_are_not_checked() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let (account, owner) = seed_source(&test).await;
    seed_post_source(&test, account, owner, "223456790", 0x712).await;
    seed_post_source(&test, account, owner, "223456791", 0x713).await;
    let recent: (uuid::Uuid, uuid::Uuid) = sqlx::query_as(
        "select source.social_source_id, source.post_id \
         from x_archive.social_sources source \
         join x_archive.posts post on post.id = source.post_id \
         where source.account_id = $1 and post.provider_id = '223456789'",
    )
    .bind(account)
    .fetch_one(test.database.pool())
    .await
    .expect("the recent source is readable");
    sqlx::query(
        "insert into x_archive.compliance_revalidation_ledger \
           (account_id, post_id, social_source_id, outcome, checked_at) \
         values ($1, $2, $3, 'available', '2026-08-27T12:15:00Z')",
    )
    .bind(account)
    .bind(recent.1)
    .bind(recent.0)
    .execute(test.database.pool())
    .await
    .expect("the recent ledger entry seeds");
    let source = FakeComplianceSource::many([
        Ok(ComplianceObservation {
            availability: ComplianceAvailability::Available,
            provider_request_id: Some("x-request-due-1".to_owned()),
        }),
        Ok(ComplianceObservation {
            availability: ComplianceAvailability::Available,
            provider_request_id: Some("x-request-excess".to_owned()),
        }),
    ]);

    let summary = service(test.database.clone())
        .run_due(
            account,
            &source,
            "2026-08-27T12:00:00Z"
                .parse()
                .expect("the due instant parses"),
            1,
        )
        .await
        .expect("the bounded due run succeeds");
    let calls = source
        .calls
        .lock()
        .expect("calls mutex is not poisoned")
        .clone();

    test.cleanup().await.expect("cleanup drops the database");

    assert_eq!(calls, vec!["223456790".to_owned()]);
    assert_eq!(summary.checked, 1);
}

#[tokio::test]
async fn indeterminate_failure_is_ledgered_without_takedown() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let (account, _) = seed_source(&test).await;
    let source = FakeComplianceSource::one(Err(ComplianceSourceError::RateLimited));

    let summary = service(test.database.clone())
        .run_due(
            account,
            &source,
            "2026-08-27T12:00:00Z"
                .parse()
                .expect("the due instant parses"),
            1,
        )
        .await
        .expect("an indeterminate provider failure is recorded, not escaped");
    let ledger: Option<(String, Option<String>)> = sqlx::query_as(
        "select outcome, failure_class from x_archive.compliance_revalidation_ledger",
    )
    .fetch_optional(test.database.pool())
    .await
    .expect("the ledger is readable");
    let removed_sources: i64 = sqlx::query_scalar(
        "select count(*) from x_archive.social_sources where removed_at is not null",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("source state is readable");
    let tombstones: i64 = sqlx::query_scalar("select count(*) from x_archive.tombstones")
        .fetch_one(test.database.pool())
        .await
        .expect("tombstones are readable");
    let removal_events: i64 = sqlx::query_scalar(
        "select count(*) from x_archive.outbox_events where event_type = 'social.source.removed.v1'",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("the outbox is readable");

    test.cleanup().await.expect("cleanup drops the database");

    assert_eq!(
        ledger,
        Some(("indeterminate".to_owned(), Some("rate_limited".to_owned())))
    );
    assert_eq!(removed_sources, 0);
    assert_eq!(tombstones, 0);
    assert_eq!(removal_events, 0);
    assert_eq!(summary.checked, 1);
    assert_eq!(summary.takedowns, 0);
}

#[tokio::test]
async fn authoritative_deletion_records_tombstone_and_one_knowledge_deletion_request() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let (account, owner) = seed_source(&test).await;
    let source_id: uuid::Uuid = sqlx::query_scalar(
        "select social_source_id from x_archive.social_sources where account_id = $1",
    )
    .bind(account)
    .fetch_one(test.database.pool())
    .await
    .expect("the source identity is readable");
    let source = FakeComplianceSource::one(Ok(ComplianceObservation {
        availability: ComplianceAvailability::Deleted,
        provider_request_id: Some("x-request-deleted-1".to_owned()),
    }));

    let summary = service(test.database.clone())
        .run_due(
            account,
            &source,
            "2026-08-27T12:00:00Z"
                .parse()
                .expect("the due instant parses"),
            1,
        )
        .await
        .expect("the authoritative takedown is processed");
    let ledger: (String, Option<String>) = sqlx::query_as(
        "select outcome, failure_class from x_archive.compliance_revalidation_ledger",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("the ledger is readable");
    let source_state: (Option<DateTime<Utc>>, Option<String>) = sqlx::query_as(
        "select removed_at, removal_reason from x_archive.social_sources where account_id = $1",
    )
    .bind(account)
    .fetch_one(test.database.pool())
    .await
    .expect("source state is readable");
    let post_availability: String = sqlx::query_scalar(
        "select post.availability from x_archive.posts post \
         join x_archive.social_sources source on source.post_id = post.id \
         where source.account_id = $1",
    )
    .bind(account)
    .fetch_one(test.database.pool())
    .await
    .expect("post availability is readable");
    let tombstone: Option<(String, String)> = sqlx::query_as(
        "select post_provider_id, reason from x_archive.tombstones where account_id = $1",
    )
    .bind(account)
    .fetch_optional(test.database.pool())
    .await
    .expect("the tombstone is readable");
    let removal_payloads: Vec<serde_json::Value> = sqlx::query_scalar(
        "select payload from x_archive.outbox_events \
         where event_type = 'social.source.removed.v1'",
    )
    .fetch_all(test.database.pool())
    .await
    .expect("removal requests are readable");
    let removal: SocialSourceRemoved = serde_json::from_value(
        removal_payloads
            .first()
            .cloned()
            .expect("one deletion request is emitted"),
    )
    .expect("the deletion request uses the shared contract");

    test.cleanup().await.expect("cleanup drops the database");

    assert_eq!(ledger, ("deleted".to_owned(), None));
    assert!(source_state.0.is_some());
    assert_eq!(source_state.1.as_deref(), Some("retention_policy"));
    assert_eq!(post_availability, "deleted");
    assert_eq!(
        tombstone,
        Some(("223456789".to_owned(), "deleted".to_owned()))
    );
    assert_eq!(removal_payloads.len(), 1);
    assert_eq!(removal.social_source_id.0, source_id);
    assert_eq!(removal.owner.to_string(), format!("user:{owner}"));
    assert_eq!(removal.reason, RemovalReason::RetentionPolicy);
    assert_eq!(summary.checked, 1);
    assert_eq!(summary.takedowns, 1);
}

#[tokio::test]
async fn repeated_takedown_and_delayed_work_cannot_resurrect_source() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let (account, owner) = seed_source(&test).await;
    let (source_id, original_digest, post_id): (uuid::Uuid, serde_json::Value, uuid::Uuid) =
        sqlx::query_as(
            "select social_source_id, current_content_digest, post_id \
             from x_archive.social_sources where account_id = $1",
        )
        .bind(account)
        .fetch_one(test.database.pool())
        .await
        .expect("the retained source is readable");
    let deleted = FakeComplianceSource::one(Ok(ComplianceObservation {
        availability: ComplianceAvailability::Deleted,
        provider_request_id: Some("x-request-deleted-first".to_owned()),
    }));
    service(test.database.clone())
        .run_due(
            account,
            &deleted,
            "2026-08-27T12:00:00Z"
                .parse()
                .expect("the due instant parses"),
            1,
        )
        .await
        .expect("the first takedown succeeds");
    let repeated = FakeComplianceSource::one(Ok(ComplianceObservation {
        availability: ComplianceAvailability::Deleted,
        provider_request_id: Some("x-request-deleted-repeated".to_owned()),
    }));
    let repeated_summary = service(test.database.clone())
        .run_due(
            account,
            &repeated,
            "2026-08-27T13:00:00Z"
                .parse()
                .expect("the later due instant parses"),
            1,
        )
        .await
        .expect("a repeated schedule is idempotent");

    sqlx::query("update x_archive.posts set text = 'Delayed stale source body.' where id = $1")
        .bind(post_id)
        .execute(test.database.pool())
        .await
        .expect("the delayed normalized observation seeds");
    ExplicitCaptureService::new(test.database.clone())
        .apply(account, capture_command(owner, "223456789", 0x714))
        .await
        .expect("the delayed ordinary observation is safely consumed");
    let completion = completion_event(owner, source_id, &original_digest);
    let completion_result = KnowledgeAnalysisService::new(test.database.clone())
        .consume_completion(&completion)
        .await;
    let (analysis_requests, revisions, links, tombstones, removal_events) =
        derived_state_counts(&test).await;
    let repeated_calls = repeated
        .calls
        .lock()
        .expect("calls mutex is not poisoned")
        .clone();

    test.cleanup().await.expect("cleanup drops the database");

    assert!(matches!(
        completion_result,
        Err(KnowledgeIntegrationError::InvalidCompletion)
    ));
    assert_eq!(repeated_calls.len(), 0);
    assert_eq!(repeated_summary.checked, 0);
    assert_eq!(
        analysis_requests, 1,
        "no delayed analysis request is emitted"
    );
    assert_eq!(revisions, 1, "the removed source head is not revised");
    assert_eq!(links, 0, "the delayed completion is not linked");
    assert_eq!(tombstones, 1);
    assert_eq!(removal_events, 1);
}
