//! Owner-verified, atomic legacy import behavior.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions and synthetic fixture setup in a test binary"
)]

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use x_persistence::test_support::TestDatabase;
use x_sync::{
    CurrentAccountIdentity, ImportCounts, LegacySourceSelection, LegacyTransitionError,
    LegacyTransitionService, OwnershipApproval, SourceLimits,
};

const FIELD_THEORY_JSONL: &str = include_str!("fixtures/legacy/field_theory_import_v1.jsonl");
const FIELD_THEORY_PREFLIGHT_JSONL: &str =
    include_str!("fixtures/legacy/field_theory_preflight_v1.jsonl");
const MONOLITH_COMMITTED_CSV: &str = include_str!("fixtures/legacy/monolith_bookmark_metadata.csv");

const CONFLICTING_MONOLITH_CSV: &str = concat!(
    "request_id,bookmark_external_id,x_category,tweet_text,tweet_text_tsv,tweet_author,",
    "tweet_url,posted_at,synced_at\n",
    "legacy-conflict-1,92001,legacy-category,Synthetic conflicting post,,mutable_handle,",
    "https://x.com/mutable_handle/status/92002,2026-01-02T12:00:00Z,",
    "2026-02-02T12:00:00Z\n",
);

#[derive(Debug)]
struct FixtureFile(PathBuf);

impl FixtureFile {
    fn create(source: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ratatoskr-x-legacy-import-{}.csv",
            uuid::Uuid::now_v7()
        ));
        std::fs::write(&path, source).expect("the synthetic import fixture is written");
        Self(path)
    }
}

impl Drop for FixtureFile {
    fn drop(&mut self) {
        drop(std::fs::remove_file(&self.0));
    }
}

#[derive(Debug)]
struct FakeCurrentIdentity {
    provider_user_id: String,
}

impl CurrentAccountIdentity for FakeCurrentIdentity {
    fn provider_user_id<'a>(
        &'a self,
        _account_id: uuid::Uuid,
    ) -> Pin<Box<dyn Future<Output = Result<String, LegacyTransitionError>> + Send + 'a>> {
        Box::pin(async { Ok(self.provider_user_id.clone()) })
    }
}

async fn preflight_fixture() -> (FixtureFile, x_sync::PreflightBatch) {
    preflight_jsonl(FIELD_THEORY_JSONL).await
}

async fn preflight_jsonl(source: &str) -> (FixtureFile, x_sync::PreflightBatch) {
    let fixture = FixtureFile::create(source);
    let batch = LegacyTransitionService::preflight_source(
        LegacySourceSelection::field_theory(Some(fixture.0.clone()), None),
        SourceLimits::default(),
    )
    .await
    .expect("the synthetic Field Theory source preflights");
    (fixture, batch)
}

async fn preflight_csv(source: &str) -> (FixtureFile, x_sync::PreflightBatch) {
    let fixture = FixtureFile::create(source);
    let batch = LegacyTransitionService::preflight_source(
        LegacySourceSelection::monolith_csv(&fixture.0),
        SourceLimits::default(),
    )
    .await
    .expect("the synthetic monolith source preflights");
    (fixture, batch)
}

