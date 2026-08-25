//! Code exchange against a wiremock provider double serving recorded fixtures.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::sync::{Arc, Mutex};

use x_oauth::callback::AcceptedCallback;
use x_oauth::cipher::{Purpose, TokenCipher};
use x_oauth::clock::Clock;
use x_oauth::error::FlowError;
use x_oauth::payload::CredentialPayload;
use x_oauth::service::ConnectionService;
use x_oauth::token_client::TokenClient;
use x_persistence::test_support::TestDatabase;

/// The fixture directory at the workspace root.
fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/oauth")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("fixture {name} must be readable: {error}"))
}

/// A deterministic key so envelopes open the same way everywhere in this binary.
fn test_cipher() -> TokenCipher {
    let key = [31u8; 32];
    TokenCipher::new(&key).expect("a valid 32-byte test key")
}

/// A test clock the suite moves by hand.
#[derive(Debug)]
struct MutableClock {
    current: Mutex<chrono::DateTime<chrono::Utc>>,
}

impl MutableClock {
    fn at(instant: &str) -> Self {
        Self {
            current: Mutex::new(
                chrono::DateTime::parse_from_rfc3339(instant)
                    .expect("a fixed instant")
                    .with_timezone(&chrono::Utc),
            ),
        }
    }
}

impl Clock for MutableClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        *self.current.lock().expect("an uncontended test clock")
    }
}

/// Captures every request the mock receives so assertions can inspect wire shape.
#[derive(Debug, Default)]
struct CapturedRequests {
    bodies: Mutex<Vec<String>>,
    authorizations: Mutex<Vec<Option<String>>>,
}

impl wiremock::Match for CapturedRequests {
    fn matches(&self, request: &wiremock::Request) -> bool {
        self.bodies
            .lock()
            .expect("an uncontended capture buffer")
            .push(String::from_utf8_lossy(&request.body).into_owned());
        self.authorizations
            .lock()
            .expect("an uncontended capture buffer")
            .push(
                request
                    .headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned),
            );
        true
    }
}

/// A sharable matcher handle the test keeps for later inspection.
#[derive(Debug, Clone, Default)]
struct CaptureMatcher(Arc<CapturedRequests>);

impl wiremock::Match for CaptureMatcher {
    fn matches(&self, request: &wiremock::Request) -> bool {
        self.0.matches(request)
    }
}

/// Configuration pointed at `token_url`, with a confidential client registered.
fn oauth_config(token_url: String) -> x_core::config::OauthConfig {
    let mut oauth = x_core::config::XConfig::default().oauth;
    oauth.token_url.clone_from(&token_url);
    oauth.revocation_url = token_url;
    oauth.client_id = Some("test-client-id".to_owned());
    oauth.client_secret = Some("test-client-secret".to_owned());
    oauth.redirect_uri = Some("https://app.example/callback".to_owned());
    oauth
}

