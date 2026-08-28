//! Legacy-transition additions to the owned schema inventory.

use super::{create_disposable_database, drop_disposable_database};
use x_persistence::database::Database;

#[tokio::test]
async fn legacy_imported_posts_allow_unresolved_authors_without_fabricating_users() {
    let (url, name, admin) = create_disposable_database().await;
    let database = Database::connect(&url, 2)
        .await
        .expect("a pooled connection");
    database.apply_schema().await.expect("the schema applies");

    let author_is_nullable: String = sqlx::query_scalar(
        "select is_nullable from information_schema.columns \
         where table_schema = 'x_archive' and table_name = 'posts' \
           and column_name = 'author_user_id'",
    )
    .fetch_one(database.pool())
    .await
    .expect("the posts author column is present");
    let inserted_without_author = sqlx::query(
        "insert into x_archive.posts \
         (provider_id, author_user_id, text, parser_version, availability) \
         values ('legacy-post-without-author', null, 'legacy post text', 1, 'unknown')",
    )
    .execute(database.pool())
    .await;
    let user_count: i64 = sqlx::query_scalar("select count(*) from x_archive.users")
        .fetch_one(database.pool())
        .await
        .expect("the normalized user count is readable");

    database.pool().close().await;
    drop_disposable_database(&name, &admin).await;
    admin.close().await;

    assert_eq!(
        author_is_nullable, "YES",
        "legacy posts with only a mutable author handle require nullable posts.author_user_id"
    );
    inserted_without_author.expect("an imported post may retain unresolved author identity");
    assert_eq!(
        user_count, 0,
        "unresolved legacy authors must not create fabricated provider users"
    );
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one schema scenario verifies the complete legacy-transition evidence boundary"
)]
async fn legacy_transition_tables_are_account_scoped_and_secret_free() {
    let (url, name, admin) = create_disposable_database().await;
    let database = Database::connect(&url, 2)
        .await
        .expect("a pooled connection");
    database.apply_schema().await.expect("the schema applies");

    let transition_tables = [
        "legacy_import_runs",
        "legacy_import_items",
        "legacy_shadow_reports",
        "legacy_transition_approvals",
    ];
    let mut missing_tables = Vec::new();
    for table in transition_tables {
        let present: bool = sqlx::query_scalar(
            "select count(*) > 0 from information_schema.tables \
             where table_schema = 'x_archive' and table_name = $1",
        )
        .bind(table)
        .fetch_one(database.pool())
        .await
        .expect("the legacy-transition table catalog is readable");
        if !present {
            missing_tables.push(table);
        }
    }

    let mut missing_columns = Vec::new();
    for (table, column) in [
        ("legacy_import_runs", "account_id"),
        ("legacy_import_runs", "source_kind"),
        ("legacy_import_runs", "source_version"),
        ("legacy_import_runs", "source_digest"),
        ("legacy_import_runs", "importer_parser_version"),
        ("legacy_import_runs", "ownership_approval_digest"),
        ("legacy_import_runs", "inserted_count"),
        ("legacy_import_runs", "matched_count"),
        ("legacy_import_runs", "updated_count"),
        ("legacy_import_runs", "conflicted_count"),
        ("legacy_import_runs", "rejected_count"),
        ("legacy_import_runs", "unmapped_count"),
        ("legacy_import_items", "import_run_id"),
        ("legacy_import_items", "source_record_key"),
        ("legacy_import_items", "source_row_digest"),
        ("legacy_import_items", "resolution"),
        ("legacy_shadow_reports", "account_id"),
        ("legacy_shadow_reports", "import_run_id"),
        ("legacy_shadow_reports", "snapshot_id"),
        ("legacy_shadow_reports", "import_digest"),
        ("legacy_shadow_reports", "snapshot_digest"),
        ("legacy_shadow_reports", "report_digest"),
        ("legacy_transition_approvals", "account_id"),
        ("legacy_transition_approvals", "internal_owner_id"),
        ("legacy_transition_approvals", "shadow_report_id"),
        ("legacy_transition_approvals", "checklist_digest"),
        ("legacy_transition_approvals", "decided_at"),
        ("legacy_transition_approvals", "superseded_at"),
    ] {
        let present: bool = sqlx::query_scalar(
            "select count(*) > 0 from information_schema.columns \
             where table_schema = 'x_archive' and table_name = $1 and column_name = $2",
        )
        .bind(table)
        .bind(column)
        .fetch_one(database.pool())
        .await
        .expect("the legacy-transition column catalog is readable");
        if !present {
            missing_columns.push(format!("{table}.{column}"));
        }
    }

    let account_foreign_keys: Vec<String> = sqlx::query_scalar(
        "select rel.relname from pg_constraint con \
         join pg_class rel on rel.oid = con.conrelid \
         join pg_namespace ns on ns.oid = rel.relnamespace \
         join pg_class target on target.oid = con.confrelid \
         where ns.nspname = 'x_archive' and rel.relname = any($1) \
           and target.relname = 'accounts' and con.contype = 'f' order by rel.relname",
    )
    .bind(
        &[
            "legacy_import_runs",
            "legacy_shadow_reports",
            "legacy_transition_approvals",
        ][..],
    )
    .fetch_all(database.pool())
    .await
    .expect("legacy-transition account constraints are readable");

    let item_run_foreign_key: bool = sqlx::query_scalar(
        "select count(*) > 0 from pg_constraint con \
         join pg_class rel on rel.oid = con.conrelid \
         join pg_namespace ns on ns.oid = rel.relnamespace \
         join pg_class target on target.oid = con.confrelid \
         where ns.nspname = 'x_archive' and rel.relname = 'legacy_import_items' \
           and target.relname = 'legacy_import_runs' and con.contype = 'f'",
    )
    .fetch_one(database.pool())
    .await
    .expect("legacy import item constraints are readable");

    let leaked_columns: Vec<String> = sqlx::query_scalar(
        "select concat(table_name, '.', column_name) from information_schema.columns \
         where table_schema = 'x_archive' and table_name = any($1) \
           and column_name = any($2) order by table_name, column_name",
    )
    .bind(&transition_tables[..])
    .bind(
        &[
            "access_token",
            "refresh_token",
            "authorization_code",
            "authorization_header",
            "bearer_header",
            "cookie",
            "cookies",
            "session",
            "session_id",
            "post_body",
            "post_text",
            "text",
            "author_handle",
            "handle",
            "username",
            "raw_url",
            "url",
            "source_path",
            "file_path",
        ][..],
    )
    .fetch_all(database.pool())
    .await
    .expect("legacy-transition columns can be checked for secret and content leaks");

    database.pool().close().await;
    drop_disposable_database(&name, &admin).await;
    admin.close().await;

    assert!(
        missing_tables.is_empty(),
        "legacy transition schema must contain all evidence tables; missing {missing_tables:?}"
    );
    assert!(
        missing_columns.is_empty(),
        "legacy transition evidence columns are incomplete; missing {missing_columns:?}"
    );
    assert_eq!(
        account_foreign_keys,
        [
            "legacy_import_runs",
            "legacy_shadow_reports",
            "legacy_transition_approvals",
        ],
        "run, report, and approval evidence must reference the owning account"
    );
    assert!(
        item_run_foreign_key,
        "legacy import items must inherit account scope through their import run"
    );
    assert!(
        leaked_columns.is_empty(),
        "legacy transition evidence must not retain credentials, post content, mutable handles, raw URLs, or source paths: {leaked_columns:?}"
    );
}
