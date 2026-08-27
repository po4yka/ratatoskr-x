//! `SocialSource` publication from authoritative X bookmark observations.

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
use ratatoskr_event_envelope::EventPayload;
use ratatoskr_social_contracts::SocialSourceCaptured;
use x_budget::gate::{BudgetClass, BudgetGate, Clock};
use x_persistence::test_support::TestDatabase;
use x_sync::{BookmarkPage, BookmarkPageSource, BookmarkSnapshotService, BookmarkSourceError};

#[derive(Debug)]
struct FakeClock {
    instant: DateTime<Utc>,
}

impl Clock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        self.instant
    }
}

#[derive(Debug)]
struct FakeBookmarkPageSource {
    pages: Mutex<VecDeque<Result<BookmarkPage, BookmarkSourceError>>>,
}

impl FakeBookmarkPageSource {
    fn one(page: BookmarkPage) -> Self {
        Self {
            pages: Mutex::new(vec![Ok(page)].into()),
        }
    }
}

impl BookmarkPageSource for FakeBookmarkPageSource {
    fn fetch_page<'a>(
        &'a self,
        _continuation: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<BookmarkPage, BookmarkSourceError>> + Send + 'a>> {
        let page = self
            .pages
            .lock()
            .expect("pages mutex is not poisoned")
            .pop_front()
            .expect("the harness supplies one response");
        Box::pin(async move { page })
    }
}

fn service(database: x_persistence::database::Database) -> BookmarkSnapshotService {
    let instant = "2026-08-20T12:00:00Z"
        .parse()
        .expect("the fixture instant parses");
    let clock: Arc<dyn Clock> = Arc::new(FakeClock { instant });
    let budget = BudgetGate::with_clock(
        database.clone(),
        BudgetClass::Read,
        10,
        3_600,
        Arc::clone(&clock),
    )
    .expect("the budget gate accepts the fixture configuration");
    BookmarkSnapshotService::new(database, budget, clock)
}

fn bookmark_page() -> BookmarkPage {
    bookmark_page_with_text("First line.\\nSecond line.")
}

fn bookmark_page_with_text(text: &str) -> BookmarkPage {
    let payload = serde_json::json!({
        "data": [{
            "id": "1234567890123456789",
            "text": text,
            "author_id": "987654321",
            "created_at": "2026-08-16T09:30:00Z"
        }],
        "includes": {"users": [{
            "id": "987654321",
            "name": "Example User",
            "username": "example_user"
        }]}
    })
    .to_string();
    BookmarkPage::new(
        serde_json::from_str(&payload).expect("the official provider fixture decodes"),
        None,
    )
}

#[tokio::test]
async fn unchanged_observation_is_silent_and_material_change_emits_one_updated_event() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("social-source-update-account")
        .await
        .expect("the account seeds");

    service(test.database.clone())
        .run(account, &FakeBookmarkPageSource::one(bookmark_page()), None)
        .await
        .expect("the initial bookmark snapshot completes");
    service(test.database.clone())
        .run(account, &FakeBookmarkPageSource::one(bookmark_page()), None)
        .await
        .expect("the unchanged bookmark snapshot completes");
    service(test.database.clone())
        .run(
            account,
            &FakeBookmarkPageSource::one(bookmark_page_with_text("Edited normalized post")),
            None,
        )
        .await
        .expect("the changed bookmark snapshot completes");

    let event_types: Vec<String> = sqlx::query_scalar(
        "select event_type from x_archive.outbox_events order by created_at, id",
    )
    .fetch_all(test.database.pool())
    .await
    .expect("the outbox is readable");
    test.cleanup().await.expect("cleanup drops the database");

    assert_eq!(
        event_types,
        vec![
            SocialSourceCaptured::EVENT_TYPE.to_owned(),
            "social.source.updated.v1".to_owned(),
        ],
        "unchanged observations stay silent while one semantic change emits one update"
    );
}

#[tokio::test]
async fn bookmark_capture_serializes_the_pinned_social_contract_fixture() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("social-source-bookmark-account")
        .await
        .expect("the account seeds");
    let source = FakeBookmarkPageSource::one(bookmark_page());

    service(test.database.clone())
        .run(account, &source, None)
        .await
        .expect("the bookmark snapshot completes");

    let payload: Option<serde_json::Value> = sqlx::query_scalar(
        "select payload from x_archive.outbox_events \
         where event_type = $1 order by created_at limit 1",
    )
    .bind(SocialSourceCaptured::EVENT_TYPE)
    .fetch_optional(test.database.pool())
    .await
    .expect("the outbox is readable");

    test.cleanup().await.expect("cleanup drops the database");

    let payload = payload.expect("a bookmark capture emits the SocialSource captured event");
    let event: SocialSourceCaptured =
        serde_json::from_value(payload).expect("the outbox payload matches the pinned contract");
    let source = serde_json::to_value(event.source).expect("the source serializes");
    assert_eq!(source["platform"], "x");
    assert_eq!(source["external_post_id"], "1234567890123456789");
    assert_eq!(source["acquisition"], "official_api");
    assert_eq!(source["saved_authority"], "authoritative_platform_state");
}
