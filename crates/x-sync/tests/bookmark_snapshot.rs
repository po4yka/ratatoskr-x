//! Full snapshot behavior against a disposable `PostgreSQL` database.

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
use x_budget::gate::{BudgetGate, Clock};
use x_persistence::test_support::TestDatabase;
use x_sync::{
    BookmarkPage, BookmarkPageSource, BookmarkSnapshotService, BookmarkSourceError, SnapshotOutcome,
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

fn service(database: x_persistence::database::Database) -> BookmarkSnapshotService {
    service_with_budget(database, 10)
}

fn service_with_budget(
    database: x_persistence::database::Database,
    request_cap: u32,
) -> BookmarkSnapshotService {
    let instant = "2026-08-20T12:00:00Z"
        .parse()
        .expect("the fixture instant parses");
    let clock: Arc<dyn Clock> = Arc::new(FakeClock { instant });
    let budget = BudgetGate::with_clock(database.clone(), request_cap, 3_600, Arc::clone(&clock))
        .expect("the budget gate accepts the fixture configuration");
    BookmarkSnapshotService::new(database, budget, clock)
}

async fn seed_previous_authority(
    test: &TestDatabase,
    account: uuid::Uuid,
    provider_post_id: &str,
) -> (uuid::Uuid, uuid::Uuid) {
    let author_provider_id = format!("{provider_post_id}-author");
    let author: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.users (provider_id, parser_version) values ($1, 1) returning id",
    )
    .bind(author_provider_id)
    .fetch_one(test.database.pool())
    .await
    .expect("the old author seeds");
    let post: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.posts (provider_id, author_user_id, parser_version) \
         values ($1, $2, 1) returning id",
    )
    .bind(provider_post_id)
    .bind(author)
    .fetch_one(test.database.pool())
    .await
    .expect("the old post seeds");
    let run: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.sync_runs (account_id, run_type, state, finished_at) \
         values ($1, 'full', 'completed', now()) returning id",
    )
    .bind(account)
    .fetch_one(test.database.pool())
    .await
    .expect("the old run seeds");
    let snapshot: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.snapshots (sync_run_id, complete, completed_at) \
         values ($1, true, now()) returning id",
    )
    .bind(run)
    .fetch_one(test.database.pool())
    .await
    .expect("the old snapshot seeds");
    sqlx::query("insert into x_archive.bookmarks (account_id, post_id) values ($1, $2)")
        .bind(account)
        .bind(post)
        .execute(test.database.pool())
        .await
        .expect("the old bookmark seeds");
    sqlx::query(
        "insert into x_archive.bookmark_snapshot_authority (account_id, snapshot_id) values ($1, $2) \
         on conflict (account_id) do update set snapshot_id = excluded.snapshot_id",
    )
    .bind(account)
    .bind(snapshot)
    .execute(test.database.pool())
    .await
    .expect("the old authority seeds");
    (snapshot, post)
}

#[tokio::test]
async fn budget_exhaustion_stops_before_the_unfetched_page() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("budget-exhaustion-account")
        .await
        .expect("the account seeds");
    let snapshot_service = service_with_budget(test.database.clone(), 1);
    let source = FakeBookmarkPageSource::new(vec![
        Ok(page("post-1", Some("must-not-be-fetched"))),
        Ok(page("post-2", None)),
    ]);

    let outcome = snapshot_service
        .run(account, &source, None)
        .await
        .expect("budget exhaustion is a resumable outcome");
    let authority_count: i64 = sqlx::query_scalar(
        "select count(*) from x_archive.bookmark_snapshot_authority where account_id = $1",
    )
    .bind(account)
    .fetch_one(test.database.pool())
    .await
    .expect("authority rows are readable");

    test.cleanup().await.expect("cleanup drops the database");

    assert!(matches!(outcome, SnapshotOutcome::Incomplete { .. }));
    assert_eq!(
        source.calls(),
        vec![None],
        "budget refusal must happen before a second provider request"
    );
    assert_eq!(
        authority_count, 0,
        "an exhausted run has no absence authority"
    );
}

