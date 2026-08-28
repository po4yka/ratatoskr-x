//! `schema.sql` creates exactly the owned inventory, applies idempotently, and stays inside
//! the bounded context. Integration tests build disposable databases from the same definition
//! the binary embeds.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use x_persistence::database::Database;

#[path = "schema/legacy_transition.rs"]
mod legacy_transition;

/// The forty tables the owned schema must contain, no more and no fewer.
const OWNED_TABLES: &[&str] = &[
    "accounts",
    "api_budget_windows",
    "article_captures",
    "bookmark_folder_items",
    "bookmark_folders",
    "bookmark_incremental_state",
    "bookmark_reconciliation_repairs",
    "bookmark_snapshot_authority",
    "bookmark_write_audit_events",
    "bookmark_write_authorizations",
    "bookmark_write_consents",
    "bookmark_write_operations",
    "bookmarks",
    "compliance_revalidation_ledger",
    "credentials",
    "explicit_captures",
    "folder_capability_limits",
    "folder_membership_observations",
    "folder_membership_snapshot_authority",
    "inbox_events",
    "knowledge_analysis_links",
    "legacy_import_items",
    "legacy_import_runs",
    "legacy_shadow_reports",
    "legacy_transition_approvals",
    "media",
    "oauth_intents",
    "outbox_events",
    "post_article_links",
    "post_relations",
    "posts",
    "rate_limit_state",
    "snapshot_bookmark_items",
    "snapshot_folder_membership_items",
    "snapshots",
    "social_source_revisions",
    "social_sources",
    "sync_runs",
    "tombstones",
    "users",
];

/// The administrative URL, overridable from the environment.
#[must_use]
#[expect(
    clippy::disallowed_methods,
    reason = "the suite's documented environment override"
)]
pub fn admin_url() -> String {
    std::env::var("X_TEST_DATABASE_URL")
        .unwrap_or_else(|_| "postgres://x:x@127.0.0.1:5432/x".to_owned())
}

/// Creates a uniquely named disposable database and returns `(url, name, admin_pool)`.
///
/// The caller drops the returned database when done.
///
/// # Panics
/// Panics when the administrative connection or the `CREATE DATABASE` fails; the suite cannot
/// continue without one.
pub async fn create_disposable_database() -> (String, String, sqlx::PgPool) {
    let admin_url = admin_url();
    let base = admin_url
        .rsplit_once('/')
        .expect("the url has a database path")
        .0
        .to_owned();
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .connect(&admin_url)
        .await
        .expect("an administrative connection");
    let name = format!("ratatoskr_x_test_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!(
        r#"create database "{name}" template template0 locale_provider icu icu_locale 'und-x-icu' encoding 'UTF8'"#
    ))
    .execute(&admin)
    .await
    .expect("a disposable database");
    (format!("{base}/{name}"), name, admin)
}

