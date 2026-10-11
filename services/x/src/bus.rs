//! The service's one NATS connection (XR-021 CONTRACTS.md section R3-01).

use x_core::config::BusConfig;

use crate::bootstrap::BootstrapError;

/// The inbox prefix of the X identity: the one subject tree its stanza in the deployed ACL lets it
/// subscribe to (`subscribe.allow = ["_INBOX_x.>"]`). Every `JetStream` reply reaches the service on
/// a subject under it, so a connection without the prefix never receives one.
pub const INBOX_PREFIX: &str = "_INBOX_x";

/// Reads the `NKey` seed named by the configuration and opens the broker connection.
///
/// # Errors
/// Returns [`BootstrapError::NatsSeed`] when the seed file cannot be read and
/// [`BootstrapError::Nats`] when the broker refuses the connection.
pub async fn connect_bus(config: &BusConfig) -> Result<async_nats::Client, BootstrapError> {
    let nkey_seed =
        std::fs::read_to_string(&config.nkey_seed_path).map_err(BootstrapError::NatsSeed)?;
    async_nats::ConnectOptions::with_nkey(nkey_seed.trim().to_owned())
        .custom_inbox_prefix(INBOX_PREFIX)
        .connect(&config.url)
        .await
        .map_err(|error| BootstrapError::Nats(error.to_string()))
}
