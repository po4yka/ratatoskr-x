//! Typed failures raised while installing telemetry.

/// Why telemetry installation failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TelemetryError {
    /// The configured filter expression is not a valid directive list.
    #[error("the log filter could not be parsed")]
    Filter(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// A global subscriber was already installed in this process.
    #[error("telemetry was already installed")]
    AlreadyInstalled,
    /// The Prometheus recorder could not be built.
    #[error("the metrics recorder could not be built")]
    Recorder(#[source] metrics_exporter_prometheus::BuildError),
}
