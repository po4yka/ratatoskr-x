//! Media metadata normalization against the committed media fixture: kinds,
//! dimensions, alt text, and video duration map through, records link to
//! their owning post by provider id, and blob references stay unset.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::path::PathBuf;

use x_normalize::dto::Envelope;
use x_normalize::normalize::normalize;
use x_normalize::records::MediaKind;

/// Decodes a committed fixture into the envelope.
fn decode(name: &str) -> Envelope {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/x-api")
        .join(name);
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("fixture {name} must be readable: {e}"));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("fixture {name} must decode: {e}"))
}

#[test]
fn attachments_yield_metadata_without_blob_references() {
    let envelope = decode("media_post.json");
    let batch = normalize(&envelope).expect("the media envelope normalizes");

    assert_eq!(batch.media().len(), 2, "both attachments normalize");

    let photo = batch
        .media()
        .iter()
        .find(|m| m.provider_id == "7_1000000000000000001")
        .expect("the photo record emerges");
    assert_eq!(photo.post_provider_id, "600700800");
    assert_eq!(photo.kind, Some(MediaKind::Photo));
    assert_eq!(photo.blob_ref, None, "bytes are never implied");
    let width = photo.metadata.get("width").cloned().unwrap_or_default();
    assert_eq!(width, serde_json::json!(1200));
    let alt = photo.metadata.get("alt_text").cloned().unwrap_or_default();
    assert_eq!(
        alt,
        serde_json::json!("A synthetic diagram of the normalization pipeline.")
    );

    let video = batch
        .media()
        .iter()
        .find(|m| m.provider_id == "7_1000000000000000002")
        .expect("the video record emerges");
    assert_eq!(video.kind, Some(MediaKind::Video));
    let duration = video
        .metadata
        .get("duration_ms")
        .cloned()
        .unwrap_or_default();
    assert_eq!(duration, serde_json::json!(45000));
    let preview = video
        .metadata
        .get("preview_image_url")
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        preview,
        serde_json::json!("https://fixture.example/media/video-1-preview.jpg")
    );
}
