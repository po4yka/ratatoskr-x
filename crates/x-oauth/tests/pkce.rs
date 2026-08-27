//! PKCE derivation and generation behave like RFC 7636 says they do.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use base64::Engine as _;
use x_oauth::pkce;

/// RFC 7636 Appendix B verifier.
const APPENDIX_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
/// RFC 7636 Appendix B expected S256 challenge.
const APPENDIX_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

/// Returns the still-percent-encoded scope query value from an authorization URL.
fn scope_query_value(url: &str) -> &str {
    url.split('&')
        .find_map(|part| part.strip_prefix("scope="))
        .expect("the authorization URL carries one scope parameter")
}

#[test]
fn challenge_matches_rfc7636_appendix_vector() {
    let challenge = pkce::challenge_from_verifier(APPENDIX_VERIFIER);
    assert_eq!(
        challenge, APPENDIX_CHALLENGE,
        "the S256 challenge must equal BASE64URL(SHA256(verifier))"
    );
}

const BASE64URL_ALPHABET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

#[test]
fn generated_verifier_and_state_meet_length_and_alphabet_rules() {
    let verifier = pkce::generate_verifier();
    assert!(
        (43..=128).contains(&verifier.len()),
        "the verifier length must be within RFC 7636's range: {verifier}"
    );
    assert!(
        verifier.chars().all(|c| BASE64URL_ALPHABET.contains(c)),
        "the verifier must stay on the base64url alphabet: {verifier}"
    );

    let state = pkce::generate_state();
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(state.as_bytes())
        .expect("the state must decode as base64url without padding");
    assert_eq!(
        decoded.len(),
        32,
        "the state carries 32 random bytes of entropy"
    );
}

#[test]
fn authorization_url_carries_minimized_read_consent_without_leaking_verifier() {
    use x_oauth::intent;

    let mut oauth = x_core::config::XConfig::default().oauth;
    oauth.client_id = Some("client-42".to_owned());
    oauth.redirect_uri = Some("https://app.example/callback".to_owned());
    let issued = intent::AuthorizationIntent::issue();

    let url = intent::authorization_url(&oauth, &issued, intent::IntentPurpose::ReadConnection)
        .expect("a configured client builds the URL");

    assert!(
        url.starts_with(oauth.authorize_url.as_str()),
        "the URL targets the configured authorize endpoint: {url}"
    );
    assert!(
        url.contains("response_type=code"),
        "the grant type is the authorization code flow: {url}"
    );
    assert!(
        url.contains("client_id=client-42"),
        "the registered client id is carried: {url}"
    );
    assert!(
        url.contains("redirect_uri=https%3A%2F%2Fapp.example%2Fcallback"),
        "the registered redirect target is carried percent-encoded: {url}"
    );
    assert!(
        url.contains("code_challenge_method=S256"),
        "the challenge method is S256: {url}"
    );
    assert!(
        url.contains(&format!("code_challenge={}", issued.code_challenge)),
        "the derived challenge is carried: {url}"
    );
    assert!(
        url.contains(&format!("state={}", issued.state)),
        "the state value is carried: {url}"
    );
    assert!(
        url.contains("scope=users.read%20tweet.read%20bookmark.read%20offline.access"),
        "the scope list equals exactly the configured read set in order: {url}"
    );
    assert!(
        !url.contains(&issued.code_verifier),
        "the verifier must never appear in the authorization URL: {url}"
    );
}

#[test]
fn bookmark_write_intent_adds_only_bookmark_write_while_default_stays_read_only() {
    use x_oauth::intent::{self, IntentPurpose};

    let mut oauth = x_core::config::XConfig::default().oauth;
    oauth.client_id = Some("client-write-consent".to_owned());
    oauth.redirect_uri = Some("https://app.example/callback".to_owned());
    let issued = intent::AuthorizationIntent::issue();
    let read_url = intent::authorization_url(&oauth, &issued, IntentPurpose::ReadConnection)
        .expect("the default read intent builds");
    let write_url = intent::authorization_url(
        &oauth,
        &issued,
        IntentPurpose::BookmarkWrite {
            account_id: uuid::Uuid::now_v7(),
        },
    )
    .expect("the bookmark-write intent builds");

    let read_scope = scope_query_value(&read_url);
    let write_scope = scope_query_value(&write_url);

    assert_eq!(
        read_scope, "users.read%20tweet.read%20bookmark.read%20offline.access",
        "the default intent stays on the exact minimized read set"
    );
    assert_ne!(
        write_scope, read_scope,
        "the separately consented write intent must not reuse the read-only scope set"
    );
    assert_eq!(
        write_scope, "users.read%20tweet.read%20bookmark.read%20offline.access%20bookmark.write",
        "the write intent adds exactly bookmark.write to the read prerequisites"
    );
}
