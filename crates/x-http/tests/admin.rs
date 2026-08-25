//! The admin router behaves like the `service-bootstrap` spec says it does.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;
use x_http::admin::{RuntimeState, admin_router};

async fn get(path: &'static str) -> axum::response::Response {
    let router = admin_router(Arc::new(RuntimeState::new()), || String::new());
    let request = Request::builder()
        .uri(path)
        .body(Body::empty())
        .expect("a valid request");
    router
        .oneshot(request)
        .await
        .expect("the router is infallible")
}

async fn body_string(response: &mut axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(std::mem::take(response.body_mut()), 1 << 20)
        .await
        .expect("a readable body");
    String::from_utf8(bytes.to_vec()).expect("utf8 bodies")
}

#[tokio::test]
async fn live_reports_running_state() {
    let mut response = get("/health/live").await;
    assert_eq!(response.status(), StatusCode::OK, "liveness while running");
    let body = body_string(&mut response).await;
    assert!(body.contains("live"), "liveness state is reported: {body}");
}

#[tokio::test]
async fn ready_tracks_initialization_and_names_checks() {
    let router = admin_router(Arc::new(RuntimeState::new()), || String::new());
    let request = Request::builder()
        .uri("/health/ready")
        .body(Body::empty())
        .expect("request");
    let mut before = router.oneshot(request).await.expect("infallible");
    assert_eq!(
        before.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "not initialized yet"
    );
    let before_body = body_string(&mut before).await;
    assert!(
        before_body.contains("database"),
        "unmet checks are named: {before_body}"
    );

    let state = Arc::new(RuntimeState::new());
    state.mark_database_ready();
    let router = admin_router(state.clone(), || String::new());
    let request = Request::builder()
        .uri("/health/ready")
        .body(Body::empty())
        .expect("request");
    let mut after = router.oneshot(request).await.expect("infallible");
    assert_eq!(after.status(), StatusCode::OK, "initialized now");
    let after_body = body_string(&mut after).await;
    assert!(
        after_body.contains("ready"),
        "readiness is reported: {after_body}"
    );
}

#[tokio::test]
async fn metrics_serves_prometheus_exposition() {
    let router = admin_router(Arc::new(RuntimeState::new()), || {
        "ratatoskr_x_bootstrap_total 1".to_owned()
    });
    let request = Request::builder()
        .uri("/metrics")
        .body(Body::empty())
        .expect("request");
    let mut response = router.oneshot(request).await.expect("infallible");
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert!(
        content_type.starts_with("text/plain; version=0.0.4"),
        "prometheus exposition content type: {content_type}"
    );
    let body = body_string(&mut response).await;
    assert!(
        body.contains("ratatoskr_x_bootstrap_total"),
        "counter rendered: {body}"
    );
}

#[tokio::test]
async fn version_identifies_service_build() {
    let mut response = get("/version").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_string(&mut response).await;
    for expected in [
        "\"ratatoskr-x\"",
        x_core::identity::VERSION,
        x_core::identity::GIT_SHA,
        x_core::identity::RUST_VERSION,
    ] {
        assert!(
            body.contains(expected),
            "version reports {expected}: {body}"
        );
    }
}

#[tokio::test]
async fn state_endpoints_disable_caching() {
    for path in ["/health/live", "/health/ready", "/metrics", "/version"] {
        let mut response = if path == "/metrics" {
            let router = admin_router(Arc::new(RuntimeState::new()), || String::from("# HELP x"));
            let request = Request::builder()
                .uri(path)
                .body(Body::empty())
                .expect("request");
            router.oneshot(request).await.expect("infallible")
        } else {
            get(path).await
        };
        let cache_control = response
            .headers()
            .get(axum::http::header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        assert_eq!(cache_control, "no-store", "{path} must not be cached");
        body_string(&mut response).await;
    }
}