fn wired_service(test: &TestDatabase, token_url: String) -> ConnectionService {
    let cipher = test_cipher();
    let client = TokenClient::new(&oauth_config(token_url.clone())).expect("an HTTP client");
    ConnectionService::new(
        test.database.clone(),
        cipher,
        client,
        Arc::new(MutableClock::at("2026-08-26T12:00:00Z")),
        oauth_config(token_url),
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

fn accepted_for(user: uuid::Uuid) -> AcceptedCallback {
    AcceptedCallback {
        internal_user_id: user,
        code_verifier: "stored-pkce-verifier-from-the-intent".to_owned(),
    }
}

#[tokio::test]
async fn successful_exchange_activates_account_with_encrypted_tokens() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let server = wiremock::MockServer::start().await;
    let captured = CaptureMatcher::default();
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/2/oauth2/token"))
        .and(captured.clone())
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_string(fixture("token_success.json")),
        )
        .mount(&server)
        .await;

    let service = wired_service(&test, format!("{}/2/oauth2/token", server.uri()));
    let account = seed_account(&test, "exchange-success-user").await;
    let user = uuid::Uuid::from_u128(0x6060);

    service
        .exchange_code(account, &accepted_for(user), "the-authorization-code")
        .await
        .expect("the exchange succeeds and activates the account");

    let credential = x_persistence::credentials::latest_for_account(&test.database, account)
        .await
        .expect("the credential query succeeds")
        .expect("a credential row is stored for the account");
    assert_eq!(credential.status, "active", "the credential activates");
    assert_eq!(
        credential.granted_scopes,
        vec![
            "users.read".to_owned(),
            "tweet.read".to_owned(),
            "bookmark.read".to_owned(),
            "offline.access".to_owned(),
        ],
        "granted scopes are recorded verbatim in provider order"
    );

    let cipher = test_cipher();
    let payload_bytes = cipher
        .open(account, Purpose::Credential, &credential.encrypted_payload)
        .expect("the stored envelope opens under the account binding");
    let payload = CredentialPayload::decode(&payload_bytes).expect("a decodable payload");
    assert_eq!(
        payload,
        CredentialPayload {
            access_token: "PLACEHOLDER-access-token-success".to_owned(),
            refresh_token: "PLACEHOLDER-refresh-token-success".to_owned(),
        },
        "the envelope decrypts to the returned token pair"
    );

    let expected_expiry =
        MutableClock::at("2026-08-26T12:00:00Z").now() + chrono::Duration::hours(2);
    assert_eq!(
        credential.expires_at,
        Some(expected_expiry),
        "expiry is created_at plus expires_in under the injected clock"
    );

    let state = x_persistence::credentials::account_state(&test.database, account)
        .await
        .expect("the account query succeeds")
        .expect("the seeded account exists");
    assert_eq!(state, "connected", "the account reads connected");

    let bodies = captured.0.bodies.lock().expect("captured bodies");
    assert!(
        !bodies.is_empty(),
        "the exchange posts to the token endpoint"
    );
    let body = &bodies[0];
    assert!(
        body.contains("grant_type=authorization_code"),
        "the grant type is the authorization code: {body}"
    );
    assert!(
        body.contains("code=the-authorization-code"),
        "the authorization code is carried: {body}"
    );
    assert!(
        body.contains("redirect_uri=https%3A%2F%2Fapp.example%2Fcallback"),
        "the registered redirect URI is carried: {body}"
    );
    assert!(
        body.contains("code_verifier=stored-pkce-verifier-from-the-intent"),
        "the stored verifier is presented: {body}"
    );
    drop(bodies);
    let authorizations = captured
        .0
        .authorizations
        .lock()
        .expect("captured auth headers");
    let header = authorizations[0]
        .as_deref()
        .expect("confidential clients authenticate");
    assert!(
        header.starts_with("Basic "),
        "client authentication uses HTTP Basic: {header}"
    );
}

#[tokio::test]
async fn downgraded_grant_refuses_activation_naming_missing_scopes() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/2/oauth2/token"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_body_string(fixture("token_downgraded_scope.json")),
        )
        .mount(&server)
        .await;

    let service = wired_service(&test, format!("{}/2/oauth2/token", server.uri()));
    let account = seed_account(&test, "exchange-downgraded-user").await;

    let outcome = service
        .exchange_code(
            account,
            &accepted_for(uuid::Uuid::from_u128(0x61)),
            "code-2",
        )
        .await;

    let error = outcome.expect_err("a downgraded grant must refuse activation");
    let FlowError::Downgrade { missing_scopes } = &error else {
        panic!("the refusal is the typed downgrade error: {error:?}")
    };
    assert_eq!(
        missing_scopes,
        &["bookmark.read".to_owned()],
        "every missing read scope is named"
    );

    let stored = x_persistence::credentials::latest_for_account(&test.database, account)
        .await
        .expect("the credential query succeeds")
        .expect("the observed grant stays auditable in a stored row");
    assert_eq!(
        stored.status, "expired",
        "the observed grant is recorded in a non-active credential row"
    );
    assert_eq!(
        stored.granted_scopes,
        vec![
            "users.read".to_owned(),
            "tweet.read".to_owned(),
            "offline.access".to_owned(),
        ],
        "the observed grant is recorded verbatim for audit"
    );

    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn scope_omitted_response_means_granted_equals_requested() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/2/oauth2/token"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_body_string(fixture("token_scope_omitted.json")),
        )
        .mount(&server)
        .await;

    let service = wired_service(&test, format!("{}/2/oauth2/token", server.uri()));
    let account = seed_account(&test, "exchange-scopeless-user").await;

    service
        .exchange_code(
            account,
            &accepted_for(uuid::Uuid::from_u128(0x62)),
            "code-3",
        )
        .await
        .expect("a scope-less response activates without any downgrade refusal");

    let stored = x_persistence::credentials::latest_for_account(&test.database, account)
        .await
        .expect("the credential query succeeds")
        .expect("the connection activated with a credential row");
    assert_eq!(stored.status, "active", "the connection is active");
    assert_eq!(
        stored.granted_scopes,
        vec![
            "users.read".to_owned(),
            "tweet.read".to_owned(),
            "bookmark.read".to_owned(),
            "offline.access".to_owned(),
        ],
        "the requested read set is recorded as granted per RFC 6749"
    );

    test.cleanup().await.expect("cleanup drops the database");
}
