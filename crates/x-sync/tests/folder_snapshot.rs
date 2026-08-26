//! Native folder membership snapshot behavior against a disposable database.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use x_budget::gate::{BudgetGate, Clock};
use x_persistence::test_support::TestDatabase;
use x_sync::{
    FolderCapability, FolderMembershipPage, FolderMembershipPageSource,
    FolderMembershipSourceError, FolderSnapshotOutcome, FolderSnapshotService, NativeFolder,
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
struct FixtureMembershipSource {
    page: FolderMembershipPage,
}

impl FolderMembershipPageSource for FixtureMembershipSource {
    fn fetch_membership_page<'a>(
        &'a self,
        _folder: &'a NativeFolder,
        _continuation: Option<&'a str>,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<FolderMembershipPage, FolderMembershipSourceError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async { Ok(self.page.clone()) })
    }
}

fn service(database: x_persistence::database::Database) -> FolderSnapshotService {
    let instant = "2026-08-20T12:00:00Z"
        .parse()
        .expect("the fixture instant parses");
    let clock: Arc<dyn Clock> = Arc::new(FakeClock { instant });
    let budget = BudgetGate::with_clock(database.clone(), 10, 3_600, Arc::clone(&clock))
        .expect("the budget gate accepts the fixture configuration");
    FolderSnapshotService::new(database, budget, clock)
}

fn fixture_source() -> FixtureMembershipSource {
    let envelope = serde_json::from_str(include_str!(
        "../../../fixtures/x-api/folder_membership_page.json"
    ))
    .expect("the redacted official membership fixture decodes");
    FixtureMembershipSource {
        page: FolderMembershipPage::new(envelope, None),
    }
}

async fn seed_post(test: &TestDatabase, provider_post_id: &str) -> uuid::Uuid {
    let author: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.users (provider_id, parser_version) values ($1, 1) returning id",
    )
    .bind(format!("{provider_post_id}-author"))
    .fetch_one(test.database.pool())
    .await
    .expect("the folder post author seeds");
    sqlx::query_scalar(
        "insert into x_archive.posts (provider_id, author_user_id, parser_version) \
         values ($1, $2, 1) returning id",
    )
    .bind(provider_post_id)
    .bind(author)
    .fetch_one(test.database.pool())
    .await
    .expect("the folder post seeds")
}

async fn seed_membership(
    test: &TestDatabase,
    account_id: uuid::Uuid,
    provider_folder_id: &str,
    provider_post_id: &str,
) -> uuid::Uuid {
    let folder: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.bookmark_folders (account_id, provider_id, name) \
         values ($1, $2, 'Reading') on conflict (account_id, provider_id) do update \
         set name = excluded.name returning id",
    )
    .bind(account_id)
    .bind(provider_folder_id)
    .fetch_one(test.database.pool())
    .await
    .expect("the native folder seeds");
    let post = seed_post(test, provider_post_id).await;
    sqlx::query("insert into x_archive.bookmark_folder_items (folder_id, post_id) values ($1, $2)")
        .bind(folder)
        .bind(post)
        .execute(test.database.pool())
        .await
        .expect("the old folder membership seeds");
    folder
}

async fn seed_previous_membership_authority(
    test: &TestDatabase,
    account_id: uuid::Uuid,
) -> (uuid::Uuid, uuid::Uuid) {
    let folder_id = seed_membership(test, account_id, "folder-reading", "old-post").await;
    let run: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.sync_runs (account_id, run_type, state, finished_at) \
         values ($1, 'folder_membership', 'completed', now()) returning id",
    )
    .bind(account_id)
    .fetch_one(test.database.pool())
    .await
    .expect("the previous folder-membership run seeds");
    let snapshot: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.snapshots (sync_run_id, folder_id, complete, completed_at) \
         values ($1, $2, true, now()) returning id",
    )
    .bind(run)
    .bind(folder_id)
    .fetch_one(test.database.pool())
    .await
    .expect("the previous folder-membership snapshot seeds");
    sqlx::query(
        "insert into x_archive.folder_membership_snapshot_authority (folder_id, snapshot_id) \
         values ($1, $2)",
    )
    .bind(folder_id)
    .bind(snapshot)
    .execute(test.database.pool())
    .await
    .expect("the previous membership authority seeds");
    (folder_id, snapshot)
}

