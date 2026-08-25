//! Structured telemetry for `ratatoskr-x`: tracing output and the Prometheus recorder.
//!
//! Building never touches process-global state, so tests can construct a full stack with an
//! injected writer; only [`Telemetry::install`] claims the process-global dispatcher and
//! recorder.

pub mod error;

use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle, PrometheusRecorder};
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::Layer as _;
use tracing_subscriber::layer::SubscriberExt as _;
use x_core::config::LogFormat;

/// A fully built but not yet process-global telemetry stack.
pub struct Telemetry {
    /// The subscriber installed as the process-global default on [`Telemetry::install`].
    pub subscriber: std::sync::Arc<dyn tracing::Subscriber + Send + Sync>,
    /// The recorder registered as the process-global recorder on [`Telemetry::install`].
    pub recorder: PrometheusRecorder,
    /// Renders the Prometheus text exposition served by the admin router.
    pub metrics_handle: PrometheusHandle,
}

impl std::fmt::Debug for Telemetry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Telemetry").finish_non_exhaustive()
    }
}

impl Telemetry {
    /// Claims the process-global subscriber and metrics recorder.
    ///
    /// # Errors
    /// When either global slot is already occupied.
    pub fn install(self) -> Result<TelemetryGuard, error::TelemetryError> {
        tracing::subscriber::set_global_default(self.subscriber)
            .map_err(|_| error::TelemetryError::AlreadyInstalled)?;
        metrics::set_global_recorder(self.recorder)
            .map_err(|_| error::TelemetryError::AlreadyInstalled)?;
        Ok(TelemetryGuard {
            metrics_handle: self.metrics_handle,
        })
    }

    /// Renders the Prometheus text exposition without touching any global state.
    #[must_use]
    pub fn metrics_render(&self) -> String {
        self.metrics_handle.render()
    }
}

/// Owns the resources telemetry installed into the process until shutdown.
#[derive(Debug)]
pub struct TelemetryGuard {
    metrics_handle: PrometheusHandle,
}

impl TelemetryGuard {
    /// Renders the Prometheus text exposition served by the admin router.
    #[must_use]
    pub fn render_metrics(&self) -> String {
        self.metrics_handle.render()
    }

    /// Releases exporter resources. Idempotent by construction: the guard is consumed.
    pub fn shutdown(self) {}
}

/// Builds a telemetry stack that logs in the given format through the given filter to stdout.
///
/// # Errors
/// When the filter expression cannot be parsed or the recorder cannot be built.
pub fn build(log_format: LogFormat, log_filter: &str) -> Result<Telemetry, error::TelemetryError> {
    build_with_writer(log_format, log_filter, std::io::stdout)
}

/// Builds a telemetry stack that logs through any injected writer.
///
/// # Errors
/// When the filter expression cannot be parsed or the recorder cannot be built.
pub fn build_with_writer<W>(
    log_format: LogFormat,
    log_filter: &str,
    writer: W,
) -> Result<Telemetry, error::TelemetryError>
where
    W: for<'writer> MakeWriter<'writer> + Send + Sync + 'static,
{
    let filter = EnvFilter::try_new(log_filter)
        .map_err(|error| error::TelemetryError::Filter(Box::new(error)))?;
    let format_layer = match log_format {
        LogFormat::Json => tracing_subscriber::fmt::layer()
            .json()
            .with_current_span(true)
            .with_span_list(false)
            .with_writer(writer)
            .boxed(),
        LogFormat::Pretty => tracing_subscriber::fmt::layer()
            .pretty()
            .with_writer(writer)
            .boxed(),
    };
    let subscriber = std::sync::Arc::new(
        tracing_subscriber::registry()
            .with(filter)
            .with(format_layer),
    );
    let recorder = PrometheusBuilder::new().build_recorder();
    let metrics_handle = recorder.handle();
    Ok(Telemetry {
        subscriber,
        recorder,
        metrics_handle,
    })
}
