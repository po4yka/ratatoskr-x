//! Durable, provenance-preserving X browser-capture command handling.

#![allow(clippy::expect_used, clippy::panic, reason = "test assertions")]

use ratatoskr_event_envelope::CommandEnvelope;
use x_capture::{Delivery, persist_browser_capture};
use x_persistence::test_support::TestDatabase;

const X_COMMAND: &str = r#"{
  "command_id": "018f0000-0000-7000-8000-000000000001",
  "command_type": "social.capture.requested.v1",
  "issued_at": "2026-08-27T10:00:00Z",
  "producer": "ratatoskr-platform",
  "aggregate_id": "operation:018f0000-0000-7000-8000-000000000002",
  "correlation_id": "operation:018f0000-0000-7000-8000-000000000002",
  "tenant_id": "user:018f0000-0000-7000-8000-000000000003",
  "schema_version": 1,
  "payload": {
    "operation_id": "018f0000-0000-7000-8000-000000000002",
    "idempotency_key": {"algorithm": "sha256", "hex": "0000000000000000000000000000000000000000000000000000000000000000"},
    "original_permalink": "https://x.com/ratatoskr/status/1",
    "captured_at": "2026-08-27T10:00:00Z",
    "provider": "x",
    "acquisition": "browser_extension",
    "saved_authority": "explicit_user_capture"
  }
}"#;

#[tokio::test]
async fn persists_explicit_provenance_once_for_an_at_least_once_delivery() {
    let database = TestDatabase::create().await.expect("test database");
    let command = CommandEnvelope::from_json(X_COMMAND.as_bytes()).expect("command parses");

    assert_eq!(
        persist_browser_capture(&database.database, &command)
            .await
            .expect("first delivery persists"),
        Delivery::Applied,
    );
    assert_eq!(
        persist_browser_capture(&database.database, &command)
            .await
            .expect("second delivery reads inbox"),
        Delivery::Duplicate,
    );

    let record: (String, String, String, String) = sqlx::query_as(
        "select original_permalink, acquisition, saved_authority, operation_id \
         from x_archive.explicit_captures",
    )
    .fetch_one(database.database.pool())
    .await
    .expect("one capture row");
    assert_eq!(record.0, "https://x.com/ratatoskr/status/1");
    assert_eq!(record.1, "browser_extension");
    assert_eq!(record.2, "explicit_user_capture");
    assert_eq!(record.3, "018f0000-0000-7000-8000-000000000002");

    database.cleanup().await.expect("test database cleanup");
}
