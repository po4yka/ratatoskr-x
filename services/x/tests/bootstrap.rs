//! Subsystem failures map to documented, distinct exit codes.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use ratatoskr_x::bootstrap::BootstrapError;
use x_core::error::{ConfigError, Violations};

#[test]
fn subsystem_errors_map_to_distinct_exit_codes() {
    let config = BootstrapError::Config(ConfigError::Invalid {
        violations: Violations::default(),
    });
    let telemetry = BootstrapError::Telemetry(x_telemetry::error::TelemetryError::AlreadyInstalled);
    let persistence = BootstrapError::Persistence(x_persistence::error::PersistenceError::Query(
        sqlx::Error::ColumnNotFound("probe".to_owned()),
    ));
    let listener =
        BootstrapError::Listener(std::io::Error::new(std::io::ErrorKind::AddrInUse, "taken"));

    assert_eq!(
        config.exit_code(),
        78,
        "EX_CONFIG for configuration rejection"
    );
    for other in [&telemetry, &persistence, &listener] {
        let code = other.exit_code();
        assert_ne!(code, 0, "every failure exits nonzero");
        assert_ne!(code, 78, "only configuration exits 78: {other:?}");
    }

    for (error, word) in [
        (&config, "configuration"),
        (&telemetry, "telemetry"),
        (&persistence, "database"),
        (&listener, "listener"),
    ] {
        assert!(
            error.report().contains(word),
            "{} names its subsystem",
            word
        );
    }
}