#[tokio::test]
async fn keeps_previous_authority_visible_until_complete_swap() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("authority-swap-account")
        .await
        .expect("the account seeds");
    let author: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.users (provider_id, parser_version) values ('old-author', 1) returning id",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("the old author seeds");
    let old_post: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.posts (provider_id, author_user_id, parser_version) \
         values ('old-post', $1, 1) returning id",
    )
    .bind(author)
    .fetch_one(test.database.pool())
    .await
    .expect("the old post seeds");
    let old_run: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.sync_runs (account_id, run_type, state, finished_at) \
         values ($1, 'full', 'completed', now()) returning id",
    )
    .bind(account)
    .fetch_one(test.database.pool())
    .await
    .expect("the old run seeds");
    let old_snapshot: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.snapshots (sync_run_id, complete, completed_at) \
         values ($1, true, now()) returning id",
    )
    .bind(old_run)
    .fetch_one(test.database.pool())
    .await
    .expect("the old snapshot seeds");
    sqlx::query("insert into x_archive.bookmarks (account_id, post_id) values ($1, $2)")
        .bind(account)
        .bind(old_post)
        .execute(test.database.pool())
        .await
        .expect("the old bookmark seeds");
    sqlx::query(
        "insert into x_archive.bookmark_snapshot_authority (account_id, snapshot_id) values ($1, $2)",
    )
    .bind(account)
    .bind(old_snapshot)
    .execute(test.database.pool())
    .await
    .expect("the old authority seeds");
    let snapshot_service = service(test.database.clone());
    let source = FakeBookmarkPageSource::new(vec![Ok(page("new-post", None))]);

    let before: uuid::Uuid = sqlx::query_scalar(
        "select snapshot_id from x_archive.bookmark_snapshot_authority where account_id = $1",
    )
    .bind(account)
    .fetch_one(test.database.pool())
    .await
    .expect("the old authority is visible before new completion");
    let outcome = snapshot_service
        .run(account, &source, None)
        .await
        .expect("the complete page is accepted");
    let new_snapshot = match outcome {
        SnapshotOutcome::Completed { snapshot_id, .. } => snapshot_id,
        other @ SnapshotOutcome::Incomplete { .. } => {
            panic!("the complete page must atomically finalize authority, got {other:?}")
        }
    };
    let after: uuid::Uuid = sqlx::query_scalar(
        "select snapshot_id from x_archive.bookmark_snapshot_authority where account_id = $1",
    )
    .bind(account)
    .fetch_one(test.database.pool())
    .await
    .expect("the new authority is visible after completion");

    test.cleanup().await.expect("cleanup drops the database");

    assert_eq!(
        before, old_snapshot,
        "staging must not replace old authority"
    );
    assert_eq!(
        after, new_snapshot,
        "completion swaps authority as one result"
    );
    assert_ne!(
        after, old_snapshot,
        "the completed snapshot replaces the old authority"
    );
}

#[tokio::test]
async fn records_unbookmark_observation_without_deleting_the_bookmark() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("unbookmark-observation-account")
        .await
        .expect("the account seeds");
    let (_, old_post) = seed_previous_authority(&test, account, "old-post").await;
    let snapshot_service = service(test.database.clone());
    let source = FakeBookmarkPageSource::new(vec![Ok(page("new-post", None))]);

    let outcome = snapshot_service
        .run(account, &source, None)
        .await
        .expect("the replacement snapshot completes");
    let evidence_snapshot = match outcome {
        SnapshotOutcome::Completed { snapshot_id, .. } => snapshot_id,
        other @ SnapshotOutcome::Incomplete { .. } => {
            panic!("the replacement must complete, got {other:?}")
        }
    };
    let observed: bool = sqlx::query_scalar(
        "select observed_removed_at is not null and observed_removed_snapshot_id = $3 \
         from x_archive.bookmarks where account_id = $1 and post_id = $2",
    )
    .bind(account)
    .bind(old_post)
    .bind(evidence_snapshot)
    .fetch_one(test.database.pool())
    .await
    .expect("the retained bookmark row is readable");
    let rows: i64 = sqlx::query_scalar(
        "select count(*) from x_archive.bookmarks where account_id = $1 and post_id = $2",
    )
    .bind(account)
    .bind(old_post)
    .fetch_one(test.database.pool())
    .await
    .expect("the bookmark row count is readable");

    test.cleanup().await.expect("cleanup drops the database");

    assert!(
        observed,
        "complete absence carries timestamp and snapshot evidence"
    );
    assert_eq!(
        rows, 1,
        "an unbookmark observation never deletes the bookmark row"
    );
}