/// Drops the named database and closes the administrative pool.
///
/// # Panics
/// Panics when the drop fails; a leaked disposable database would poison later runs.
pub async fn drop_disposable_database(name: &str, admin: &sqlx::PgPool) {
    sqlx::query(&format!(r#"drop database if exists "{name}" with (force)"#))
        .execute(admin)
        .await
        .expect("the disposable database is dropped");
}

#[tokio::test]
async fn fresh_database_receives_full_owned_inventory() {
    let (url, name, admin) = create_disposable_database().await;
    let database = Database::connect(&url, 2)
        .await
        .expect("a pooled connection");
    database.apply_schema().await.expect("the schema applies");

    let mut actual: Vec<String> = sqlx::query_scalar(
        "select table_name from information_schema.tables \
         where table_schema = 'x_archive' and table_type = 'BASE TABLE'",
    )
    .fetch_all(database.pool())
    .await
    .expect("the catalog lists the owned tables");
    actual.sort();

    database.pool().close().await;
    drop_disposable_database(&name, &admin).await;
    admin.close().await;

    assert_eq!(actual, OWNED_TABLES, "the owned inventory is exact");
}

#[tokio::test]
async fn reapplication_is_idempotent() {
    let (url, name, admin) = create_disposable_database().await;
    let database = Database::connect(&url, 2)
        .await
        .expect("a pooled connection");

    database
        .apply_schema()
        .await
        .expect("the first application succeeds");
    let before: Vec<String> = sqlx::query_scalar(
        "select table_name from information_schema.tables \
         where table_schema = 'x_archive' and table_type = 'BASE TABLE' order by table_name",
    )
    .fetch_all(database.pool())
    .await
    .expect("the catalog is readable");

    database
        .apply_schema()
        .await
        .expect("the second application also succeeds");
    let after: Vec<String> = sqlx::query_scalar(
        "select table_name from information_schema.tables \
         where table_schema = 'x_archive' and table_type = 'BASE TABLE' order by table_name",
    )
    .fetch_all(database.pool())
    .await
    .expect("the catalog is readable twice");

    database.pool().close().await;
    drop_disposable_database(&name, &admin).await;
    admin.close().await;

    assert_eq!(
        before, after,
        "reapplication leaves the structure unchanged"
    );
}

#[tokio::test]
async fn no_migration_bookkeeping_table_exists() {
    let (url, name, admin) = create_disposable_database().await;
    let database = Database::connect(&url, 2)
        .await
        .expect("a pooled connection");
    database.apply_schema().await.expect("the schema applies");

    let ledger_tables: Vec<String> = sqlx::query_scalar(
        "select concat(table_schema, '.', table_name) from information_schema.tables \
         where table_name like '%migration%' or table_name like '_sqlx_%'",
    )
    .fetch_all(database.pool())
    .await
    .expect("the whole catalog is searchable");

    database.pool().close().await;
    drop_disposable_database(&name, &admin).await;
    admin.close().await;

    assert!(
        ledger_tables.is_empty(),
        "no migration bookkeeping may exist anywhere: {ledger_tables:?}"
    );
}

#[tokio::test]
async fn accounts_connection_state_is_constrained() {
    let (url, name, admin) = create_disposable_database().await;
    let database = Database::connect(&url, 2)
        .await
        .expect("a pooled connection");
    database.apply_schema().await.expect("the schema applies");

    sqlx::query("insert into x_archive.accounts (provider_user_id) values ('state-default-user')")
        .execute(database.pool())
        .await
        .expect("the default connection state is accepted");
    let stored: String = sqlx::query_scalar(
        "select state from x_archive.accounts where provider_user_id = 'state-default-user'",
    )
    .fetch_one(database.pool())
    .await
    .expect("the account row is readable");
    let outside_vocabulary = sqlx::query(
        "insert into x_archive.accounts (provider_user_id, state) \
         values ('state-bogus-user', 'disconnected')",
    )
    .execute(database.pool())
    .await;

    database.pool().close().await;
    drop_disposable_database(&name, &admin).await;
    admin.close().await;

    assert_eq!(stored, "connected", "accounts default to connected");
    let error = outside_vocabulary.expect_err("a state outside the vocabulary must be rejected");
    let code = match &error {
        sqlx::Error::Database(database_error) => database_error.code(),
        other => panic!("expected a database constraint error, got {other:?}"),
    };
    assert_eq!(
        code.as_deref(),
        Some("23514"),
        "the rejection must come from the CHECK constraint"
    );
}

#[tokio::test]
async fn credentials_carry_superseded_refresh_hash_column() {
    let (url, name, admin) = create_disposable_database().await;
    let database = Database::connect(&url, 2)
        .await
        .expect("a pooled connection");
    database.apply_schema().await.expect("the schema applies");

    let present: bool = sqlx::query_scalar(
        "select count(*) > 0 from information_schema.columns \
         where table_schema = 'x_archive' and table_name = 'credentials' \
           and column_name = 'superseded_refresh_hash' and data_type = 'text'",
    )
    .fetch_one(database.pool())
    .await
    .expect("the column catalog is readable");

    database.pool().close().await;
    drop_disposable_database(&name, &admin).await;
    admin.close().await;

    assert!(
        present,
        "credentials must carry the superseded refresh token hash column"
    );
}

#[tokio::test]
async fn posts_carry_conversation_counts_and_parser_columns() {
    let (url, name, admin) = create_disposable_database().await;
    let database = Database::connect(&url, 2)
        .await
        .expect("a pooled connection");
    database.apply_schema().await.expect("the schema applies");

    let mut missing = Vec::new();
    for (column, data_type) in [
        ("conversation_provider_id", "text"),
        ("like_count", "bigint"),
        ("retweet_count", "bigint"),
        ("reply_count", "bigint"),
        ("quote_count", "bigint"),
        ("bookmark_count", "bigint"),
        ("impression_count", "bigint"),
    ] {
        let present = sqlx::query_scalar::<_, bool>(
            "select count(*) > 0 from information_schema.columns \
             where table_schema = 'x_archive' and table_name = 'posts' \
               and column_name = $1 and data_type = $2 and is_nullable = 'YES'",
        )
        .bind(column)
        .bind(data_type)
        .fetch_one(database.pool())
        .await
        .expect("the posts column catalog is readable");
        if !present {
            missing.push(format!("posts.{column} {data_type} nullable"));
        }
    }
    let parser_version = sqlx::query_scalar::<_, bool>(
        "select count(*) > 0 from information_schema.columns \
         where table_schema = 'x_archive' and table_name = 'posts' \
           and column_name = 'parser_version' and data_type = 'integer' \
           and is_nullable = 'NO'",
    )
    .fetch_one(database.pool())
    .await
    .expect("the posts parser-version column is checkable");
    if !parser_version {
        missing.push("posts.parser_version integer not-null".to_owned());
    }

    database.pool().close().await;
    drop_disposable_database(&name, &admin).await;
    admin.close().await;

    assert!(
        missing.is_empty(),
        "posts must carry conversation, count, and parser columns; missing {missing:?}"
    );
}

#[tokio::test]
async fn normalization_targets_stamp_parser_version() {
    let (url, name, admin) = create_disposable_database().await;
    let database = Database::connect(&url, 2)
        .await
        .expect("a pooled connection");
    database.apply_schema().await.expect("the schema applies");

    let mut missing = Vec::new();
    for table in ["users", "post_relations", "media"] {
        let present = sqlx::query_scalar::<_, bool>(
            "select count(*) > 0 from information_schema.columns \
             where table_schema = 'x_archive' and table_name = $1 \
               and column_name = 'parser_version' and data_type = 'integer' \
               and is_nullable = 'NO'",
        )
        .bind(table)
        .fetch_one(database.pool())
        .await
        .expect("the parser-version column is checkable");
        if !present {
            missing.push(format!("{table}.parser_version integer not-null"));
        }
    }

    database.pool().close().await;
    drop_disposable_database(&name, &admin).await;
    admin.close().await;

    assert!(
        missing.is_empty(),
        "normalization targets must stamp their parser version; missing {missing:?}"
    );
}

#[tokio::test]
async fn snapshot_authority_schema_carries_staging_checkpoints_and_statistics() {
    let (url, name, admin) = create_disposable_database().await;
    let database = Database::connect(&url, 2)
        .await
        .expect("a pooled connection");
    database.apply_schema().await.expect("the schema applies");

    let mut missing = Vec::new();
    for table in ["snapshot_bookmark_items", "bookmark_snapshot_authority"] {
        let present = sqlx::query_scalar::<_, bool>(
            "select count(*) > 0 from information_schema.tables \
             where table_schema = 'x_archive' and table_name = $1",
        )
        .bind(table)
        .fetch_one(database.pool())
        .await
        .expect("the table catalog is readable");
        if !present {
            missing.push(format!("table {table}"));
        }
    }
    for (table, column) in [
        ("sync_runs", "checkpoint"),
        ("sync_runs", "pages_fetched"),
        ("sync_runs", "items_observed"),
        ("sync_runs", "added_count"),
        ("sync_runs", "retained_count"),
        ("sync_runs", "removed_count"),
        ("bookmarks", "observed_removed_snapshot_id"),
    ] {
        let present = sqlx::query_scalar::<_, bool>(
            "select count(*) > 0 from information_schema.columns \
             where table_schema = 'x_archive' and table_name = $1 and column_name = $2",
        )
        .bind(table)
        .bind(column)
        .fetch_one(database.pool())
        .await
        .expect("the column catalog is readable");
        if !present {
            missing.push(format!("{table}.{column}"));
        }
    }

    database.pool().close().await;
    drop_disposable_database(&name, &admin).await;
    admin.close().await;

    assert!(
        missing.is_empty(),
        "snapshot authority needs staging, checkpoint, statistics, and removal evidence; missing {missing:?}"
    );
}

#[tokio::test]
async fn social_source_and_article_capture_inventory_is_owned_and_scoped() {
    let (url, name, admin) = create_disposable_database().await;
    let database = Database::connect(&url, 2)
        .await
        .expect("a pooled connection");
    database.apply_schema().await.expect("the schema applies");

    let mut missing = Vec::new();
    for table in [
        "social_sources",
        "social_source_revisions",
        "article_captures",
        "post_article_links",
    ] {
        let present = sqlx::query_scalar::<_, bool>(
            "select count(*) > 0 from information_schema.tables \
             where table_schema = 'x_archive' and table_name = $1",
        )
        .bind(table)
        .fetch_one(database.pool())
        .await
        .expect("the table catalog is readable");
        if !present {
            missing.push(format!("table {table}"));
        }
    }
    for (table, column) in [
        ("accounts", "internal_user_id"),
        ("posts", "expanded_urls"),
        ("social_sources", "social_source_id"),
        ("social_source_revisions", "content_digest"),
        ("article_captures", "normalized_url"),
        ("article_captures", "document_ir_blob"),
        ("post_article_links", "article_capture_id"),
    ] {
        let present = sqlx::query_scalar::<_, bool>(
            "select count(*) > 0 from information_schema.columns \
             where table_schema = 'x_archive' and table_name = $1 and column_name = $2",
        )
        .bind(table)
        .bind(column)
        .fetch_one(database.pool())
        .await
        .expect("the column catalog is readable");
        if !present {
            missing.push(format!("{table}.{column}"));
        }
    }

    database.pool().close().await;
    drop_disposable_database(&name, &admin).await;
    admin.close().await;

    assert!(
        missing.is_empty(),
        "social source and article capture inventory must be owned and scoped; missing {missing:?}"
    );
}

#[tokio::test]
async fn knowledge_linkage_and_compliance_inventory_is_owned_and_scoped() {
    let (url, name, admin) = create_disposable_database().await;
    let database = Database::connect(&url, 2)
        .await
        .expect("a pooled connection");
    database.apply_schema().await.expect("the schema applies");

    let mut missing = Vec::new();
    for table in ["knowledge_analysis_links", "compliance_revalidation_ledger"] {
        let present = sqlx::query_scalar::<_, bool>(
            "select count(*) > 0 from information_schema.tables \
             where table_schema = 'x_archive' and table_name = $1",
        )
        .bind(table)
        .fetch_one(database.pool())
        .await
        .expect("the table catalog is readable");
        if !present {
            missing.push(format!("table {table}"));
        }
    }
    for (table, column) in [
        ("social_sources", "removed_at"),
        ("social_sources", "removal_reason"),
        ("knowledge_analysis_links", "content_digest"),
        ("compliance_revalidation_ledger", "provider_request_id"),
        ("compliance_revalidation_ledger", "failure_class"),
        ("tombstones", "social_source_id"),
        ("tombstones", "revalidation_id"),
    ] {
        let present = sqlx::query_scalar::<_, bool>(
            "select count(*) > 0 from information_schema.columns \
             where table_schema = 'x_archive' and table_name = $1 and column_name = $2",
        )
        .bind(table)
        .bind(column)
        .fetch_one(database.pool())
        .await
        .expect("the column catalog is readable");
        if !present {
            missing.push(format!("{table}.{column}"));
        }
    }

    database.pool().close().await;
    drop_disposable_database(&name, &admin).await;
    admin.close().await;

    assert!(
        missing.is_empty(),
        "Knowledge linkage and compliance evidence must be source-scoped; missing {missing:?}"
    );
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one schema inventory scenario keeps table, constraint, and secret-leak assertions together"
)]
async fn bookmark_writeback_inventory_is_owned_scoped_and_secret_free() {
    let (url, name, admin) = create_disposable_database().await;
    let database = Database::connect(&url, 2)
        .await
        .expect("a pooled connection");
    database.apply_schema().await.expect("the schema applies");

    let mut missing = Vec::new();
    let write_tables = [
        "bookmark_write_authorizations",
        "bookmark_write_consents",
        "bookmark_write_operations",
        "bookmark_write_audit_events",
    ];
    for table in write_tables {
        let present: bool = sqlx::query_scalar(
            "select count(*) > 0 from information_schema.tables \
             where table_schema = 'x_archive' and table_name = $1",
        )
        .bind(table)
        .fetch_one(database.pool())
        .await
        .expect("the write-back table catalog is readable");
        if !present {
            missing.push(format!("table {table}"));
        }
    }

    for (table, column) in [
        ("oauth_intents", "purpose"),
        ("oauth_intents", "account_id"),
        ("credentials", "activation_outcome"),
        ("api_budget_windows", "budget_class"),
        ("bookmarks", "last_write_operation_id"),
        ("bookmarks", "last_write_observed_at"),
        ("bookmarks", "observed_removed_write_operation_id"),
    ] {
        let present: bool = sqlx::query_scalar(
            "select count(*) > 0 from information_schema.columns \
             where table_schema = 'x_archive' and table_name = $1 and column_name = $2",
        )
        .bind(table)
        .bind(column)
        .fetch_one(database.pool())
        .await
        .expect("the write-back column catalog is readable");
        if !present {
            missing.push(format!("{table}.{column}"));
        }
    }

    let oauth_account_is_scoped: bool = sqlx::query_scalar(
        "select count(*) > 0 from pg_constraint con \
         join pg_class rel on rel.oid = con.conrelid \
         join pg_namespace ns on ns.oid = rel.relnamespace \
         join pg_class target on target.oid = con.confrelid \
         where ns.nspname = 'x_archive' and rel.relname = 'oauth_intents' \
           and target.relname = 'accounts' and con.contype = 'f'",
    )
    .fetch_one(database.pool())
    .await
    .expect("the OAuth intent constraints are readable");
    if !oauth_account_is_scoped {
        missing.push("oauth_intents account foreign key".to_owned());
    }

    let budget_primary_key_is_classed: bool = sqlx::query_scalar(
        "select count(*) > 0 from pg_indexes \
         where schemaname = 'x_archive' and tablename = 'api_budget_windows' \
           and indexdef ilike '%unique%' and indexdef ilike '%account_id%' \
           and indexdef ilike '%budget_class%' and indexdef ilike '%window_start%'",
    )
    .fetch_one(database.pool())
    .await
    .expect("the budget-window indexes are readable");
    if !budget_primary_key_is_classed {
        missing.push("classed api_budget_windows identity".to_owned());
    }

    let leaked_columns: Vec<String> = sqlx::query_scalar(
        "select concat(table_name, '.', column_name) from information_schema.columns \
         where table_schema = 'x_archive' and table_name = any($1) \
           and column_name = any($2) order by table_name, column_name",
    )
    .bind(&write_tables[..])
    .bind(
        &[
            "access_token",
            "refresh_token",
            "authorization_header",
            "bearer_header",
            "raw_body",
            "post_body",
            "post_text",
        ][..],
    )
    .fetch_all(database.pool())
    .await
    .expect("write-back columns can be checked for credential and content leaks");

    database.pool().close().await;
    drop_disposable_database(&name, &admin).await;
    admin.close().await;

    assert!(
        missing.is_empty(),
        "bookmark write-back schema must be owned and account-scoped; missing {missing:?}"
    );
    assert!(
        leaked_columns.is_empty(),
        "bookmark write-back evidence must not retain credentials or post bodies: {leaked_columns:?}"
    );
}

#[tokio::test]
async fn incremental_scan_schema_carries_watermark_escalation_and_unique_repairs() {
    let (url, name, admin) = create_disposable_database().await;
    let database = Database::connect(&url, 2)
        .await
        .expect("a pooled connection");
    database.apply_schema().await.expect("the schema applies");

    let mut missing = Vec::new();
    for table in [
        "bookmark_incremental_state",
        "bookmark_reconciliation_repairs",
    ] {
        let present = sqlx::query_scalar::<_, bool>(
            "select count(*) > 0 from information_schema.tables \
             where table_schema = 'x_archive' and table_name = $1",
        )
        .bind(table)
        .fetch_one(database.pool())
        .await
        .expect("the table catalog is readable");
        if !present {
            missing.push(format!("table {table}"));
        }
    }
    for column in [
        "watermark_provider_post_id",
        "requires_full_snapshot",
        "last_outcome",
        "last_incremental_run_id",
    ] {
        let present = sqlx::query_scalar::<_, bool>(
            "select count(*) > 0 from information_schema.columns \
             where table_schema = 'x_archive' and table_name = 'bookmark_incremental_state' \
               and column_name = $1",
        )
        .bind(column)
        .fetch_one(database.pool())
        .await
        .expect("the column catalog is readable");
        if !present {
            missing.push(format!("bookmark_incremental_state.{column}"));
        }
    }
    let repair_identity_is_unique: bool = sqlx::query_scalar(
        "select count(*) > 0 from pg_indexes where schemaname = 'x_archive' \
         and tablename = 'bookmark_reconciliation_repairs' and indexdef ilike '%unique%' \
         and indexdef ilike '%snapshot_id%' and indexdef ilike '%bookmark_id%'",
    )
    .fetch_one(database.pool())
    .await
    .expect("the repair index catalog is readable");

    database.pool().close().await;
    drop_disposable_database(&name, &admin).await;
    admin.close().await;

    assert!(
        missing.is_empty(),
        "incremental scans need durable state and repair evidence; missing {missing:?}"
    );
    assert!(
        repair_identity_is_unique,
        "a completed snapshot may record each bookmark repair only once"
    );
}

#[tokio::test]
async fn constraints_stay_within_x_archive_boundary() {
    let (url, name, admin) = create_disposable_database().await;
    let database = Database::connect(&url, 2)
        .await
        .expect("a pooled connection");
    database.apply_schema().await.expect("the schema applies");

    let foreign_foreign_keys: i64 = sqlx::query_scalar(
        "select count(*) from pg_constraint con \
         join pg_class rel on rel.oid = con.conrelid \
         join pg_namespace rns on rns.oid = rel.relnamespace \
         left join pg_class frel on frel.oid = con.confrelid \
         left join pg_namespace fns on fns.oid = frel.relnamespace \
         where con.contype = 'f' and rns.nspname = 'x_archive' \
           and coalesce(fns.nspname, '') <> 'x_archive'",
    )
    .fetch_one(database.pool())
    .await
    .expect("constraint catalog readable");

    let mut missing_uniqueness = Vec::new();
    for (table, column) in [
        ("accounts", "provider_user_id"),
        ("users", "provider_id"),
        ("posts", "provider_id"),
        ("bookmarks", "post_id"),
        ("bookmark_folders", "provider_id"),
    ] {
        let present = sqlx::query_scalar::<_, bool>(
            "select count(*) > 0 from pg_indexes \
             where schemaname = 'x_archive' and tablename = $1 \
               and indexdef ilike '%unique%' and indexdef ilike $2",
        )
        .bind(table)
        .bind(format!("%{column}%"))
        .fetch_one(database.pool())
        .await
        .expect("index catalog readable");
        if !present {
            missing_uniqueness.push(format!("{table}.{column}"));
        }
    }

    database.pool().close().await;
    drop_disposable_database(&name, &admin).await;
    admin.close().await;

    assert_eq!(
        foreign_foreign_keys, 0,
        "no foreign key may leave x_archive"
    );
    assert!(
        missing_uniqueness.is_empty(),
        "provider identity uniqueness missing on {missing_uniqueness:?}"
    );
}
