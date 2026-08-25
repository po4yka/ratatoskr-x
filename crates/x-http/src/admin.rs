//! Process-state endpoints: liveness, readiness, metrics, and version.
//!
//! The router owns no exporter dependency: metrics are rendered through a closure the caller
//! supplies, exactly like the sibling services do it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Serialize;
use x_core::identity;

/// The lifecycle phase after bootstrap completed.
const READY: u8 = 1;
/// The lifecycle phase once shutdown started; readiness never returns during it.
const DRAINING: u8 = 2;

/// The Prometheus text exposition media type the `/metrics` endpoint answers with.
pub const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// Process lifecycle and dependency-check state shared by the handlers.
///
/// Readiness succeeds when every named dependency check reports met, and never during drain.
#[derive(Debug, Default)]
pub struct RuntimeState {
    lifecycle: AtomicU8,
    database_ready: AtomicBool,
}

impl RuntimeState {
    /// A fresh state: starting, with every dependency check unmet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks the database dependency as met.
    pub fn mark_database_ready(&self) {
        self.database_ready.store(true, Ordering::Release);
    }

    /// Marks bootstrap complete.
    pub fn set_ready(&self) {
        self.lifecycle.store(READY, Ordering::Release);
    }

    /// Marks shutdown as started.
    pub fn begin_drain(&self) {
        self.lifecycle.store(DRAINING, Ordering::Release);
    }

    /// Whether shutdown has started.
    #[must_use]
    pub fn is_draining(&self) -> bool {
        self.lifecycle.load(Ordering::Acquire) == DRAINING
    }

    fn readiness(&self) -> (bool, Vec<CheckStatus>) {
        let database = self.database_ready.load(Ordering::Acquire);
        let checks = vec![CheckStatus {
            name: "database".to_owned(),
            ready: database,
        }];
        let draining = self.is_draining();
        (!draining && database, checks)
    }
}

/// Renders the Prometheus text exposition without owning an exporter.
type RenderMetrics = Arc<dyn Fn() -> String + Send + Sync>;

/// Everything the handlers reach through axum state.
#[derive(Clone)]
struct AdminState {
    runtime: Arc<RuntimeState>,
    render_metrics: RenderMetrics,
}

/// Builds the admin router serving `/health/live`, `/health/ready`, `/metrics`, and `/version`.
#[must_use]
pub fn admin_router<R>(state: Arc<RuntimeState>, render_metrics: R) -> Router
where
    R: Fn() -> String + Send + Sync + 'static,
{
    let admin = AdminState {
        runtime: state,
        render_metrics: Arc::new(render_metrics),
    };
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/metrics", get(metrics))
        .route("/version", get(version))
        .with_state(Arc::new(admin))
        .layer(middleware::map_response(no_store))
}

/// Adds `Cache-Control: no-store` to every process-state response.
async fn no_store(response: Response) -> Response {
    let (mut parts, body) = response.into_parts();
    parts
        .headers
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Response::from_parts(parts, body)
}

#[derive(Serialize)]
struct Liveness {
    state: &'static str,
}

async fn live() -> Json<Liveness> {
    Json(Liveness { state: "live" })
}

#[derive(Serialize)]
struct CheckStatus {
    name: String,
    ready: bool,
}

#[derive(Serialize)]
struct Readiness {
    state: &'static str,
    checks: Vec<CheckStatus>,
}

async fn ready(State(admin): State<Arc<AdminState>>) -> Response {
    let (is_ready, checks) = admin.runtime.readiness();
    let body = Readiness {
        state: if is_ready { "ready" } else { "not_ready" },
        checks,
    };
    if is_ready {
        (StatusCode::OK, Json(body)).into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response()
    }
}

async fn metrics(State(admin): State<Arc<AdminState>>) -> Response {
    let exposition = (admin.render_metrics)();
    (
        [(header::CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE)],
        exposition,
    )
        .into_response()
}

#[derive(Serialize)]
struct VersionInfo {
    service: String,
    version: String,
    git_sha: String,
    rust_version: String,
}

async fn version() -> Json<VersionInfo> {
    Json(VersionInfo {
        service: identity::SERVICE_NAME.to_owned(),
        version: identity::VERSION.to_owned(),
        git_sha: identity::GIT_SHA.to_owned(),
        rust_version: identity::RUST_VERSION.to_owned(),
    })
}
