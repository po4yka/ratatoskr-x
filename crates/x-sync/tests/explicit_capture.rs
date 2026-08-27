//! Explicit browser capture provenance.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use ratatoskr_event_envelope::CommandEnvelope;
use x_persistence::test_support::TestDatabase;
use x_sync::ExplicitCaptureService;

#[tokio::test]
async fn x_social_capture_command_preserves_explicit_provenance() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("explicit-capture-account")
        .await
        .expect("account seeds");
    let owner: uuid::Uuid =
        sqlx::query_scalar("select internal_user_id from x_archive.accounts where id = $1")
            .bind(account)
            .fetch_one(test.database.pool())
            .await
            .expect("the account owner is readable");
    let author: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.users (provider_id, parser_version) values ('author-1', 1) returning id",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("author seeds");
    sqlx::query(
        "insert into x_archive.posts (provider_id, author_user_id, text, parser_version, expanded_urls) \
         values ('123456789', $1, 'captured in browser', 1, $2::jsonb)",
    )
    .bind(author)
    .bind("[\"https://example.test/explicit-article\"]")
    .execute(test.database.pool())
    .await
    .expect("normalized post seeds");
    let command: CommandEnvelope = serde_json::from_value(serde_json::json!({
        "command_id": "018f0000-0000-7000-8000-000000000701",
        "command_type": "social.capture.requested.v1",
        "issued_at": "2026-08-27T09:30:00Z",
        "producer": "ratatoskr-platform",
        "aggregate_id": "x-post:123456789",
        "correlation_id": "operation:018f0000-0000-7000-8000-000000000702",
        "tenant_id": format!("user:{owner}"),
        "schema_version": 1,
        "payload": {
            "operation_id": "018f0000-0000-7000-8000-000000000702",
            "idempotency_key": {"algorithm": "sha256", "hex": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"},
            "original_permalink": "https://x.com/author/status/123456789",
            "captured_at": "2026-08-27T09:30:00Z",
            "provider": "x",
            "acquisition": "browser_extension",
            "saved_authority": "explicit_user_capture"
        }
    }))
    .expect("the pinned shared command fixture decodes");

    ExplicitCaptureService::new(test.database.clone())
        .apply(account, command)
        .await
        .expect("a valid X command is accepted");
    let provenance: (String, String) = sqlx::query_as(
        "select acquisition, saved_authority from x_archive.social_sources where account_id = $1",
    )
    .bind(account)
    .fetch_one(test.database.pool())
    .await
    .expect("the explicit source is persisted");
    let capture_url: Option<String> = sqlx::query_scalar(
        "select payload->'payload'->>'url' from x_archive.outbox_events \
         where event_type = 'content.capture.requested.v1'",
    )
    .fetch_optional(test.database.pool())
    .await
    .expect("the explicit article capture is persisted");
    test.cleanup().await.expect("cleanup drops the database");

    assert_eq!(
        provenance,
        (
            "browser_extension".to_owned(),
            "explicit_user_capture".to_owned()
        )
    );
    assert_eq!(
        capture_url.as_deref(),
        Some("https://example.test/explicit-article"),
        "explicit capture delegates the normalized post's external article"
    );
}

#[tokio::test]
async fn redelivered_command_does_not_duplicate_source_event() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("explicit-redelivery-account")
        .await
        .expect("account seeds");
    let owner: uuid::Uuid =
        sqlx::query_scalar("select internal_user_id from x_archive.accounts where id = $1")
            .bind(account)
            .fetch_one(test.database.pool())
            .await
            .expect("owner reads");
    let author: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.users (provider_id, parser_version) values ('redelivery-author', 1) returning id",
    ).fetch_one(test.database.pool()).await.expect("author seeds");
    sqlx::query("insert into x_archive.posts (provider_id, author_user_id, text, parser_version) values ('987654321', $1, 'browser capture', 1)")
        .bind(author).execute(test.database.pool()).await.expect("post seeds");
    let command: CommandEnvelope = serde_json::from_value(serde_json::json!({
        "command_id": "018f0000-0000-7000-8000-000000000703", "command_type": "social.capture.requested.v1",
        "issued_at": "2026-08-27T09:30:00Z", "producer": "ratatoskr-platform", "aggregate_id": "x-post:987654321",
        "correlation_id": "operation:018f0000-0000-7000-8000-000000000704", "tenant_id": format!("user:{owner}"), "schema_version": 1,
        "payload": {"operation_id": "018f0000-0000-7000-8000-000000000704", "idempotency_key": {"algorithm":"sha256","hex":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}, "original_permalink":"https://x.com/author/status/987654321", "captured_at":"2026-08-27T09:30:00Z", "provider":"x", "acquisition":"browser_extension", "saved_authority":"explicit_user_capture"}
    })).expect("fixture decodes");
    let service = ExplicitCaptureService::new(test.database.clone());
    service
        .apply(account, command.clone())
        .await
        .expect("first delivery applies");
    service
        .apply(account, command)
        .await
        .expect("redelivery is accepted");
    let events: i64 = sqlx::query_scalar("select count(*) from x_archive.outbox_events where event_type = 'social.source.captured.v1'")
        .fetch_one(test.database.pool()).await.expect("outbox reads");
    test.cleanup().await.expect("cleanup drops database");
    assert_eq!(
        events, 1,
        "delivery deduplication prevents a second source event"
    );
}
