//! Safe incremental bookmark scans against a disposable `PostgreSQL` database.

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
use x_budget::gate::{BudgetClass, BudgetGate, Clock};
use x_persistence::test_support::TestDatabase;
use x_sync::{
    BookmarkPage, BookmarkPageSource, BookmarkSourceError, IncrementalOutcome,
    IncrementalScanService, SCHEDULED_BOOKMARK_SCAN_COMMAND, ScheduledScan,
};

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
    calls: Mutex<Vec<Option<String>>>,
    pages: Mutex<VecDeque<Result<BookmarkPage, BookmarkSourceError>>>,
}

impl FakeBookmarkPageSource {
    fn new(pages: Vec<Result<BookmarkPage, BookmarkSourceError>>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            pages: Mutex::new(pages.into()),
        }
    }

    fn calls(&self) -> Vec<Option<String>> {
        self.calls
            .lock()
            .expect("calls mutex is not poisoned")
            .clone()
    }
}

impl BookmarkPageSource for FakeBookmarkPageSource {
    fn fetch_page<'a>(
        &'a self,
        continuation: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<BookmarkPage, BookmarkSourceError>> + Send + 'a>> {
        self.calls
            .lock()
            .expect("calls mutex is not poisoned")
            .push(continuation.map(ToOwned::to_owned));
        let page = self
            .pages
            .lock()
            .expect("pages mutex is not poisoned")
            .pop_front()
            .expect("the harness supplied a response for every request");
        Box::pin(async move { page })
    }
}

fn page(post_id: &str, next_token: Option<&str>) -> BookmarkPage {
    let json = format!(
        r#"{{"data":[{{"id":"{post_id}","text":"bookmark {post_id}","author_id":"author-1","created_at":"2026-08-01T12:00:00Z"}}],"includes":{{"users":[{{"id":"author-1","name":"Author","username":"author"}}]}}}}"#
    );
    let envelope = serde_json::from_str(&json).expect("the synthetic official payload decodes");
    BookmarkPage::new(envelope, next_token.map(ToOwned::to_owned))
}

fn service(database: x_persistence::database::Database) -> IncrementalScanService {
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
    IncrementalScanService::new(database, budget, clock, 3, 3)
}

#[tokio::test]
async fn advances_watermark_only_after_reaching_prior_watermark() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("incremental-watermark-account")
        .await
        .expect("the account seeds");
    sqlx::query(
        "insert into x_archive.bookmark_incremental_state (account_id, watermark_provider_post_id) \
         values ($1, 'old-watermark')",
    )
    .bind(account)
    .execute(test.database.pool())
    .await
    .expect("the prior watermark seeds");
    let source = FakeBookmarkPageSource::new(vec![
        Ok(page("new-watermark", Some("next"))),
        Ok(page("old-watermark", None)),
    ]);

    let outcome = service(test.database.clone())
        .run(account, &source)
        .await
        .expect("the bounded scan completes");
    let watermark: Option<String> = sqlx::query_scalar(
        "select watermark_provider_post_id from x_archive.bookmark_incremental_state \
         where account_id = $1",
    )
    .bind(account)
    .fetch_one(test.database.pool())
    .await
    .expect("the incremental state is readable");

    test.cleanup().await.expect("cleanup drops the database");

    let watermark_provider_post_id = match outcome {
        IncrementalOutcome::Completed {
            watermark_provider_post_id,
            ..
        } => watermark_provider_post_id,
        IncrementalOutcome::RequiresFullSnapshot { .. } => {
            panic!("the scan reaches the prior watermark")
        }
        IncrementalOutcome::Incomplete { .. } => panic!("the scan reaches the prior watermark"),
    };
    assert_eq!(
        watermark_provider_post_id.as_deref(),
        Some("new-watermark"),
        "only a scan that reaches the old watermark advances to the newest observation"
    );
    assert_eq!(watermark.as_deref(), Some("new-watermark"));
    assert_eq!(source.calls(), vec![None, Some("next".to_owned())]);
}

#[tokio::test]
async fn page_bound_gap_requires_a_full_rescan() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("incremental-gap-account")
        .await
        .expect("the account seeds");
    sqlx::query(
        "insert into x_archive.bookmark_incremental_state (account_id, watermark_provider_post_id) \
         values ($1, 'old-watermark')",
    )
    .bind(account)
    .execute(test.database.pool())
    .await
    .expect("the prior watermark seeds");
    let source = FakeBookmarkPageSource::new(vec![
        Ok(page("new-watermark", Some("next"))),
        Ok(page("old-watermark", None)),
    ]);
    let instant = "2026-08-20T12:00:00Z"
        .parse()
        .expect("the fixture instant parses");
    let clock: Arc<dyn Clock> = Arc::new(FakeClock { instant });
    let budget = BudgetGate::with_clock(
        test.database.clone(),
        BudgetClass::Read,
        10,
        3_600,
        Arc::clone(&clock),
    )
    .expect("the budget gate accepts the fixture configuration");

    let outcome = IncrementalScanService::new(test.database.clone(), budget, clock, 1, 3)
        .run(account, &source)
        .await
        .expect("a bounded gap is a recorded outcome");
    let requires_full_snapshot: bool = sqlx::query_scalar(
        "select requires_full_snapshot from x_archive.bookmark_incremental_state where account_id = $1",
    )
    .bind(account)
    .fetch_one(test.database.pool())
    .await
    .expect("the incremental state is readable");

    test.cleanup().await.expect("cleanup drops the database");

    assert!(matches!(
        outcome,
        IncrementalOutcome::RequiresFullSnapshot { .. }
    ));
    assert!(
        requires_full_snapshot,
        "a bounded scan gap requires a full rescan"
    );
    assert_eq!(
        source.calls(),
        vec![None],
        "the page cap prevents another request"
    );
}

#[tokio::test]
async fn budget_refusal_stops_before_incremental_provider_contact() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("incremental-budget-account")
        .await
        .expect("the account seeds");
    let instant = "2026-08-20T12:00:00Z"
        .parse()
        .expect("the fixture instant parses");
    let clock: Arc<dyn Clock> = Arc::new(FakeClock { instant });
    let budget = BudgetGate::with_clock(
        test.database.clone(),
        BudgetClass::Read,
        1,
        3_600,
        Arc::clone(&clock),
    )
    .expect("the budget gate accepts the fixture configuration");
    budget
        .reserve(account, 1)
        .await
        .expect("the fixture spends the only request");
    let source = FakeBookmarkPageSource::new(vec![Ok(page("must-not-fetch", None))]);

    let outcome = IncrementalScanService::new(test.database.clone(), budget, clock, 3, 1)
        .run(account, &source)
        .await;

    test.cleanup().await.expect("cleanup drops the database");

    assert!(outcome.is_ok(), "budget refusal is a recorded scan outcome");
    assert!(
        source.calls().is_empty(),
        "budget refusal precedes provider contact"
    );
}

#[tokio::test]
async fn scheduled_scan_command_uses_the_platform_type_grammar() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("scheduled-incremental-account")
        .await
        .expect("the account seeds");
    sqlx::query(
        "insert into x_archive.bookmark_incremental_state (account_id, requires_full_snapshot) \
         values ($1, true)",
    )
    .bind(account)
    .execute(test.database.pool())
    .await
    .expect("the escalation state seeds");

    let selected = service(test.database.clone())
        .scheduled_scan(SCHEDULED_BOOKMARK_SCAN_COMMAND, account)
        .await
        .expect("the platform command type is accepted");
    test.cleanup().await.expect("cleanup drops the database");

    assert_eq!(selected, ScheduledScan::FullSnapshot);
}
