//! Every `SocialSource` outbox row is the complete canonical event envelope (XR-021 CONTRACTS.md
//! S02 rule 1): the bus relay publishes the stored JSON unchanged, so the row id must be the
//! envelope's event id and the producer, tenant and aggregate must already be on the document.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use ratatoskr_event_envelope::{EventEnvelope, EventPayload};
use ratatoskr_social_contracts::SocialSourceCaptured;
use x_budget::gate::{BudgetClass, BudgetGate, Clock};
use x_persistence::test_support::TestDatabase;
use x_sync::{BookmarkPage, BookmarkPageSource, BookmarkSnapshotService, BookmarkSourceError};

#[derive(Debug)]
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        "2026-08-20T12:00:00Z"
            .parse()
            .expect("the fixture instant parses")
    }
}

#[derive(Debug)]
struct OnePage;

impl BookmarkPageSource for OnePage {
    fn fetch_page<'a>(
        &'a self,
        _continuation: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<BookmarkPage, BookmarkSourceError>> + Send + 'a>> {
        let payload = serde_json::json!({
            "data": [{
                "id": "1234567890123456789",
                "text": "A bookmarked post.",
                "author_id": "987654321",
                "created_at": "2026-08-16T09:30:00.120Z"
            }],
            "includes": {"users": [{
                "id": "987654321",
                "name": "Example User",
                "username": "example_user"
            }]}
        });
        let envelope = serde_json::from_value(payload).expect("the fixture decodes");
        Box::pin(async move { Ok(BookmarkPage::new(envelope, None)) })
    }
}

#[tokio::test]
async fn outbox_row_is_a_complete_event_envelope() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("social-source-envelope-owner")
        .await
        .expect("the account seeds");
    let owner: uuid::Uuid =
        sqlx::query_scalar("select internal_user_id from x_archive.accounts where id = $1")
            .bind(account)
            .fetch_one(test.database.pool())
            .await
            .expect("the owner is readable");
    let clock: Arc<dyn Clock> = Arc::new(FixedClock);
    let budget = BudgetGate::with_clock(
        test.database.clone(),
        BudgetClass::Read,
        10,
        3_600,
        Arc::clone(&clock),
    )
    .expect("the budget configuration is valid");
    BookmarkSnapshotService::new(test.database.clone(), budget, clock)
        .run(account, &OnePage, None)
        .await
        .expect("the bookmark snapshot completes");

    let row: (uuid::Uuid, String, serde_json::Value) = sqlx::query_as(
        "select id, aggregate, payload from x_archive.outbox_events where event_type = $1",
    )
    .bind(SocialSourceCaptured::EVENT_TYPE)
    .fetch_one(test.database.pool())
    .await
    .expect("one captured row is stored");
    test.cleanup().await.expect("cleanup drops the database");

    let (row_id, aggregate, payload) = row;
    let envelope = EventEnvelope::from_json(payload.to_string().as_bytes())
        .expect("the stored payload is a complete event envelope");
    assert_eq!(envelope.event_id.0, row_id, "the row id is the event id");
    assert_eq!(envelope.producer.as_str(), "ratatoskr-x");
    assert_eq!(
        envelope.tenant_id.map(|tenant| tenant.to_string()),
        Some(format!("user:{owner}")),
        "the envelope names the owner"
    );
    assert_eq!(envelope.aggregate_id.to_wire(), aggregate);
    assert!(aggregate.starts_with("social_source:"));
    let captured: SocialSourceCaptured = envelope
        .payload_as()
        .expect("the envelope payload is the captured contract");
    let source = serde_json::to_value(captured.source).expect("the source serializes");
    assert_eq!(
        source["published_at"], "2026-08-16T09:30:00.12Z",
        "timestamps use the canonical wire form, trailing zeros trimmed"
    );
}
