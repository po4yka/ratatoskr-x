//! The authorization-intents repository round-trips intents through the state digest.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::time::Duration;

use x_persistence::database::Database;
use x_persistence::oauth_intents::{IntentRow, NewIntent};

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
/// # Panics
/// Panics when the administrative connection or `CREATE DATABASE` fails; the suite cannot
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
        .acquire_timeout(Duration::from_secs(5))
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

async fn fresh_database() -> (Database, String, sqlx::PgPool) {
    let (url, name, admin) = create_disposable_database().await;
    let database = Database::connect(&url, 2)
        .await
        .expect("a pooled connection");
    database.apply_schema().await.expect("the schema applies");
    (database, name, admin)
}

#[tokio::test]
async fn intent_round_trips_through_digest_lookup() {
    let (database, name, admin) = fresh_database().await;
    let created_at = sqlx::types::chrono::DateTime::parse_from_rfc3339("2026-08-26T12:00:00Z")
        .expect("a fixed instant")
        .with_timezone(&sqlx::types::chrono::Utc);
    let scopes = vec![
        "users.read".to_owned(),
        "tweet.read".to_owned(),
        "bookmark.read".to_owned(),
        "offline.access".to_owned(),
    ];
    let intent = NewIntent {
        internal_user_id: uuid::Uuid::from_u128(0x4242),
        state_hash: "9408fb980ecb806c67bd33721112861ef1c14736ffd1a0b12842ff9f98b46749",
        code_verifier_encrypted: b"sealed-verifier-bytes",
        nonce: "nonce-observation",
        redirect_uri: "https://app.example/callback",
        requested_scopes: &scopes,
        created_at,
        expires_at: created_at + std::time::Duration::from_mins(10),
    };

    let inserted = x_persistence::oauth_intents::insert_intent(&database, &intent)
        .await
        .expect("the insert succeeds");
    assert!(
        inserted != uuid::Uuid::nil(),
        "the repository returns a generated identity"
    );

    let found: Option<IntentRow> =
        x_persistence::oauth_intents::find_intent_by_state_hash(&database, intent.state_hash)
            .await
            .expect("the lookup query succeeds");
    let row = found.expect("the inserted intent must be found by its state digest");
    assert_eq!(row.id, inserted, "the row identity matches");
    assert_eq!(
        row.internal_user_id, intent.internal_user_id,
        "the internal-user binding survives"
    );
    assert_eq!(
        row.code_verifier_encrypted, intent.code_verifier_encrypted,
        "the sealed verifier bytes survive"
    );
    assert_eq!(row.redirect_uri, intent.redirect_uri);
    assert_eq!(row.requested_scopes, intent.requested_scopes);
    assert_eq!(row.created_at, created_at);
    assert_eq!(
        row.expires_at,
        created_at + std::time::Duration::from_mins(10)
    );
    assert!(row.consumed_at.is_none(), "a fresh intent is unconsumed");

    let unseen = x_persistence::oauth_intents::find_intent_by_state_hash(
        &database,
        "0000000000000000000000000000000000000000000000000000000000000000",
    )
    .await
    .expect("the lookup query succeeds for unseen digests too");
    assert!(unseen.is_none(), "no digest means no row");

    database.pool().close().await;
    drop_disposable_database(&name, &admin).await;
    admin.close().await;
}
