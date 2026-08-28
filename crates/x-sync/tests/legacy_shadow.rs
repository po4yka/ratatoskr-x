//! Deterministic, non-mutating legacy shadow comparison.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions and synthetic fixture setup in a test binary"
)]

use sha2::Digest as _;
use x_persistence::test_support::TestDatabase;
use x_sync::{LegacyTransitionError, LegacyTransitionService, ShadowDiffClass};

fn sha256(value: &str) -> String {
    let digest = sha2::Sha256::digest(value.as_bytes());
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        for nibble in [byte >> 4, byte & 0x0f] {
            if let Some(character) = char::from_digit(u32::from(nibble), 16) {
                encoded.push(character);
            }
        }
    }
    encoded
}

#[expect(
    clippy::too_many_lines,
    reason = "one synthetic database graph keeps all five shadow classifications internally consistent"
)]
async fn seed_shadow_evidence(test: &TestDatabase) -> (uuid::Uuid, uuid::Uuid, uuid::Uuid) {
    let account_id = test
        .seed_account("shadow-owner-provider")
        .await
        .expect("the shadow target account seeds");
    let import_run_id: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.legacy_import_runs \
             (account_id, source_kind, source_version, source_digest, importer_parser_version, \
              ownership_approval_digest, status, inserted_count, conflicted_count, \
              unmapped_count, finished_at) \
         values ($1, 'monolith_csv', 1, $2, 1, $3, 'completed', 2, 1, 1, now()) \
         returning id",
    )
    .bind(account_id)
    .bind("a".repeat(64))
    .bind("b".repeat(64))
    .fetch_one(test.database.pool())
    .await
    .expect("the completed import run seeds");
    let matched_post_id: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.posts \
             (provider_id, text, parser_version, normalization_provenance, availability) \
         values ('10001', 'official changed text', 9, 'official-api', 'active') returning id",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("the matched official post seeds");
    let legacy_only_post_id: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.posts \
             (provider_id, text, parser_version, normalization_provenance, availability) \
         values ('10002', 'legacy only text', 1, 'legacy-import', 'unknown') returning id",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("the legacy-only post seeds");
    let official_only_post_id: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.posts \
             (provider_id, text, parser_version, normalization_provenance, availability) \
         values ('10005', 'official only text', 9, 'official-api', 'active') returning id",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("the official-only post seeds");
    let url_matched_post_id: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.posts \
             (provider_id, text, parser_version, normalization_provenance, availability) \
         values ('10006', 'official URL-matched text', 9, 'official-api', 'active') returning id",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("the URL-matched official post seeds");

    for (
        key,
        row_digest,
        provider_id,
        conflicting_id,
        post_id,
        content_digest,
        category,
        folder,
        resolution,
    ) in [
        (
            "matched",
            "1".repeat(64),
            Some("10001"),
            None,
            Some(matched_post_id),
            Some(sha256("legacy matched text")),
            serde_json::json!([]),
            serde_json::json!([{"id": "folder-1"}]),
            "provider_post",
        ),
        (
            "legacy-only",
            "2".repeat(64),
            Some("10002"),
            None,
            Some(legacy_only_post_id),
            Some(sha256("legacy only text")),
            serde_json::json!(["research"]),
            serde_json::json!([]),
            "provider_post",
        ),
        (
            "url-matched",
            "6".repeat(64),
            Some("10006"),
            None,
            Some(url_matched_post_id),
            Some(sha256("official URL-matched text")),
            serde_json::json!([]),
            serde_json::json!([]),
            "canonical_url",
        ),
        (
            "matched-duplicate-observation",
            "7".repeat(64),
            Some("10001"),
            None,
            Some(matched_post_id),
            Some(sha256("legacy matched text")),
            serde_json::json!([]),
            serde_json::json!([]),
            "provider_post",
        ),
        (
            "conflict",
            "3".repeat(64),
            Some("10003"),
            Some("10004"),
            None,
            Some(sha256("conflicting legacy text")),
            serde_json::json!([]),
            serde_json::json!([]),
            "identity_conflict",
        ),
        (
            "unmapped",
            "4".repeat(64),
            None,
            None,
            None,
            Some(sha256("unmapped legacy text")),
            serde_json::json!([]),
            serde_json::json!([]),
            "unmapped",
        ),
    ] {
        sqlx::query(
            "insert into x_archive.legacy_import_items \
                 (import_run_id, source_record_key, source_row_digest, provider_post_id, \
                  conflicting_provider_post_id, post_id, content_digest, source_synced_at, \
                  legacy_category_metadata, legacy_folder_metadata, resolution) \
             values ($1, $2, $3, $4, $5, $6, $7, now(), $8, $9, $10)",
        )
        .bind(import_run_id)
        .bind(key)
        .bind(row_digest)
        .bind(provider_id)
        .bind(conflicting_id)
        .bind(post_id)
        .bind(content_digest)
        .bind(category)
        .bind(folder)
        .bind(resolution)
        .execute(test.database.pool())
        .await
        .expect("the redacted import item seeds");
    }

    let sync_run_id: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.sync_runs \
             (account_id, run_type, state, pages_fetched, items_observed, finished_at) \
         values ($1, 'full', 'completed', 1, 2, now()) returning id",
    )
    .bind(account_id)
    .fetch_one(test.database.pool())
    .await
    .expect("the completed full run seeds");
    let snapshot_id: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.snapshots \
             (sync_run_id, complete, completed_at, page_count) \
         values ($1, true, now(), 1) returning id",
    )
    .bind(sync_run_id)
    .fetch_one(test.database.pool())
    .await
    .expect("the complete snapshot seeds");
    for post_id in [matched_post_id, url_matched_post_id, official_only_post_id] {
        sqlx::query(
            "insert into x_archive.snapshot_bookmark_items \
                 (snapshot_id, post_id, observed_at) values ($1, $2, now())",
        )
        .bind(snapshot_id)
        .bind(post_id)
        .execute(test.database.pool())
        .await
        .expect("the official snapshot item seeds");
        sqlx::query(
            "insert into x_archive.bookmarks \
                 (account_id, post_id, first_observed_saved_at, last_observed_saved_at) \
             values ($1, $2, now(), now())",
        )
        .bind(account_id)
        .bind(post_id)
        .execute(test.database.pool())
        .await
        .expect("the authoritative bookmark projection seeds");
    }
    sqlx::query(
        "insert into x_archive.bookmark_snapshot_authority (account_id, snapshot_id) \
         values ($1, $2)",
    )
    .bind(account_id)
    .bind(snapshot_id)
    .execute(test.database.pool())
    .await
    .expect("the current snapshot authority seeds");
    (account_id, import_run_id, snapshot_id)
}

