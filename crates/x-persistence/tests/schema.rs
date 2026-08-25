//! `schema.sql` creates exactly the owned inventory, applies idempotently, and stays inside
//! the bounded context. Integration tests build disposable databases from the same definition
//! the binary embeds.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use x_persistence::database::Database;

/// The fifteen tables the owned schema must contain, no more and no fewer.
const OWNED_TABLES: &[&str] = &[
    "accounts",
    "bookmark_folder_items",
    "bookmark_folders",
    "bookmarks",
    "credentials",
    "inbox_events",
    "media",
    "outbox_events",
    "post_relations",
    "posts",
    "rate_limit_state",
    "snapshots",
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
