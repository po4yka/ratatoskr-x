//! X ownership checks for the explicit browser social-capture command.

#![allow(clippy::expect_used, clippy::panic, reason = "test assertions")]

use ratatoskr_event_envelope::CommandEnvelope;
use x_capture::{CaptureCommandError, validate_browser_capture};

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

#[test]
fn accepts_x_and_rejects_a_different_social_owner() {
    let x = CommandEnvelope::from_json(X_COMMAND.as_bytes()).expect("X command parses");
    assert_eq!(validate_browser_capture(&x), Ok(()));

    let instagram = CommandEnvelope::from_json(
        X_COMMAND
            .replace("\"provider\": \"x\"", "\"provider\": \"instagram\"")
            .as_bytes(),
    )
    .expect("other owner command parses");
    assert_eq!(
        validate_browser_capture(&instagram),
        Err(CaptureCommandError::WrongProvider),
    );
}
