//! Subsystem failures map to documented, distinct exit codes.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use figment::Figment;
use figment::providers::Serialized;
use figment::value::Value;
use ratatoskr_x::bootstrap::BootstrapError;
use x_core::config::{ENV_PREFIX, XConfig};
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
        assert!(error.report().contains(word), "{word} names its subsystem");
    }
}

/// The `KEY=value` lines of a systemd `EnvironmentFile`, comments and blanks dropped.
fn environment_pairs(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (key, value) = line.split_once('=').expect("every line is KEY=value");
            (key.trim().to_owned(), value.trim().to_owned())
        })
        .collect()
}

/// Builds the provider stack the process environment would build, from explicit pairs: the same
/// prefix strip, the same `__` nesting, the same typed scalars.
fn figment_from(pairs: &[(String, String)]) -> Figment {
    let mut figment = Figment::from(Serialized::defaults(XConfig::default()));
    for (key, value) in pairs {
        let path = key
            .strip_prefix(ENV_PREFIX)
            .expect("every key carries the prefix")
            .to_ascii_lowercase()
            .replace("__", ".");
        let typed = value
            .parse::<u64>()
            .map_or_else(|_| Value::from(value.clone()), Value::from);
        figment = figment.merge(Serialized::default(&path, typed));
    }
    figment
}

#[test]
fn the_shipped_example_loads() {
    let example = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../deploy/systemd/x.conf.example"
    ))
    .expect("deploy/systemd/x.conf.example is shipped");
    let suffix = uuid::Uuid::now_v7().simple();
    let seed = std::env::temp_dir().join(format!("ratatoskr-x-example-{suffix}.nkey"));
    let token = std::env::temp_dir().join(format!("ratatoskr-x-example-{suffix}.token"));
    std::fs::write(&seed, "seed").expect("the seed stand-in is written");
    std::fs::write(&token, "token").expect("the token stand-in is written");

    let shipped = environment_pairs(&example);
    let value_of = |key: &str| {
        shipped.iter().find(|(name, _)| name == key).map_or_else(
            || panic!("the example sets {key}"),
            |(_, value)| value.clone(),
        )
    };
    assert_eq!(
        value_of("RATATOSKR__BUS__NKEY_SEED_PATH"),
        "/etc/ratatoskr/x.nkey",
        "the seed lives where CONTRACTS.md R2-10 puts every seed"
    );
    // The loader only checks that the paths are absolute; the stand-ins prove the example still
    // names keys the loader accepts when its secrets are real files.
    let substituted: Vec<(String, String)> = shipped
        .iter()
        .map(|(key, value)| match key.as_str() {
            "RATATOSKR__BUS__NKEY_SEED_PATH" => (key.clone(), seed.display().to_string()),
            "RATATOSKR__PUBLIC_CAPTURE__BEARER_TOKEN_PATH" => {
                (key.clone(), token.display().to_string())
            }
            _ => (key.clone(), value.clone()),
        })
        .collect();

    let config = XConfig::extract_from(&figment_from(&substituted))
        .expect("the shipped example is a valid configuration");

    assert_eq!(config.admin.listen_addr, "127.0.0.1:9087");
    assert_eq!(config.bus.url, "nats://127.0.0.1:4222");
    assert_eq!(config.bus.nkey_seed_path, seed.display().to_string());
    assert_eq!(
        config
            .require_public_capture_token_path()
            .expect("the bus is configured, so the token path is required"),
        token.display().to_string()
    );
    assert_eq!(config.public_capture.api_base_url, "https://api.x.com");
    assert_eq!(
        (
            config.public_capture.max_attempts,
            config.public_capture.batch_size,
            config.public_capture.poll_interval_seconds
        ),
        (5, 8, 2)
    );
    let _ = std::fs::remove_file(&seed);
    let _ = std::fs::remove_file(&token);
}

/// The shared database every Ratatoskr service connects to as its own role (XR-021 CONTRACTS.md
/// R3-11).
const SHARED_DATABASE_URL: &str = "postgres://ratatoskr_x:CHANGE-ME@127.0.0.1:5432/ratatoskr";

#[test]
fn the_example_names_the_shared_database_and_the_role_file_exists() {
    let example = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../deploy/systemd/x.conf.example"
    ))
    .expect("deploy/systemd/x.conf.example is shipped");
    let suffix = uuid::Uuid::now_v7().simple();
    let seed = std::env::temp_dir().join(format!("ratatoskr-x-shared-{suffix}.nkey"));
    let token = std::env::temp_dir().join(format!("ratatoskr-x-shared-{suffix}.token"));
    std::fs::write(&seed, "seed").expect("the seed stand-in is written");
    std::fs::write(&token, "token").expect("the token stand-in is written");
    let substituted: Vec<(String, String)> = environment_pairs(&example)
        .into_iter()
        .map(|(key, value)| match key.as_str() {
            "RATATOSKR__BUS__NKEY_SEED_PATH" => (key, seed.display().to_string()),
            "RATATOSKR__PUBLIC_CAPTURE__BEARER_TOKEN_PATH" => (key, token.display().to_string()),
            _ => (key, value),
        })
        .collect();

    let config = XConfig::extract_from(&figment_from(&substituted))
        .expect("the shipped example is a valid configuration");
    let _ = std::fs::remove_file(&seed);
    let _ = std::fs::remove_file(&token);

    assert_eq!(
        config.database.url, SHARED_DATABASE_URL,
        "the example connects as its own role to the one shared database on 5432"
    );

    let role = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../deploy/postgres/role.sql"
    ))
    .expect("deploy/postgres/role.sql creates the role the example names");
    for required in [
        "create role ratatoskr_x login;",
        "nosuperuser nocreatedb nocreaterole noreplication nobypassrls",
        "grant connect, create on database ratatoskr to ratatoskr_x;",
        "\\connect ratatoskr",
        "revoke all on schema public from ratatoskr_x;",
    ] {
        assert!(role.contains(required), "role.sql lacks: {required}");
    }
    assert!(
        !role.to_ascii_lowercase().contains("password '"),
        "no credential belongs in the role file"
    );
}