#[tokio::test]
async fn complete_membership_snapshot_records_addition_and_observed_removal() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("folder-membership-diff-account")
        .await
        .expect("the account seeds");
    let folder_id = seed_membership(&test, account, "folder-reading", "old-post").await;
    seed_membership(&test, account, "folder-reading", "retained-post").await;
    let folder = NativeFolder::new("folder-reading", Some("Reading".to_owned()));

    let outcome = service(test.database.clone())
        .run(account, &folder, &fixture_source(), None)
        .await
        .expect("the complete membership page is accepted");
    let active_posts: Vec<String> = sqlx::query_scalar(
        "select post.provider_id from x_archive.bookmark_folder_items item \
         join x_archive.posts post on post.id = item.post_id \
         where item.folder_id = $1 and item.observed_removed_from_folder_at is null \
         order by post.provider_id",
    )
    .bind(folder_id)
    .fetch_all(test.database.pool())
    .await
    .expect("the active membership projection is readable");
    let observations: Vec<(String, String)> = sqlx::query_as(
        "select post.provider_id, observation.kind from x_archive.folder_membership_observations observation \
         join x_archive.posts post on post.id = observation.post_id where observation.folder_id = $1 \
         order by observation.kind, post.provider_id",
    )
    .bind(folder_id)
    .fetch_all(test.database.pool())
    .await
    .expect("the membership observations are readable");

    test.cleanup().await.expect("cleanup drops the database");

    assert!(matches!(outcome, FolderSnapshotOutcome::Completed { .. }));
    assert_eq!(
        active_posts,
        ["new-post", "retained-post"],
        "a complete folder snapshot replaces membership and makes absence observable"
    );
    assert_eq!(
        observations,
        [
            ("new-post".to_owned(), "added".to_owned()),
            ("old-post".to_owned(), "removed".to_owned())
        ],
        "the complete membership diff has one addition and one removal observation"
    );
}

#[tokio::test]
async fn keeps_previous_membership_authority_until_complete_swap() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("folder-membership-authority-account")
        .await
        .expect("the account seeds");
    let (folder_id, old_snapshot) = seed_previous_membership_authority(&test, account).await;
    let before: uuid::Uuid = sqlx::query_scalar(
        "select snapshot_id from x_archive.folder_membership_snapshot_authority where folder_id = $1",
    )
    .bind(folder_id)
    .fetch_one(test.database.pool())
    .await
    .expect("the old membership authority is visible before completion");
    let folder = NativeFolder::new("folder-reading", Some("Reading".to_owned()));
    let outcome = service(test.database.clone())
        .run(account, &folder, &fixture_source(), None)
        .await
        .expect("the complete membership page is accepted");
    let new_snapshot = match outcome {
        FolderSnapshotOutcome::Completed { snapshot_id, .. } => snapshot_id,
        other => panic!("the snapshot must complete, got {other:?}"),
    };
    let after: uuid::Uuid = sqlx::query_scalar(
        "select snapshot_id from x_archive.folder_membership_snapshot_authority where folder_id = $1",
    )
    .bind(folder_id)
    .fetch_one(test.database.pool())
    .await
    .expect("the new membership authority is visible after completion");

    test.cleanup().await.expect("cleanup drops the database");

    assert_eq!(before, old_snapshot, "staging cannot replace old authority");
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
async fn folder_capability_limit_records_no_authority_or_fabricated_state() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("folder-capability-limit-account")
        .await
        .expect("the account seeds");
    let table_exists: bool =
        sqlx::query_scalar("select to_regclass('x_archive.folder_capability_limits') is not null")
            .fetch_one(test.database.pool())
            .await
            .expect("the folder capability table check runs");
    let outcome = service(test.database.clone())
        .record_capability_limit(account, FolderCapability::Listing)
        .await
        .expect("a provider capability limit is recorded");
    let capability: (String, uuid::Uuid) = sqlx::query_as(
        "select capability, last_run_id from x_archive.folder_capability_limits \
         where account_id = $1 and capability = 'listing'",
    )
    .bind(account)
    .fetch_one(test.database.pool())
    .await
    .expect("the durable capability limit is readable");
    let folder_count: i64 =
        sqlx::query_scalar("select count(*) from x_archive.bookmark_folders where account_id = $1")
            .bind(account)
            .fetch_one(test.database.pool())
            .await
            .expect("the folder projection is readable");
    let authority_count: i64 = sqlx::query_scalar(
        "select count(*) from x_archive.folder_membership_snapshot_authority authority \
         join x_archive.bookmark_folders folder on folder.id = authority.folder_id \
         where folder.account_id = $1",
    )
    .bind(account)
    .fetch_one(test.database.pool())
    .await
    .expect("the membership authorities are readable");

    test.cleanup().await.expect("cleanup drops the database");

    assert!(
        table_exists,
        "a provider capability limit needs durable state rather than fabricated folder absence"
    );
    assert!(matches!(
        outcome,
        FolderSnapshotOutcome::CapabilityLimited {
            capability: FolderCapability::Listing,
            ..
        }
    ));
    assert_eq!(capability.0, "listing");
    assert_ne!(
        capability.1,
        uuid::Uuid::nil(),
        "the capability points to its terminal run"
    );
    assert_eq!(
        folder_count, 0,
        "a capability limit cannot invent a native folder"
    );
    assert_eq!(
        authority_count, 0,
        "a capability limit cannot invent membership authority"
    );
}
