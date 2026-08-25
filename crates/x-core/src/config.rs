//! Typed finite configuration read from `RATATOSKR__`-prefixed environment variables.
//!
//! The declared tree is closed: any variable outside it is refused, and semantically invalid
//! values are reported together before the process starts anything else.

use figment::Figment;
use figment::providers::{Env, Serialized};
use serde::{Deserialize, Serialize};

use crate::error::{ConfigError, Violations};

/// The prefix every environment override carries.
pub const ENV_PREFIX: &str = "RATATOSKR__";

/// The whole runtime configuration of the service.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XConfig {
    /// The admin listener that serves process-state endpoints.
    pub admin: AdminConfig,
    /// The `PostgreSQL` database this service owns its schema in.
    pub database: DatabaseConfig,
    /// Structured logging and metrics output.
    pub telemetry: TelemetryConfig,
}

/// Admin listener settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminConfig {
    /// Socket address such as `127.0.0.1:8080` the admin listener binds.
    pub listen_addr: String,
}

/// Database settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    /// Connection URL; never logged.
    pub url: String,
    /// Upper bound on pooled connections.
    pub max_connections: u32,
}

/// Telemetry settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetryConfig {
    /// Output format for log events.
    pub log_format: LogFormat,
    /// Filter directive list such as `info` or `ratatoskr_x=debug`.
    pub log_filter: String,
}

/// The machine-readable or human-readable log output style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogFormat {
    /// One JSON object per event on standard output.
    Json,
    /// Colorized single-line human output.
    Pretty,
}

impl Default for XConfig {
    fn default() -> Self {
        Self {
            admin: AdminConfig {
                listen_addr: "127.0.0.1:8080".to_owned(),
            },
            database: DatabaseConfig {
                url: "postgres://x:x@127.0.0.1:5432/x".to_owned(),
                max_connections: 5,
            },
            telemetry: TelemetryConfig {
                log_format: LogFormat::Json,
                log_filter: "info".to_owned(),
            },
        }
    }
}

impl XConfig {
    /// Extracts the configuration from an already-built figment and validates it.
    ///
    /// # Errors
    /// When the provider data does not fit the declared tree or breaks a validation rule.
    pub fn extract_from(figment: &Figment) -> Result<Self, ConfigError> {
        let config: Self = figment.extract()?;
        let violations = validate(&config);
        if violations.is_empty() {
            Ok(config)
        } else {
            Err(ConfigError::Invalid { violations })
        }
    }
}

/// The provider stack production reads: built-in defaults plus prefixed environment overrides.
#[must_use]
pub fn figment() -> Figment {
    Figment::from(Serialized::defaults(XConfig::default()))
        .merge(Env::prefixed(ENV_PREFIX).split("__"))
}

/// Loads the configuration from the process environment.
///
/// # Errors
/// See [`XConfig::extract_from`].
pub fn load() -> Result<XConfig, ConfigError> {
    XConfig::extract_from(&figment())
}

fn validate(config: &XConfig) -> Violations {
    let mut violations = Violations::default();
    if config
        .admin
        .listen_addr
        .parse::<std::net::SocketAddr>()
        .is_err()
    {
        violations.push("admin.listen_addr must be a socket address such as 127.0.0.1:8080");
    }
    if config.database.url.is_empty() {
        violations.push("database.url must not be empty");
    }
    if config.database.max_connections == 0 {
        violations.push("database.max_connections must be at least 1");
    }
    if config.telemetry.log_filter.trim().is_empty() {
        violations.push("telemetry.log_filter must not be empty");
    }
    violations
}
