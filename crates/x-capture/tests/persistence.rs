//! Durable, provenance-preserving X browser-capture command handling.

#![allow(clippy::expect_used, clippy::panic, reason = "test assertions")]

use ratatoskr_event_envelope::{CommandEnvelope, EventEnvelope};
use ratatoskr_operation_contracts::{OperationReported, OperationStage, OperationStatus};
use x_capture::{
    CaptureCommandError, Delivery, DeliveryError, persist_browser_capture, status_id_from_permalink,
};
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
        "select original_permalink, acquisition, saved_authority, operation_id::text \
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

const OWNER: &str = "018f0000-0000-7000-8000-000000000003";
const OPERATION: &str = "018f0000-0000-7000-8000-000000000002";

#[tokio::test]
async fn persists_the_tenant_as_owner_and_queues_the_operation() {
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

    let capture: (uuid::Uuid, String, Option<String>, String, bool) = sqlx::query_as(
        "select owner, operation_id::text, provider_post_id, status, \
                next_attempt_at <= now() from x_archive.explicit_captures",
    )
    .fetch_one(database.database.pool())
    .await
    .expect("one capture row");
    assert_eq!(capture.0.to_string(), OWNER, "the tenant is the owner");
    assert_eq!(capture.1, OPERATION);
    assert_eq!(capture.2.as_deref(), Some("1"), "the status id is kept");
    assert_eq!(capture.3, "accepted");
    assert!(capture.4, "the capture is due immediately");

    let reports: Vec<(uuid::Uuid, String, serde_json::Value)> = sqlx::query_as(
        "select id, event_type, payload from x_archive.outbox_events order by created_at, id",
    )
    .fetch_all(database.database.pool())
    .await
    .expect("outbox rows");
    assert_eq!(reports.len(), 1, "redelivery adds no second report");
    let (row_id, event_type, payload) = &reports.first().expect("the element exists");
    assert_eq!(event_type, "platform.operation.reported.v1");
    let envelope =
        EventEnvelope::from_json(payload.to_string().as_bytes()).expect("a complete envelope");
    assert_eq!(envelope.event_id.0, *row_id);
    assert_eq!(envelope.producer.as_str(), "ratatoskr-x");
    assert_eq!(
        envelope.tenant_id.map(|tenant| tenant.to_string()),
        Some(format!("user:{OWNER}"))
    );
    assert_eq!(
        envelope
            .causation_id
            .as_ref()
            .map(ratatoskr_identifiers::EntityRef::to_wire),
        Some("command:018f0000-0000-7000-8000-000000000001".to_owned())
    );
    let report: OperationReported = envelope.payload_as().expect("an operation report");
    assert_eq!(report.operation_id.to_string(), OPERATION);
    assert_eq!(report.status, OperationStatus::Queued);
    assert_eq!(
        report.stage.as_ref().map(OperationStage::as_str),
        Some("capture_queued")
    );

    database.cleanup().await.expect("test database cleanup");
}

#[test]
fn every_status_host_platform_accepts_yields_the_status_id() {
    for permalink in [
        "https://x.com/ratatoskr/status/1900000000000000001",
        "https://www.x.com/ratatoskr/status/1900000000000000001",
        "https://mobile.x.com/ratatoskr/status/1900000000000000001",
        "https://twitter.com/ratatoskr/status/1900000000000000001",
        "https://www.twitter.com/ratatoskr/status/1900000000000000001",
        "https://mobile.twitter.com/ratatoskr/status/1900000000000000001?s=20",
        "https://x.com/i/web/status/1900000000000000001",
    ] {
        assert_eq!(
            status_id_from_permalink(permalink),
            Some("1900000000000000001"),
            "{permalink}"
        );
    }
    for permalink in [
        "https://x.com/ratatoskr",
        "https://x.com/ratatoskr/status/",
        "https://x.com/ratatoskr/status/abc",
        "https://example.com/ratatoskr/status/1",
        "https://x.com.evil.test/ratatoskr/status/1",
        "https://x.com/ratatoskr/status/12345678901234567890",
    ] {
        assert_eq!(status_id_from_permalink(permalink), None, "{permalink}");
    }
}

#[tokio::test]
async fn a_command_without_a_tenant_is_rejected_as_missing_tenant() {
    let database = TestDatabase::create().await.expect("test database");
    let mut value: serde_json::Value = serde_json::from_str(X_COMMAND).expect("fixture is JSON");
    value
        .as_object_mut()
        .expect("the fixture is an object")
        .remove("tenant_id");
    let command = CommandEnvelope::from_json(value.to_string().as_bytes())
        .expect("a tenant-less command is a valid envelope");

    let refusal = persist_browser_capture(&database.database, &command)
        .await
        .expect_err("a command without an owner is a poison command");
    assert!(
        matches!(
            refusal,
            DeliveryError::Command(CaptureCommandError::MissingTenant)
        ),
        "{refusal:?}"
    );
    let rows: i64 = sqlx::query_scalar(
        "select (select count(*) from x_archive.explicit_captures) \
              + (select count(*) from x_archive.inbox_events)",
    )
    .fetch_one(database.database.pool())
    .await
    .expect("row count");
    assert_eq!(rows, 0, "a refused command leaves nothing behind");

    database.cleanup().await.expect("test database cleanup");
}

#[tokio::test]
async fn a_permalink_without_a_status_id_is_reported_unavailable() {
    let database = TestDatabase::create().await.expect("test database");
    let command = CommandEnvelope::from_json(
        X_COMMAND
            .replace(
                "https://x.com/ratatoskr/status/1",
                "https://x.com/ratatoskr",
            )
            .as_bytes(),
    )
    .expect("command parses");

    assert_eq!(
        persist_browser_capture(&database.database, &command)
            .await
            .expect("an attributable command is accepted"),
        Delivery::Applied,
    );

    let capture: (String, Option<String>, bool) = sqlx::query_as(
        "select status, provider_post_id, reported_at is not null \
         from x_archive.explicit_captures",
    )
    .fetch_one(database.database.pool())
    .await
    .expect("one capture row");
    assert_eq!(capture, ("unavailable".to_owned(), None, true));

    let payloads: Vec<serde_json::Value> =
        sqlx::query_scalar("select payload from x_archive.outbox_events order by created_at, id")
            .fetch_all(database.database.pool())
            .await
            .expect("outbox rows");
    assert_eq!(payloads.len(), 1, "exactly one terminal report");
    let envelope = EventEnvelope::from_json(
        payloads
            .first()
            .expect("the element exists")
            .to_string()
            .as_bytes(),
    )
    .expect("a complete envelope");
    let report: OperationReported = envelope.payload_as().expect("an operation report");
    assert_eq!(report.status, OperationStatus::Failed);
    let error = report.error.expect("a failed report carries its error");
    assert_eq!(error.code.as_str(), "social.source.unavailable");
    assert!(!error.retryable, "an unmappable permalink is permanent");

    database.cleanup().await.expect("test database cleanup");
}
