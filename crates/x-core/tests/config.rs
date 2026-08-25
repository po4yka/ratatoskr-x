//! Configuration loading behaves like the `service-bootstrap` spec says it does.

use figment::Figment;
use figment::providers::Serialized;
use figment::value::Value;
use x_core::config::{LogFormat, XConfig};
use x_core::error::ConfigError;

/// Builds the production-shaped provider stack from explicit pairs, so tests never mutate
/// process environment (which is `unsafe` in edition 2024 and racy across test threads).
/// Each pair lands in the same profile the environment writes to, with real types.
fn figment_with(pairs: &[(&str, Value)]) -> Figment {
    let mut figment = Figment::from(Serialized::defaults(XConfig::default()));
    for (key, value) in pairs {
        figment = figment.merge(Serialized::default(*key, value.clone()));
    }
    figment
}

#[test]
fn unknown_environment_key_is_refused() {
    let figment = figment_with(&[("unknown_key", Value::from("1"))]);
    let error = XConfig::extract_from(&figment).expect_err("an undeclared key must be refused");
    assert!(
        error.report().contains("unknown"),
        "the rejection must name the offending key, got: {}",
        error.report()
    );
}

#[test]
fn absent_variables_yield_documented_defaults() {
    let config = XConfig::extract_from(&figment_with(&[])).expect("defaults must load");
    assert_eq!(config.admin.listen_addr, "127.0.0.1:8080");
    assert_eq!(config.database.max_connections, 5);
    assert_eq!(config.telemetry.log_format, LogFormat::Json);
    assert_eq!(config.telemetry.log_filter, "info");
}

#[test]
fn invalid_values_report_all_violations_together() {
    let figment = figment_with(&[
        ("admin.listen_addr", Value::from("not-an-address")),
        ("database.max_connections", Value::from(0_u32)),
    ]);
    let Err(ConfigError::Invalid { violations }) = XConfig::extract_from(&figment) else {
        panic!("semantic violations must surface as one collected rejection");
    };
    assert_eq!(
        violations.len(),
        2,
        "every violation must be reported together"
    );
}

#[test]
fn config_rejection_maps_to_exit_code_78() {
    let figment = figment_with(&[("unknown_key", Value::from("1"))]);
    let error = XConfig::extract_from(&figment).expect_err("an undeclared key must be refused");
    assert_eq!(error.exit_code(), 78, "EX_CONFIG is the documented status");
}
