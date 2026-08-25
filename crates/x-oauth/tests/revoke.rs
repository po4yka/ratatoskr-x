//! Revocation notifies the provider once, scrubs local material, and stays
//! idempotent without repeat provider contact.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::sync::{Arc, Mutex};

use x_oauth::cipher::{Purpose, TokenCipher};
use x_oauth::clock::Clock;
use x_oauth::payload::CredentialPayload;
use x_oauth::service::ConnectionService;
use x_oauth::token_client::TokenClient;
use x_persistence::credentials;
use x_persistence::test_support::TestDatabase;

/// A deterministic key so envelopes open the same way everywhere in this binary.
fn test_cipher() -> TokenCipher {
    TokenCipher::new(&[51u8; 32]).expect("a valid 32-byte test key")
}

/// A test clock the suite moves by hand.
#[derive(Debug)]
struct MutableClock(Mutex<chrono::DateTime<chrono::Utc>>);

impl MutableClock {
    fn at(instant: &str) -> Arc<Self> {
        Arc::new(Self(Mutex::new(
            chrono::DateTime::parse_from_rfc3339(instant)
                .expect("a fixed instant")
                .with_timezone(&chrono::Utc),
        )))
    }
}

impl Clock for MutableClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        *self.0.lock().expect("an uncontended test clock")
    }
}

/// Captures request bodies and paths so assertions can inspect the wire shape.
#[derive(Debug, Default, Clone)]
struct CaptureMatcher(Arc<CapturedRequests>);

#[derive(Debug, Default)]
struct CapturedRequests {
    bodies: Mutex<Vec<String>>,
    paths: Mutex<Vec<String>>,
}

impl wiremock::Match for CaptureMatcher {
    fn matches(&self, request: &wiremock::Request) -> bool {
        self.0
            .bodies
            .lock()
            .expect("an uncontended capture buffer")
            .push(String::from_utf8_lossy(&request.body).into_owned());
        self.0
            .paths
            .lock()
            .expect("an uncontended capture buffer")
            .push(request.url.path().to_owned());
        true
    }
}

impl CaptureMatcher {
    fn snapshot(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.bodies.lock().expect("an uncontended capture buffer"))
    }
}

fn oauth_config(token_url: String) -> x_core::config::OauthConfig {
    let mut oauth = x_core::config::XConfig::default().oauth;
    oauth.token_url.clone_from(&token_url);
    oauth.revocation_url = token_url;
    oauth.client_id = Some("test-client-id".to_owned());
    oauth.client_secret = Some("test-client-secret".to_owned());
    oauth.redirect_uri = Some("https://app.example/callback".to_owned());
    oauth
}

fn wired_service(test: &TestDatabase, revocation_url: String) -> ConnectionService {
    let client = TokenClient::new(&oauth_config(revocation_url.clone())).expect("an HTTP client");
    ConnectionService::new(
        test.database.clone(),
        test_cipher(),
        client,
        MutableClock::at("2026-08-26T12:00:00Z"),
        oauth_config(revocation_url),
    )
}

async fn seed_account(test: &TestDatabase, provider_user_id: &str) -> uuid::Uuid {
    let account_id = uuid::Uuid::now_v7();
    test.database
        .query_raw(&format!(
            "insert into x_archive.accounts (id, provider_user_id) \
             values ('{account_id}', '{provider_user_id}')"
        ))
        .await
        .expect("the account row seeds");
    account_id
}

/// Seeds one active credential holding the given tokens for `account`.
async fn seed_active_credential(
    test: &TestDatabase,
    account: uuid::Uuid,
    tokens: CredentialPayload,
) -> uuid::Uuid {
    let sealed = test_cipher().seal(account, Purpose::Credential, &tokens.encode());
    let scopes = vec![
        "users.read".to_owned(),
        "tweet.read".to_owned(),
        "bookmark.read".to_owned(),
        "offline.access".to_owned(),
    ];
    credentials::insert_with_status(
        &test.database,
        &credentials::NewCredential {
            account_id: account,
            encrypted_payload: &sealed,
            granted_scopes: &scopes,
            expires_at: None,
        },
        "active",
    )
    .await
    .expect("the credential seeds")
}

#[tokio::test]
async fn revocation_scrubs_local_material() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let server = wiremock::MockServer::start().await;
    let captured = CaptureMatcher::default();
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/2/oauth2/token"))
        .and(captured.clone())
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;

    let service = wired_service(&test, format!("{}/2/oauth2/token", server.uri()));
    let account = seed_account(&test, "revoke-active-user").await;
    seed_active_credential(
        &test,
        account,
        CredentialPayload {
            access_token: "PLACEHOLDER-access-token-rotated".to_owned(),
            refresh_token: "PLACEHOLDER-refresh-token-rotated".to_owned(),
        },
    )
    .await;

    service.revoke(account).await.expect("revocation succeeds");

    let row = credentials::latest_for_account(&test.database, account)
        .await
        .expect("the credential query succeeds")
        .expect("the credential row remains as evidence");
    assert_eq!(row.status, "revoked", "the credential reads revoked");
    assert!(
        row.encrypted_payload.is_empty(),
        "the payload holds no recoverable token bytes"
    );
    let state = credentials::account_state(&test.database, account)
        .await
        .expect("the account query succeeds");
    assert_eq!(
        state.as_deref(),
        Some("revoked"),
        "the account reads revoked"
    );

    let bodies = captured.snapshot();
    assert_eq!(bodies.len(), 1, "exactly one provider call happened");
    assert!(
        bodies[0].contains("token=PLACEHOLDER-access-token-rotated"),
        "the current access token is presented to the revocation endpoint: {}",
        bodies[0]
    );

    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn revocation_is_idempotent_without_repeat_provider_contact() {
    // Case A: an already-revoked connection revokes again without provider contact.
    let test = TestDatabase::create().await.expect("a disposable database");
    let server = wiremock::MockServer::start().await;
    let captured = CaptureMatcher::default();
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(captured.clone())
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;

    let service = wired_service(&test, format!("{}/2/oauth2/token", server.uri()));
    let account = seed_account(&test, "revoke-again-user").await;
    credentials::insert_with_status(
        &test.database,
        &credentials::NewCredential {
            account_id: account,
            encrypted_payload: &[],
            granted_scopes: &[],
            expires_at: None,
        },
        "revoked",
    )
    .await
    .expect("the already-revoked credential seeds");

    service
        .revoke(account)
        .await
        .expect("revoking an already-revoked connection succeeds idempotently");
    assert!(
        captured.snapshot().is_empty(),
        "no further provider request may happen for an already-revoked connection"
    );
    test.cleanup().await.expect("cleanup drops the database");

    // Case B: an account with no credential at all also succeeds silently.
    let test = TestDatabase::create().await.expect("a disposable database");
    let server = wiremock::MockServer::start().await;
    let captured = CaptureMatcher::default();
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(captured.clone())
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string("{}"))
        .mount(&server)
        .await;

    let service = wired_service(&test, format!("{}/2/oauth2/token", server.uri()));
    let account = seed_account(&test, "revoke-absent-user").await;

    service
        .revoke(account)
        .await
        .expect("revoking an absent connection succeeds");
    assert!(
        captured.snapshot().is_empty(),
        "an absent credential never contacts the provider"
    );
    let state = credentials::account_state(&test.database, account)
        .await
        .expect("the account query succeeds")
        .expect("the seeded account exists");
    assert_eq!(
        state, "connected",
        "a silent revocation leaves the untouched account consistent"
    );

    test.cleanup().await.expect("cleanup drops the database");
}
