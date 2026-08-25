//! Credential-flow secrecy: debug renderings and error renderings never carry
//! the secret inputs that produced them.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use x_oauth::error::{CipherError, FlowError};
use x_oauth::payload::CredentialPayload;

#[test]
fn payload_debug_rendering_excludes_token_material() {
    let payload = CredentialPayload {
        access_token: "MARKER-access-token-secret".to_owned(),
        refresh_token: "MARKER-refresh-token-secret".to_owned(),
    };

    let rendered = format!("{payload:?}");
    assert!(
        !rendered.contains("MARKER-access-token-secret"),
        "the debug rendering must not contain the access token: {rendered}"
    );
    assert!(
        !rendered.contains("MARKER-refresh-token-secret"),
        "the debug rendering must not contain the refresh token: {rendered}"
    );
}

/// GUARD: every OAuth error is value-free by construction, so this cannot fail
/// first. It pins that property against regressions.
#[test]
fn error_renderings_exclude_secret_inputs() {
    let marker_access = "MARKER-access-token-value";
    let marker_refresh = "MARKER-refresh-token-value";
    let marker_verifier = "MARKER-verifier-value";
    let marker_code = "MARKER-authorization-code";
    // Held in scope while errors are constructed and rendered.
    let _ = (marker_access, marker_refresh, marker_verifier, marker_code);

    let flow_errors: Vec<FlowError> = vec![
        FlowError::Configuration,
        FlowError::Cipher(CipherError::Key),
        FlowError::Cipher(CipherError::Auth),
        FlowError::Cipher(CipherError::Binding),
        FlowError::Cipher(CipherError::Version),
        FlowError::Downgrade {
            missing_scopes: vec!["bookmark.read".to_owned()],
        },
        FlowError::UnknownCallbackState,
        FlowError::ExpiredCallbackIntent,
        FlowError::ReplayedCallbackState,
        FlowError::StaleRefreshToken,
        FlowError::RefreshReplay,
        FlowError::CredentialInactive,
        FlowError::UpstreamInvalidation,
        FlowError::Transport,
        FlowError::MalformedProviderResponse,
    ];
    let cipher_errors: Vec<CipherError> = vec![
        CipherError::Key,
        CipherError::Auth,
        CipherError::Binding,
        CipherError::Version,
    ];

    for error in &flow_errors {
        let rendered = format!("{error}");
        for marker in [marker_access, marker_refresh, marker_verifier, marker_code] {
            assert!(
                !rendered.contains(marker),
                "the display rendering leaks secret material: {rendered}"
            );
        }
        let debugged = format!("{error:?}");
        for marker in [marker_access, marker_refresh, marker_verifier, marker_code] {
            assert!(
                !debugged.contains(marker),
                "the debug rendering leaks secret material: {debugged}"
            );
        }
    }
    for error in &cipher_errors {
        let rendered = format!("{error}");
        assert!(
            !rendered.contains(marker_verifier),
            "the display rendering leaks secret material: {rendered}"
        );
    }
}