async fn mutation_counts(test: &TestDatabase) -> (i64, i64, i64) {
    let bookmarks = sqlx::query_scalar("select count(*) from x_archive.bookmarks")
        .fetch_one(test.database.pool())
        .await
        .expect("bookmark count is readable");
    let outbox = sqlx::query_scalar("select count(*) from x_archive.outbox_events")
        .fetch_one(test.database.pool())
        .await
        .expect("outbox count is readable");
    let reports = sqlx::query_scalar("select count(*) from x_archive.legacy_shadow_reports")
        .fetch_one(test.database.pool())
        .await
        .expect("report count is readable");
    (bookmarks, outbox, reports)
}

#[tokio::test]
async fn shadow_report_classifies_differences_deterministically_without_mutating_bookmarks() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let (account_id, import_run_id, snapshot_id) = seed_shadow_evidence(&test).await;
    let service = LegacyTransitionService::new(test.database.clone());
    let before = mutation_counts(&test).await;

    let first = service
        .shadow_report_for(account_id, import_run_id, snapshot_id)
        .await;
    assert!(
        !matches!(first, Err(LegacyTransitionError::NotImplemented)),
        "shadow comparison remains NotImplemented"
    );
    let first = first.expect("complete official evidence produces a shadow report");
    let second = service
        .shadow_report_for(account_id, import_run_id, snapshot_id)
        .await
        .expect("the same comparison is deterministic and reusable");
    assert_eq!(second.report_id, first.report_id);
    assert_eq!(second.report_digest, first.report_digest);
    assert_eq!(second.report, first.report);
    assert!(second.reused);
    assert_eq!(
        first
            .report
            .entries
            .iter()
            .map(|entry| entry.class)
            .collect::<Vec<_>>(),
        vec![
            ShadowDiffClass::Matched,
            ShadowDiffClass::Matched,
            ShadowDiffClass::UrlMatched,
            ShadowDiffClass::LegacyOnly,
            ShadowDiffClass::OfficialOnly,
            ShadowDiffClass::IdentityConflict,
            ShadowDiffClass::Unmapped,
        ]
    );
    assert_eq!(first.report.summary.matched, 2);
    assert_eq!(first.report.summary.url_matched, 1);
    assert_eq!(first.report.summary.legacy_only, 1);
    assert_eq!(first.report.summary.official_only, 1);
    assert_eq!(first.report.summary.identity_conflicts, 1);
    assert_eq!(first.report.summary.unmapped, 1);
    assert!(first.report.entries[0].content_changed);
    assert!(first.report.entries[0].legacy_folder_present);
    assert!(first.report.entries[3].legacy_category_present);
    let canonical = serde_json::to_string(&first.report).expect("the report serializes");
    for forbidden in [
        "legacy matched text",
        "official changed text",
        "legacy only text",
        "https://",
        "mutable_handle",
    ] {
        assert!(
            !canonical.contains(forbidden),
            "report leaked `{forbidden}`"
        );
    }
    assert_eq!(before, (3, 0, 0));
    assert_eq!(mutation_counts(&test).await, (3, 0, 1));
    test.cleanup().await.expect("cleanup drops the database");
}

