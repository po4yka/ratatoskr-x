//! Typed failures of the bootstrap sequence and the process exit codes they map to.

/// Why the service failed to start.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BootstrapError {
    /// Configuration loading or validation refused to continue.
    #[error("configuration was rejected: {0}")]
    Config(#[from] x_core::error::ConfigError),
    /// Telemetry could not be installed.
    #[error("telemetry could not start: {0}")]
    Telemetry(#[from] x_telemetry::error::TelemetryError),
    /// The database pool or schema application failed.
    #[error("database could not start: {0}")]
    Persistence(#[from] x_persistence::error::PersistenceError),
    /// The broker `NKey` seed file could not be read.
    #[error("the NATS credential could not be read")]
    NatsSeed(#[source] std::io::Error),
    /// The broker connection or X command consumer could not start.
    #[error("the NATS command consumer could not start: {0}")]
    Nats(String),
    /// The admin listener could not bind.
    #[error("the admin listener could not bind")]
    Listener(#[source] std::io::Error),
}

impl BootstrapError {
    /// The process exit status this failure maps to: `EX_CONFIG` (78) only for configuration
    /// rejection, one for every other failing subsystem.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Config(_) => 78,
            Self::Telemetry(_)
            | Self::Persistence(_)
            | Self::NatsSeed(_)
            | Self::Nats(_)
            | Self::Listener(_) => 1,
        }
    }

    /// Operator-readable description without any secret values.
    #[must_use]
    pub fn report(&self) -> String {
        self.to_string()
    }
}