async fn imported_provenance_column_exists(test: &TestDatabase) -> bool {
    sqlx::query_scalar(
        "select exists (select 1 from information_schema.columns \
         where table_schema = 'x_archive' and table_name = 'posts' \
           and column_name = 'normalization_provenance')",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("the post column catalog is readable")
}

async fn owner_id(test: &TestDatabase, account_id: uuid::Uuid) -> uuid::Uuid {
    sqlx::query_scalar("select internal_user_id from x_archive.accounts where id = $1")
        .bind(account_id)
        .fetch_one(test.database.pool())
        .await
        .expect("the account owner is readable")
}

fn approval(
    account_id: uuid::Uuid,
    internal_owner_id: uuid::Uuid,
    source_digest: &str,
) -> OwnershipApproval {
    OwnershipApproval {
        account_id,
        internal_owner_id,
        source_digest: source_digest.to_owned(),
        approval_digest: "a".repeat(64),
    }
}

async fn transition_row_counts(test: &TestDatabase) -> (i64, i64, i64, i64, i64) {
    let runs = sqlx::query_scalar("select count(*) from x_archive.legacy_import_runs")
        .fetch_one(test.database.pool())
        .await
        .expect("import run count is readable");
    let items = sqlx::query_scalar("select count(*) from x_archive.legacy_import_items")
        .fetch_one(test.database.pool())
        .await
        .expect("import item count is readable");
    let posts = sqlx::query_scalar("select count(*) from x_archive.posts")
        .fetch_one(test.database.pool())
        .await
        .expect("post count is readable");
    let bookmarks = sqlx::query_scalar("select count(*) from x_archive.bookmarks")
        .fetch_one(test.database.pool())
        .await
        .expect("bookmark count is readable");
    let outbox = sqlx::query_scalar("select count(*) from x_archive.outbox_events")
        .fetch_one(test.database.pool())
        .await
        .expect("outbox count is readable");
    (runs, items, posts, bookmarks, outbox)
}

#[tokio::test]
async fn matching_current_oauth_identity_imports_legacy_observations_atomically() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account_id = test
        .seed_account("owner-provider-100")
        .await
        .expect("the target account seeds");
    let internal_owner_id = owner_id(&test, account_id).await;
    let (_fixture, batch) = preflight_fixture().await;
    let owner_approval = approval(account_id, internal_owner_id, batch.source_digest());
    let identity = FakeCurrentIdentity {
        provider_user_id: "owner-provider-100".to_owned(),
    };

    let result = LegacyTransitionService::new(test.database.clone())
        .import_batch(&identity, &batch, Some(&owner_approval))
        .await;
    assert!(
        !matches!(result, Err(LegacyTransitionError::NotImplemented)),
        "the owner-verified atomic import remains NotImplemented"
    );
    let outcome = result.expect("matching current OAuth identity admits the import");
    let counts = transition_row_counts(&test).await;
    assert_eq!(counts, (1, 1, 1, 0, 0));
    assert_eq!(outcome.counts.inserted, 1);
    assert!(!outcome.reused);
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn mismatched_or_ambiguous_owner_mapping_leaves_target_unchanged() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account_id = test
        .seed_account("owner-provider-200")
        .await
        .expect("the target account seeds");
    let internal_owner_id = owner_id(&test, account_id).await;
    let (_fixture, batch) = preflight_fixture().await;
    let owner_approval = approval(account_id, internal_owner_id, batch.source_digest());
    let mismatch = FakeCurrentIdentity {
        provider_user_id: "different-provider-user".to_owned(),
    };
    let service = LegacyTransitionService::new(test.database.clone());

    let mismatch_error = service
        .import_batch(&mismatch, &batch, Some(&owner_approval))
        .await
        .expect_err("a current OAuth identity mismatch is refused");
    assert!(matches!(
        mismatch_error,
        LegacyTransitionError::CurrentIdentityMismatch
    ));
    assert_eq!(transition_row_counts(&test).await, (0, 0, 0, 0, 0));

    let missing_approval_error = service
        .import_batch(&mismatch, &batch, None)
        .await
        .expect_err("an archive with no explicit owner mapping is ambiguous");
    assert!(matches!(
        missing_approval_error,
        LegacyTransitionError::MissingOwnershipApproval
    ));
    assert_eq!(transition_row_counts(&test).await, (0, 0, 0, 0, 0));

    let wrong_owner = approval(account_id, uuid::Uuid::now_v7(), batch.source_digest());
    let matching = FakeCurrentIdentity {
        provider_user_id: "owner-provider-200".to_owned(),
    };
    let wrong_owner_error = service
        .import_batch(&matching, &batch, Some(&wrong_owner))
        .await
        .expect_err("an approval for a different internal owner is refused");
    assert!(matches!(
        wrong_owner_error,
        LegacyTransitionError::InvalidOwnershipApproval
    ));
    assert_eq!(transition_row_counts(&test).await, (0, 0, 0, 0, 0));
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn imports_post_identity_parser_version_and_legacy_provenance_without_fabricating_author() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account_id = test
        .seed_account("owner-provider-300")
        .await
        .expect("the target account seeds");
    let internal_owner_id = owner_id(&test, account_id).await;
    let (_fixture, batch) = preflight_fixture().await;
    let owner_approval = approval(account_id, internal_owner_id, batch.source_digest());
    let identity = FakeCurrentIdentity {
        provider_user_id: "owner-provider-300".to_owned(),
    };
    LegacyTransitionService::new(test.database.clone())
        .import_batch(&identity, &batch, Some(&owner_approval))
        .await
        .expect("the verified synthetic import succeeds");

    let post: (
        String,
        Option<uuid::Uuid>,
        i32,
        String,
        Option<chrono::DateTime<chrono::Utc>>,
    ) = sqlx::query_as(
        "select provider_id, author_user_id, parser_version, availability, published_at \
             from x_archive.posts where provider_id = '91001'",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("the imported normalized post is readable");
    let evidence: (String, bool, String, Option<chrono::DateTime<chrono::Utc>>) = sqlx::query_as(
        "select provenance, author_identity_resolved, resolution, source_synced_at \
         from x_archive.legacy_import_items",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("the imported legacy evidence is readable");
    assert_eq!(post.0, "91001");
    assert_eq!(post.1, None, "a mutable handle never fabricates an X user");
    assert_eq!(post.2, x_sync::IMPORTER_PARSER_VERSION);
    assert_eq!(post.3, "unknown");
    assert_eq!(
        post.4,
        Some("2026-01-01T12:00:00Z".parse().expect("a fixed timestamp"))
    );
    assert_eq!(evidence.0, "legacy-import");
    assert!(!evidence.1);
    assert_eq!(evidence.2, "provider_post");
    assert_eq!(
        evidence.3,
        Some("2026-02-01T12:00:00Z".parse().expect("a fixed timestamp"))
    );
    assert!(
        imported_provenance_column_exists(&test).await,
        "normalized posts need an explicit legacy-import provenance stamp"
    );
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn conflicting_url_identity_is_retained_without_authoritative_bookmark_mutation() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account_id = test
        .seed_account("owner-provider-400")
        .await
        .expect("the target account seeds");
    let internal_owner_id = owner_id(&test, account_id).await;
    let (_fixture, batch) = preflight_csv(CONFLICTING_MONOLITH_CSV).await;
    let owner_approval = approval(account_id, internal_owner_id, batch.source_digest());
    let identity = FakeCurrentIdentity {
        provider_user_id: "owner-provider-400".to_owned(),
    };
    let outcome = LegacyTransitionService::new(test.database.clone())
        .import_batch(&identity, &batch, Some(&owner_approval))
        .await
        .expect("the verified conflict is retained as evidence");

    let evidence: (String, String, String, serde_json::Value) = sqlx::query_as(
        "select provider_post_id, conflicting_provider_post_id, resolution, \
                legacy_category_metadata \
         from x_archive.legacy_import_items",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("the conflicting evidence is readable");
    assert_eq!(outcome.counts.conflicted, 1);
    assert_eq!(evidence.0, "92001");
    assert_eq!(evidence.1, "92002");
    assert_eq!(evidence.2, "identity_conflict");
    assert_eq!(evidence.3, serde_json::json!(["legacy-category"]));
    assert_eq!(transition_row_counts(&test).await, (1, 1, 0, 0, 0));
    assert!(
        imported_provenance_column_exists(&test).await,
        "conflict evidence and normalized provenance use one explicit vocabulary"
    );
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn changed_legacy_source_updates_only_a_legacy_import_projection() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account_id = test
        .seed_account("owner-provider-update")
        .await
        .expect("the target account seeds");
    let internal_owner_id = owner_id(&test, account_id).await;
    let identity = FakeCurrentIdentity {
        provider_user_id: "owner-provider-update".to_owned(),
    };
    let service = LegacyTransitionService::new(test.database.clone());
    let (_initial_fixture, initial_batch) = preflight_fixture().await;
    let initial_approval = approval(account_id, internal_owner_id, initial_batch.source_digest());
    service
        .import_batch(&identity, &initial_batch, Some(&initial_approval))
        .await
        .expect("the initial legacy projection imports");

    let update_csv = concat!(
        "request_id,bookmark_external_id,x_category,tweet_text,tweet_text_tsv,tweet_author,",
        "tweet_url,posted_at,synced_at\n",
        "legacy-update-1,91001,research,Updated legacy text,,mutable_handle,",
        "https://x.com/mutable_handle/status/91001,2026-01-02T12:00:00Z,",
        "2026-02-02T12:00:00Z\n",
    );
    let (_update_fixture, update_batch) = preflight_csv(update_csv).await;
    let update_approval = approval(account_id, internal_owner_id, update_batch.source_digest());
    let update = service
        .import_batch(&identity, &update_batch, Some(&update_approval))
        .await
        .expect("changed evidence updates its importer-owned projection");
    assert_eq!(update.counts.updated, 1);
    assert_eq!(update.counts.inserted, 0);
    let updated_text: String =
        sqlx::query_scalar("select text from x_archive.posts where provider_id = '91001'")
            .fetch_one(test.database.pool())
            .await
            .expect("the updated legacy projection is readable");
    assert_eq!(updated_text, "Updated legacy text");

    sqlx::query(
        "update x_archive.posts set text = 'Official API text', \
         normalization_provenance = 'official-api', parser_version = 9, availability = 'active' \
         where provider_id = '91001'",
    )
    .execute(test.database.pool())
    .await
    .expect("the synthetic official projection supersedes legacy setup");
    let official_conflict_csv = update_csv
        .replace("legacy-update-1", "legacy-update-2")
        .replace(
            "Updated legacy text",
            "Later legacy text must not replace official",
        );
    let (_conflict_fixture, conflict_batch) = preflight_csv(&official_conflict_csv).await;
    let conflict_approval = approval(
        account_id,
        internal_owner_id,
        conflict_batch.source_digest(),
    );
    let matched = service
        .import_batch(&identity, &conflict_batch, Some(&conflict_approval))
        .await
        .expect("official projection precedence is retained");
    assert_eq!(matched.counts.matched, 1);
    assert_eq!(matched.counts.updated, 0);
    let official_text: String =
        sqlx::query_scalar("select text from x_archive.posts where provider_id = '91001'")
            .fetch_one(test.database.pool())
            .await
            .expect("the official projection is readable");
    assert_eq!(official_text, "Official API text");
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn repeating_same_fixture_import_is_idempotent_and_invalid_batch_is_atomic() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account_id = test
        .seed_account("owner-provider-500")
        .await
        .expect("the target account seeds");
    let internal_owner_id = owner_id(&test, account_id).await;
    let (_fixture, batch) = preflight_fixture().await;
    let owner_approval = approval(account_id, internal_owner_id, batch.source_digest());
    let identity = FakeCurrentIdentity {
        provider_user_id: "owner-provider-500".to_owned(),
    };
    let service = LegacyTransitionService::new(test.database.clone());

    let first = service
        .import_batch(&identity, &batch, Some(&owner_approval))
        .await
        .expect("the first import succeeds");
    assert_eq!(
        first.counts,
        ImportCounts {
            inserted: 1,
            matched: 0,
            updated: 0,
            conflicted: 0,
            rejected: 0,
            unmapped: 0,
        },
        "the committed Field Theory JSONL fixture has exact terminal counts"
    );
    let second = service
        .import_batch(&identity, &batch, Some(&owner_approval))
        .await
        .expect("an identical completed import is reused without writes");
    assert_eq!(second.run_id, first.run_id);
    assert_eq!(second.counts, first.counts);
    assert!(second.reused);
    assert_eq!(transition_row_counts(&test).await, (1, 1, 1, 0, 0));

    let (_monolith_fixture, monolith_batch) = preflight_csv(MONOLITH_COMMITTED_CSV).await;
    let monolith_approval = approval(
        account_id,
        internal_owner_id,
        monolith_batch.source_digest(),
    );
    let monolith_first = service
        .import_batch(&identity, &monolith_batch, Some(&monolith_approval))
        .await
        .expect("the committed monolith fixture imports");
    let monolith_second = service
        .import_batch(&identity, &monolith_batch, Some(&monolith_approval))
        .await
        .expect("the committed monolith fixture reuses its completed run");
    assert_eq!(monolith_first.counts.inserted, 1);
    assert_eq!(monolith_second.run_id, monolith_first.run_id);
    assert!(monolith_second.reused);

    let (_jsonl_fixture, jsonl_batch) = preflight_jsonl(FIELD_THEORY_PREFLIGHT_JSONL).await;
    let jsonl_approval = approval(account_id, internal_owner_id, jsonl_batch.source_digest());
    let jsonl_first = service
        .import_batch(&identity, &jsonl_batch, Some(&jsonl_approval))
        .await
        .expect("the committed two-row Field Theory fixture imports");
    let jsonl_second = service
        .import_batch(&identity, &jsonl_batch, Some(&jsonl_approval))
        .await
        .expect("the committed two-row Field Theory fixture reuses its completed run");
    assert_eq!(jsonl_first.counts.inserted, 2);
    assert_eq!(jsonl_second.run_id, jsonl_first.run_id);
    assert!(jsonl_second.reused);
    assert_eq!(transition_row_counts(&test).await, (3, 4, 4, 0, 0));

    let invalid_key = "x".repeat(300);
    let invalid_csv = format!(
        concat!(
            "request_id,bookmark_external_id,x_category,tweet_text,tweet_text_tsv,tweet_author,",
            "tweet_url,posted_at,synced_at\n",
            "valid-before-failure,93001,research,First row must roll back,,mutable_handle,",
            "https://x.com/mutable_handle/status/93001,2026-01-03T12:00:00Z,",
            "2026-02-03T12:00:00Z\n",
            "{},93002,research,Constraint failure row,,mutable_handle,",
            "https://x.com/mutable_handle/status/93002,2026-01-04T12:00:00Z,",
            "2026-02-04T12:00:00Z\n"
        ),
        invalid_key
    );
    let (_invalid_fixture, invalid_batch) = preflight_csv(&invalid_csv).await;
    let invalid_approval = approval(account_id, internal_owner_id, invalid_batch.source_digest());
    let invalid_error = service
        .import_batch(&identity, &invalid_batch, Some(&invalid_approval))
        .await
        .expect_err("a persistence-invalid batch rolls back in full");
    assert!(matches!(invalid_error, LegacyTransitionError::Database));
    assert_eq!(
        transition_row_counts(&test).await,
        (3, 4, 4, 0, 0),
        "neither the first row nor its running import survives rollback"
    );
    test.cleanup().await.expect("cleanup drops the database");
}