#[tokio::test]
async fn reconciles_added_retained_and_removed_counts() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("reconciliation-counts-account")
        .await
        .expect("the account seeds");
    let (_, removed_post) = seed_previous_authority(&test, account, "removed-post").await;
    seed_previous_authority(&test, account, "retained-post").await;
    let snapshot_service = service(test.database.clone());
    let source = FakeBookmarkPageSource::new(vec![
        Ok(page("new-post", Some("second-page"))),
        Ok(page("retained-post", None)),
    ]);

    let outcome = snapshot_service
        .run(account, &source, None)
        .await
        .expect("the complete replacement succeeds");
    assert!(matches!(outcome, SnapshotOutcome::Completed { .. }));
    let statistics: (i32, i32, i32) = sqlx::query_as(
        "select added_count, retained_count, removed_count from x_archive.sync_runs \
         where id = $1",
    )
    .bind(match outcome {
        SnapshotOutcome::Completed { run_id, .. } => run_id,
        other @ SnapshotOutcome::Incomplete { .. } => {
            panic!("expected completion, got {other:?}")
        }
    })
    .fetch_one(test.database.pool())
    .await
    .expect("the completed run statistics are readable");
    let active_posts: Vec<String> = sqlx::query_scalar(
        "select post.provider_id from x_archive.bookmarks bookmark \
         join x_archive.posts post on post.id = bookmark.post_id \
         where bookmark.account_id = $1 and bookmark.observed_removed_at is null \
         order by post.provider_id",
    )
    .bind(account)
    .fetch_all(test.database.pool())
    .await
    .expect("the current bookmark projection is readable");
    let removed_observed: bool = sqlx::query_scalar(
        "select observed_removed_at is not null from x_archive.bookmarks \
         where account_id = $1 and post_id = $2",
    )
    .bind(account)
    .bind(removed_post)
    .fetch_one(test.database.pool())
    .await
    .expect("the removed bookmark is retained as evidence");

    test.cleanup().await.expect("cleanup drops the database");

    assert_eq!(
        statistics,
        (1, 1, 1),
        "the three reconciliation outcomes are counted"
    );
    assert_eq!(active_posts, ["new-post", "retained-post"]);
    assert!(
        removed_observed,
        "the omitted bookmark is observed removed rather than deleted"
    );
}

#[tokio::test]
async fn resumes_from_checkpoint_after_mid_run_provider_failure() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("checkpoint-account")
        .await
        .expect("the account seeds");
    let snapshot_service = service(test.database.clone());
    let interrupted = FakeBookmarkPageSource::new(vec![
        Ok(page("post-1", Some("opaque-next-token"))),
        Err(BookmarkSourceError::Unavailable),
    ]);

    let first = snapshot_service
        .run(account, &interrupted, None)
        .await
        .expect("a provider failure must produce a resumable run");
    let run_id = match first {
        SnapshotOutcome::Incomplete { run_id } => run_id,
        SnapshotOutcome::Completed { .. } => panic!("the interrupted run must not complete"),
    };
    let checkpoint: Option<String> =
        sqlx::query_scalar("select checkpoint from x_archive.sync_runs where id = $1")
            .bind(run_id)
            .fetch_one(test.database.pool())
            .await
            .expect("the run is readable");

    let resumed = FakeBookmarkPageSource::new(vec![Err(BookmarkSourceError::Unavailable)]);
    let second = snapshot_service
        .run(account, &resumed, Some(run_id))
        .await
        .expect("the same run stays resumable after another provider failure");
    let staged: i64 = sqlx::query_scalar(
        "select count(*) from x_archive.snapshot_bookmark_items where snapshot_id = \
         (select id from x_archive.snapshots where sync_run_id = $1)",
    )
    .bind(run_id)
    .fetch_one(test.database.pool())
    .await
    .expect("staged membership is readable");

    test.cleanup().await.expect("cleanup drops the database");

    assert_eq!(
        interrupted.calls(),
        vec![None, Some("opaque-next-token".to_owned())],
        "the interrupted traversal reached the failed continuation token"
    );
    assert_eq!(checkpoint.as_deref(), Some("opaque-next-token"));
    assert_eq!(
        resumed.calls(),
        vec![Some("opaque-next-token".to_owned())],
        "resume passes the committed opaque token through unchanged"
    );
    assert!(matches!(second, SnapshotOutcome::Incomplete { .. }));
    assert_eq!(
        staged, 1,
        "resume must not duplicate the committed page membership"
    );
}