async fn assert_shadow_refused_without_report(
    service: &LegacyTransitionService,
    test: &TestDatabase,
    account_id: uuid::Uuid,
    import_run_id: uuid::Uuid,
    snapshot_id: uuid::Uuid,
) {
    let error = service
        .shadow_report_for(account_id, import_run_id, snapshot_id)
        .await
        .expect_err("non-authoritative evidence is refused");
    assert!(matches!(
        error,
        LegacyTransitionError::SnapshotNotAuthoritative
            | LegacyTransitionError::ImportRunUnavailable
    ));
    assert_eq!(mutation_counts(test).await, (3, 0, 0));
}

#[tokio::test]
async fn incomplete_snapshot_cannot_produce_reviewable_shadow_report() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let (account_id, import_run_id, snapshot_id) = seed_shadow_evidence(&test).await;
    let service = LegacyTransitionService::new(test.database.clone());
    let sync_run_id: uuid::Uuid =
        sqlx::query_scalar("select sync_run_id from x_archive.snapshots where id = $1")
            .bind(snapshot_id)
            .fetch_one(test.database.pool())
            .await
            .expect("the snapshot run is readable");

    sqlx::query("update x_archive.snapshots set complete = false where id = $1")
        .bind(snapshot_id)
        .execute(test.database.pool())
        .await
        .expect("the partial scenario is configured");
    assert_shadow_refused_without_report(&service, &test, account_id, import_run_id, snapshot_id)
        .await;
    sqlx::query("update x_archive.snapshots set complete = true where id = $1")
        .bind(snapshot_id)
        .execute(test.database.pool())
        .await
        .expect("the partial scenario is restored");

    for state in ["failed", "cancelled"] {
        sqlx::query("update x_archive.sync_runs set state = $2 where id = $1")
            .bind(sync_run_id)
            .bind(state)
            .execute(test.database.pool())
            .await
            .expect("the unsuccessful scenario is configured");
        assert_shadow_refused_without_report(
            &service,
            &test,
            account_id,
            import_run_id,
            snapshot_id,
        )
        .await;
    }
    sqlx::query("update x_archive.sync_runs set state = 'completed' where id = $1")
        .bind(sync_run_id)
        .execute(test.database.pool())
        .await
        .expect("the successful state is restored");

    for checkpoint in ["truncated-page", "rate-limited", "schema-invalid"] {
        sqlx::query("update x_archive.sync_runs set checkpoint = $2 where id = $1")
            .bind(sync_run_id)
            .bind(checkpoint)
            .execute(test.database.pool())
            .await
            .expect("the incomplete checkpoint scenario is configured");
        assert_shadow_refused_without_report(
            &service,
            &test,
            account_id,
            import_run_id,
            snapshot_id,
        )
        .await;
    }
    sqlx::query("update x_archive.sync_runs set checkpoint = null where id = $1")
        .bind(sync_run_id)
        .execute(test.database.pool())
        .await
        .expect("the terminal checkpoint is restored");

    sqlx::query("update x_archive.snapshots set page_count = 0 where id = $1")
        .bind(snapshot_id)
        .execute(test.database.pool())
        .await
        .expect("the schema-invalid count scenario is configured");
    assert_shadow_refused_without_report(&service, &test, account_id, import_run_id, snapshot_id)
        .await;
    sqlx::query("update x_archive.snapshots set page_count = 1 where id = $1")
        .bind(snapshot_id)
        .execute(test.database.pool())
        .await
        .expect("the page count is restored");

    let other_account = test
        .seed_account("other-shadow-owner")
        .await
        .expect("the other account seeds");
    assert_shadow_refused_without_report(
        &service,
        &test,
        other_account,
        import_run_id,
        snapshot_id,
    )
    .await;
    test.cleanup().await.expect("cleanup drops the database");
}
