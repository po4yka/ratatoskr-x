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
    /// Credential-protection settings.
    pub security: SecurityConfig,
    /// Provider OAuth client and consent settings.
    pub oauth: OauthConfig,
    /// Provider API request budget settings.
    pub budgets: BudgetsConfig,
    /// `JetStream` command-consumer configuration.
    pub bus: BusConfig,
}

/// Credential-protection settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecurityConfig {
    /// Base64url-encoded 32-byte token-encryption key; never logged, never persisted.
    pub token_encryption_key: Option<SecretKey>,
}

/// The base64url-encoded token-encryption key material.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretKey(pub String);

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretKey([redacted])")
    }
}

impl From<&str> for SecretKey {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl SecretKey {
    /// Decodes the configured key material, accepting padded or unpadded base64url,
    /// and yields exactly the 32 raw bytes the cipher needs; anything else is [`None`].
    #[must_use]
    pub fn decoded_key(&self) -> Option<[u8; 32]> {
        use base64::Engine as _;
        let raw = self.0.trim();
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(raw)
            .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(raw))
            .ok()?;
        decoded.try_into().ok()
    }
}

/// Provider OAuth client and consent settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OauthConfig {
    /// Provider authorization endpoint.
    pub authorize_url: String,
    /// Provider token endpoint, also used for revocation posting.
    pub token_url: String,
    /// Provider revocation endpoint (RFC 7009).
    pub revocation_url: String,
    /// Registered public/confidential client identifier; absent until provisioned.
    pub client_id: Option<String>,
    /// Confidential-client secret when the registration uses one; never logged.
    pub client_secret: Option<String>,
    /// Redirect URI registered with the provider; absent until provisioned.
    pub redirect_uri: Option<String>,
    /// The minimized read-consent scope set requested on connect.
    pub read_scopes: Vec<String>,
    /// How long an authorization intent stays usable, in seconds.
    pub intent_ttl_seconds: u64,
}

/// Provider API request budget settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetsConfig {
    /// Request cost allowed per account per window before the gate refuses more.
    pub request_cap_per_window: u32,
    /// Length of one fixed budget window, in seconds.
    pub window_seconds: u64,
}

/// The private broker connection used only for X-owned commands.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusConfig {
    /// Private NATS or NATS-over-TLS endpoint; credentials never appear in this URL.
    pub url: String,
    /// Absolute path to the private `NKey` seed file used for broker authentication.
    pub nkey_seed_path: String,
    /// Platform-owned `JetStream` stream containing command subjects.
    pub stream_name: String,
    /// Stable durable name for the X browser-capture pull consumer.
    pub consumer_name: String,
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
            security: SecurityConfig {
                token_encryption_key: None,
            },
            oauth: OauthConfig {
                authorize_url: "https://x.com/i/oauth2/authorize".to_owned(),
                token_url: "https://api.x.com/2/oauth2/token".to_owned(),
                revocation_url: "https://api.x.com/2/oauth2/token".to_owned(),
                client_id: None,
                client_secret: None,
                redirect_uri: None,
                read_scopes: vec![
                    "users.read".to_owned(),
                    "tweet.read".to_owned(),
                    "bookmark.read".to_owned(),
                    "offline.access".to_owned(),
                ],
                intent_ttl_seconds: 600,
            },
            budgets: BudgetsConfig {
                request_cap_per_window: 1000,
                window_seconds: 900,
            },
            bus: BusConfig {
                url: "nats://127.0.0.1:4222".to_owned(),
                nkey_seed_path: "/run/secrets/ratatoskr-x-nats.nkey".to_owned(),
                stream_name: "ratatoskr_commands".to_owned(),
                consumer_name: "ratatoskr_x_browser_capture".to_owned(),
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
    if config
        .security
        .token_encryption_key
        .as_ref()
        .is_some_and(|key| key.decoded_key().is_none())
    {
        violations.push("security.token_encryption_key must be 32 raw bytes base64url-encoded");
    }
    for (name, url) in [
        ("authorize_url", &config.oauth.authorize_url),
        ("token_url", &config.oauth.token_url),
        ("revocation_url", &config.oauth.revocation_url),
    ] {
        if url.trim().is_empty() {
            violations.push(format!("oauth.{name} must not be empty"));
        }
    }
    if config
        .oauth
        .client_id
        .as_ref()
        .is_some_and(String::is_empty)
    {
        violations.push("oauth.client_id must not be empty when set");
    }
    if config
        .oauth
        .redirect_uri
        .as_ref()
        .is_some_and(String::is_empty)
    {
        violations.push("oauth.redirect_uri must not be empty when set");
    }
    if config.oauth.intent_ttl_seconds == 0 {
        violations.push("oauth.intent_ttl_seconds must be at least 1");
    }
    if config.oauth.read_scopes.is_empty() {
        violations.push("oauth.read_scopes must list at least one scope");
    }
    if config.budgets.request_cap_per_window == 0 {
        violations.push("budgets.request_cap_per_window must be at least 1");
    }
    if config.budgets.window_seconds == 0 {
        violations.push("budgets.window_seconds must be at least 1");
    }
    if !(config.bus.url.starts_with("nats://") || config.bus.url.starts_with("tls://")) {
        violations.push("bus.url must start with nats:// or tls://");
    }
    if !config.bus.nkey_seed_path.starts_with('/') {
        violations.push("bus.nkey_seed_path must be an absolute path");
    }
    if config.bus.stream_name.trim().is_empty() {
        violations.push("bus.stream_name must not be empty");
    }
    if config.bus.consumer_name.trim().is_empty() {
        violations.push("bus.consumer_name must not be empty");
    }
    violations
}
