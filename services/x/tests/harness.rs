//! The disposable-database harness creates an isolated database from `schema.sql` and removes
//! it on cleanup, per the `x-archive-schema` delta.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use x_persistence::test_support::TestDatabase;

#[tokio::test]
async fn test_database_harness_creates_and_removes_isolated_database() {
    let admin_url = TestDatabase::admin_url();
    let test = TestDatabase::create()
        .await
        .expect("the harness creates an isolated database");
    assert!(
        test.name().starts_with("ratatoskr_x_test_"),
        "generated names are namespaced: {}",
        test.name()
    );

    test.database
        .query_raw("select 1")
        .await
        .expect("the harness database serves queries");

    let name = test.name().to_owned();
    test.cleanup().await.expect("cleanup drops the database");

    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await
        .expect("an administrative connection after cleanup");
    let remaining: i64 = sqlx::query_scalar("select count(*) from pg_database where datname = $1")
        .bind(&name)
        .fetch_one(&admin)
        .await
        .expect("the catalog is readable");
    admin.close().await;

    assert_eq!(remaining, 0, "the disposable database no longer exists");
}
