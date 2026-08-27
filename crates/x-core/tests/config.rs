//! Configuration loading behaves like the `service-bootstrap` spec says it does.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use figment::Figment;
use figment::providers::Serialized;
use figment::value::Value;
use x_core::config::{LogFormat, SecretKey, XConfig};
use x_core::error::ConfigError;

/// Builds the production-shaped provider stack from explicit pairs, so tests never mutate
/// process environment (which is `unsafe` in edition 2024 and racy across test threads).
/// Each pair lands in the same profile the environment writes to, with real types.
fn figment_with(pairs: &[(&str, Value)]) -> Figment {
    let mut figment = Figment::from(Serialized::defaults(XConfig::default()));
    for (key, value) in pairs {
        figment = figment.merge(Serialized::default(key, value.clone()));
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
    let error = XConfig::extract_from(&figment)
        .expect_err("semantic violations must surface as one collected rejection");
    let ConfigError::Invalid { violations } = error else {
        return;
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

#[test]
fn oauth_budget_and_security_sections_load_with_documented_defaults() {
    let config = XConfig::extract_from(&figment_with(&[])).expect("defaults must load");
    assert_eq!(
        config.oauth.read_scopes,
        vec![
            "users.read".to_owned(),
            "tweet.read".to_owned(),
            "bookmark.read".to_owned(),
            "offline.access".to_owned()
        ],
        "the read scope set is exactly the minimized read consent"
    );
    assert!(
        !config
            .oauth
            .read_scopes
            .iter()
            .any(|scope| scope.contains("write")),
        "a read connection must never request a write scope"
    );
    assert_eq!(config.oauth.intent_ttl_seconds, 600);
    assert_eq!(config.budgets.request_cap_per_window, 1000);
    assert_eq!(config.budgets.window_seconds, 900);
    assert!(
        config.security.token_encryption_key.is_none(),
        "no key may exist by default"
    );
}

#[test]
fn browser_capture_bus_requires_a_private_endpoint_and_absolute_nkey_path() {
    let valid =
        XConfig::extract_from(&figment_with(&[])).expect("the documented bus defaults load");
    assert_eq!(valid.bus.stream_name, "ratatoskr_commands");
    assert_eq!(valid.bus.consumer_name, "ratatoskr_x_browser_capture");
    assert!(valid.bus.nkey_seed_path.starts_with('/'));

    let invalid = figment_with(&[
        ("bus.url", Value::from("https://broker.example")),
        ("bus.nkey_seed_path", Value::from("relative.nkey")),
    ]);
    let error = XConfig::extract_from(&invalid)
        .expect_err("a non-NATS endpoint and relative seed path must be refused");
    let ConfigError::Invalid { violations } = error else {
        panic!("a semantic broker rejection must be Invalid");
    };
    assert_eq!(
        violations.len(),
        2,
        "both unsafe broker settings are reported"
    );
}

#[test]
fn malformed_encryption_key_reports_violation() {
    let malformed = figment_with(&[(
        "security.token_encryption_key",
        Value::from("definitely-not-base64url!!"),
    )]);
    let error = XConfig::extract_from(&malformed)
        .expect_err("a malformed key must be refused with collected violations");
    let ConfigError::Invalid { violations } = error else {
        panic!("a semantic key rejection must be Invalid, not Source");
    };
    assert!(
        violations
            .iter()
            .any(|violation| violation.message.contains("token_encryption_key")),
        "the violation must name the setting, got: {violations}"
    );

    // 43 base64url characters encode exactly 32 bytes.
    let raw_32_bytes_base64url = "A".repeat(43);
    let well_formed = figment_with(&[(
        "security.token_encryption_key",
        Value::from(raw_32_bytes_base64url),
    )]);
    assert!(
        XConfig::extract_from(&well_formed).is_ok(),
        "a well-formed 32-byte base64url key must load cleanly"
    );
}

#[test]
fn secret_key_debug_rendering_redacts_material() {
    let marker = "super-secret-marker-value";
    let key = SecretKey::from(marker);
    let rendered = format!("{key:?}");
    assert!(
        rendered.contains("redacted"),
        "the debug rendering must mark the secret as redacted, got: {rendered}"
    );
    assert!(
        !rendered.contains(marker),
        "the debug rendering must never contain the raw material"
    );
}
