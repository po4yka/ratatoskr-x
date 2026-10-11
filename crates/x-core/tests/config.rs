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
    assert_eq!(config.admin.listen_addr, "127.0.0.1:9087");
    assert_eq!(config.database.max_connections, 5);
    assert_eq!(config.telemetry.log_format, LogFormat::Json);
    assert_eq!(config.telemetry.log_filter, "info");
}

/// Edge owns the public `8080` listener; the X operator listener is a host-only allocation of its own
/// (XR-021 CONTRACTS.md S05), so the two defaults must never collide.
#[test]
fn operator_listener_default_differs_from_the_edge_public_port() {
    let config = XConfig::extract_from(&figment_with(&[])).expect("defaults must load");
    let address: std::net::SocketAddr = config
        .admin
        .listen_addr
        .parse()
        .expect("the default listener is a socket address");
    assert_ne!(address.port(), 8080, "8080 is the Edge public port");
    assert!(
        address.ip().is_loopback(),
        "operator listeners are loopback"
    );
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

#[test]
fn public_capture_section_loads_with_the_documented_defaults() {
    let config = XConfig::extract_from(&figment_with(&[])).expect("defaults must load");
    assert_eq!(config.public_capture.api_base_url, "https://api.x.com");
    assert!(
        config.public_capture.bearer_token_path.is_none(),
        "no credential path may exist by default"
    );
    assert_eq!(config.public_capture.max_attempts, 5);
    assert_eq!(config.public_capture.batch_size, 8);
    assert_eq!(config.public_capture.poll_interval_seconds, 2);
    assert_eq!(config.bus.events_stream_name, "ratatoskr_events");
}

#[test]
fn public_capture_requires_an_https_endpoint_and_an_absolute_token_path() {
    let figment = figment_with(&[
        (
            "public_capture.api_base_url",
            Value::from("http://api.x.com"),
        ),
        (
            "public_capture.bearer_token_path",
            Value::from("relative/token"),
        ),
    ]);
    let error = XConfig::extract_from(&figment)
        .expect_err("a plain-http endpoint and a relative token path must be refused");
    let ConfigError::Invalid { violations } = error else {
        panic!("a semantic rejection must be Invalid");
    };
    assert_eq!(violations.len(), 2, "both unsafe settings are reported");
    assert!(
        violations
            .iter()
            .any(|violation| violation.message.contains("public_capture.api_base_url")),
        "{violations}"
    );
    assert!(
        violations.iter().any(|violation| violation
            .message
            .contains("public_capture.bearer_token_path")),
        "{violations}"
    );
}

#[test]
fn public_capture_counters_must_be_positive() {
    let figment = figment_with(&[
        ("public_capture.max_attempts", Value::from(0_u32)),
        ("public_capture.batch_size", Value::from(0_u32)),
        ("public_capture.poll_interval_seconds", Value::from(0_u64)),
    ]);
    let error = XConfig::extract_from(&figment).expect_err("zero-valued counters must be refused");
    let ConfigError::Invalid { violations } = error else {
        panic!("a semantic rejection must be Invalid");
    };
    assert_eq!(violations.len(), 3, "{violations}");
}

#[test]
fn public_capture_unknown_keys_are_still_refused() {
    let figment = figment_with(&[("public_capture.bogus", Value::from("1"))]);
    let error = XConfig::extract_from(&figment).expect_err("an undeclared key must be refused");
    assert!(error.report().contains("bogus"), "{}", error.report());
}

#[test]
fn a_configured_bus_requires_the_bearer_token_path() {
    let without = XConfig::extract_from(&figment_with(&[])).expect("defaults must load");
    let error = without
        .require_public_capture_token_path()
        .expect_err("a bus without the public-capture credential cannot complete work");
    assert_eq!(error.exit_code(), 78, "EX_CONFIG");
    assert!(
        error.report().contains("public_capture.bearer_token_path"),
        "{}",
        error.report()
    );

    let with = XConfig::extract_from(&figment_with(&[(
        "public_capture.bearer_token_path",
        Value::from("/run/secrets/ratatoskr-x-bearer"),
    )]))
    .expect("an absolute token path loads");
    assert_eq!(
        with.require_public_capture_token_path()
            .expect("the path is present"),
        "/run/secrets/ratatoskr-x-bearer"
    );
}

/// A throwaway self-signed certificate; only its PEM framing matters to the loader.
const CERTIFICATE_PEM: &str = "-----BEGIN CERTIFICATE-----
MIIBejCCASCgAwIBAgIUd3xNCkFrw39bCg8g3stNoCgC7jAwCgYIKoZIzj0EAwIw
IjEgMB4GA1UEAwwXUmF0YXRvc2tyIFggY29uZmlnIHRlc3QwIBcNMjYxMDExMTEy
MzMzWhgPMjEyNjA5MTcxMTIzMzNaMCIxIDAeBgNVBAMMF1JhdGF0b3NrciBYIGNv
bmZpZyB0ZXN0MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEoRwL0r3xE0fQgHwO
G0HIew5Oli4lfowu5TyChX36tyZ4XlTmyDFbkoYZgKDQuQH/6lMRGcmSlU4EV6fJ
eMwkpKMyMDAwHQYDVR0OBBYEFEuleYd2/nD1rT0BsvbIEqWmewyXMA8GA1UdEwEB
/wQFMAMBAf8wCgYIKoZIzj0EAwIDSAAwRQIgHUe5gv5mEk8POtOWi+O5Y/LrHSb6
Jmtr1xbyREBMF00CIQDALZSV25I6Laka8zc2cBzQcfPFvCLzkUZpz75Syv5x1g==
-----END CERTIFICATE-----
";

fn extra_ca_result(path: &str) -> Result<XConfig, ConfigError> {
    XConfig::extract_from(&figment_with(&[(
        "public_capture.extra_ca_file",
        Value::from(path),
    )]))
}

#[test]
fn the_extra_ca_file_must_be_absolute_readable_and_hold_a_certificate() {
    let directory =
        std::env::temp_dir().join(format!("ratatoskr-x-extra-ca-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("a scratch directory");
    let good = directory.join("good.pem");
    let empty = directory.join("empty.pem");
    let key_only = directory.join("key.pem");
    std::fs::write(&good, CERTIFICATE_PEM).expect("the certificate file is written");
    std::fs::write(&empty, "nothing here").expect("the text file is written");
    std::fs::write(
        &key_only,
        "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n",
    )
    .expect("the key file is written");
    let missing = directory.join("missing.pem");

    for (label, path) in [
        ("a relative path", "relative/ca.pem".to_owned()),
        ("a missing file", missing.display().to_string()),
        ("a file with no PEM", empty.display().to_string()),
        ("a file with only a key", key_only.display().to_string()),
    ] {
        let error = extra_ca_result(&path).expect_err(label);
        assert!(
            error.report().contains("public_capture.extra_ca_file"),
            "{label} is refused with the key named, got: {}",
            error.report()
        );
    }

    let accepted = extra_ca_result(&good.display().to_string()).expect("a certificate file loads");
    assert_eq!(
        accepted.public_capture.extra_ca_file.as_deref(),
        Some(good.display().to_string().as_str())
    );
    let absent = XConfig::extract_from(&figment_with(&[])).expect("defaults must load");
    assert_eq!(
        absent.public_capture.extra_ca_file, None,
        "absent changes nothing"
    );

    let _ = std::fs::remove_dir_all(&directory);
}
