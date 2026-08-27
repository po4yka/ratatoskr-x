//! Code exchange against a wiremock provider double serving recorded fixtures.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "assertions in a test binary"
)]

use std::sync::{Arc, Mutex};

use x_oauth::callback::AcceptedCallback;
use x_oauth::cipher::{Purpose, TokenCipher};
use x_oauth::clock::Clock;
use x_oauth::error::FlowError;
use x_oauth::intent::IntentPurpose;
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
        intent_id: uuid::Uuid::nil(),
        internal_user_id: user,
        purpose: IntentPurpose::ReadConnection,
        requested_scopes: x_core::config::XConfig::default().oauth.read_scopes,
        code_verifier: "stored-pkce-verifier-from-the-intent".to_owned(),
    }
}

struct WriteExchangeObservation {
    outcome: Result<(), FlowError>,
    previous_credential: uuid::Uuid,
    previous_payload: Vec<u8>,
    current: (uuid::Uuid, Vec<u8>, Vec<String>),
    activation_probe_rows: u64,
    authenticated_user_requests: usize,
    rejected_grants: Vec<(Vec<String>, Vec<u8>, String, String)>,
}

async fn observe_write_exchange(
    case: &str,
    granted_scope: &str,
    authenticated_provider_user_id: &str,
) -> WriteExchangeObservation {
    let test = TestDatabase::create().await.expect("a disposable database");
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/2/oauth2/token"))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "token_type": "bearer",
                "expires_in": 7200,
                "access_token": format!("PLACEHOLDER-access-token-{case}"),
                "refresh_token": format!("PLACEHOLDER-refresh-token-{case}"),
                "scope": granted_scope
            })),
        )
        .mount(&server)
        .await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/2/users/me"))
        .and(wiremock::matchers::header(
            "authorization",
            format!("Bearer PLACEHOLDER-access-token-{case}"),
        ))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "id": authenticated_provider_user_id }
            })),
        )
        .mount(&server)
        .await;

    let service = wired_service(&test, format!("{}/2/oauth2/token", server.uri()));
    let user = uuid::Uuid::from_u128(0x626f_6f6b_6d61_726b);
    let account = seed_account(&test, "provider-target-account").await;
    test.database
        .query_raw(&format!(
            "update x_archive.accounts set internal_user_id = '{user}' where id = '{account}'"
        ))
        .await
        .expect("the account is bound to the authenticated owner");
    let requested_scopes = vec![
        "users.read".to_owned(),
        "tweet.read".to_owned(),
        "bookmark.read".to_owned(),
        "offline.access".to_owned(),
        "bookmark.write".to_owned(),
    ];
    let read_scopes = vec![
        "users.read".to_owned(),
        "tweet.read".to_owned(),
        "bookmark.read".to_owned(),
        "offline.access".to_owned(),
    ];
    let intent_id = x_persistence::oauth_intents::insert_intent(
        &test.database,
        &x_persistence::oauth_intents::NewIntent {
            internal_user_id: user,
            account_id: Some(account),
            purpose: "bookmark_write",
            state_hash: "f22ce33d7c7d72c8df61a01b800ffdf640ea7ad6f03ca44b8f46d0c8a7269a6f",
            code_verifier_encrypted: b"REDACTED-intent-verifier-envelope",
            nonce: "bookmark-write-intent-nonce",
            redirect_uri: "https://app.example/callback",
            requested_scopes: &requested_scopes,
            created_at: chrono::DateTime::parse_from_rfc3339("2026-08-26T11:55:00Z")
                .expect("a fixed creation instant")
                .with_timezone(&chrono::Utc),
            expires_at: chrono::DateTime::parse_from_rfc3339("2026-08-26T12:05:00Z")
                .expect("a fixed expiry instant")
                .with_timezone(&chrono::Utc),
        },
    )
    .await
    .expect("the account-bound write intent seeds");
    let previous_payload = test_cipher().seal(
        account,
        Purpose::Credential,
        &CredentialPayload {
            access_token: "PLACEHOLDER-access-token-read".to_owned(),
            refresh_token: "PLACEHOLDER-refresh-token-read".to_owned(),
        }
        .encode(),
    );
    let previous_credential = x_persistence::credentials::insert_with_status(
        &test.database,
        &x_persistence::credentials::NewCredential {
            account_id: account,
            encrypted_payload: &previous_payload,
            granted_scopes: &read_scopes,
            expires_at: None,
        },
        "active",
    )
    .await
    .expect("the prior read credential seeds");
    let accepted = AcceptedCallback {
        intent_id,
        internal_user_id: user,
        purpose: IntentPurpose::BookmarkWrite {
            account_id: account,
        },
        requested_scopes: requested_scopes.clone(),
        code_verifier: "stored-bookmark-write-pkce-verifier".to_owned(),
    };

    let outcome = service
        .exchange_bookmark_write_code(&accepted, "bookmark-write-authorization-code")
        .await;
    let authenticated_user_requests = server
        .received_requests()
        .await
        .expect("the mock request ledger is readable")
        .iter()
        .filter(|request| request.method.as_str() == "GET" && request.url.path() == "/2/users/me")
        .count();
    let current = sqlx::query_as(
        "select id, encrypted_payload, granted_scopes from x_archive.credentials \
         where account_id = $1 and status = 'active' order by created_at desc, id desc limit 1",
    )
    .bind(account)
    .fetch_optional(test.database.pool())
    .await
    .expect("the current credential is readable")
    .expect("the account retains a credential");
    let activation_probe_rows = test
        .database
        .query_raw(&format!(
            "insert into x_archive.bookmark_write_authorizations \
             (account_id, oauth_intent_id, granted_scopes, status, authorized_at) \
             values ('{account}', '{intent_id}', \
                     array['users.read','tweet.read','bookmark.read','offline.access','bookmark.write'], \
                     'active', '2026-08-26T12:00:00Z') \
             on conflict (account_id) do nothing"
        ))
        .await
        .expect("the activation existence probe executes")
        .rows_affected();
    let rejected_grants = sqlx::query_as(
        "select granted_scopes, encrypted_payload, status, activation_outcome \
         from x_archive.credentials \
         where account_id = $1 and id <> $2 and status = 'expired' order by created_at, id",
    )
    .bind(account)
    .bind(previous_credential)
    .fetch_all(test.database.pool())
    .await
    .expect("rejected grant evidence is readable");

    test.cleanup().await.expect("cleanup drops the database");
    WriteExchangeObservation {
        outcome,
        previous_credential,
        previous_payload,
        current,
        activation_probe_rows,
        authenticated_user_requests,
        rejected_grants,
    }
}

#[tokio::test]
async fn write_callback_replaces_credential_only_for_matching_account_and_complete_scope() {
    let complete_scopes = [
        "users.read",
        "tweet.read",
        "bookmark.read",
        "offline.access",
        "bookmark.write",
    ]
    .join(" ");
    let read_scopes = [
        "users.read",
        "tweet.read",
        "bookmark.read",
        "offline.access",
    ]
    .join(" ");
    let matching = observe_write_exchange(
        "matching-complete",
        &complete_scopes,
        "provider-target-account",
    )
    .await;
    let missing_scope = observe_write_exchange(
        "missing-write-scope",
        &read_scopes,
        "provider-target-account",
    )
    .await;
    let identity_mismatch = observe_write_exchange(
        "identity-mismatch",
        &complete_scopes,
        "provider-other-account",
    )
    .await;
    let expected_complete_scopes = vec![
        "users.read".to_owned(),
        "tweet.read".to_owned(),
        "bookmark.read".to_owned(),
        "offline.access".to_owned(),
        "bookmark.write".to_owned(),
    ];
    let mut violations = Vec::new();
    if matching.outcome.is_err() {
        violations.push(format!(
            "matching complete grant was refused: {:?}",
            matching.outcome
        ));
    }
    if matching.current.0 == matching.previous_credential
        || matching.current.2 != expected_complete_scopes
    {
        violations.push("matching complete grant did not replace the read credential".to_owned());
    }
    if matching.activation_probe_rows != 0 {
        violations.push("matching complete grant did not activate write authority".to_owned());
    }
    if matching.authenticated_user_requests != 1 {
        violations
            .push("matching complete grant did not authenticate provider identity".to_owned());
    }
    match &missing_scope.outcome {
        Err(FlowError::Downgrade { missing_scopes })
            if missing_scopes == &["bookmark.write".to_owned()] => {}
        other => violations.push(format!(
            "missing bookmark.write was not a typed downgrade: {other:?}"
        )),
    }
    if missing_scope.current.0 != missing_scope.previous_credential
        || missing_scope.current.1 != missing_scope.previous_payload
    {
        violations.push("scope downgrade replaced the prior credential".to_owned());
    }
    if missing_scope.activation_probe_rows != 1 {
        violations.push("scope downgrade activated write authority".to_owned());
    }
    if missing_scope.rejected_grants
        != vec![(
            vec![
                "users.read".to_owned(),
                "tweet.read".to_owned(),
                "bookmark.read".to_owned(),
                "offline.access".to_owned(),
            ],
            Vec::new(),
            "expired".to_owned(),
            "scope_downgrade".to_owned(),
        )]
    {
        violations.push(format!(
            "scope downgrade did not retain secret-free grant evidence: {:?}",
            missing_scope.rejected_grants
        ));
    }
    if !matches!(
        &identity_mismatch.outcome,
        Err(FlowError::ProviderIdentityMismatch)
    ) {
        violations.push(format!(
            "provider identity mismatch was not typed: {:?}",
            identity_mismatch.outcome
        ));
    }
    if identity_mismatch.authenticated_user_requests != 1 {
        violations
            .push("identity mismatch did not inspect authenticated provider identity".to_owned());
    }
    if identity_mismatch.current.0 != identity_mismatch.previous_credential
        || identity_mismatch.current.1 != identity_mismatch.previous_payload
    {
        violations.push("identity mismatch replaced the prior credential".to_owned());
    }
    if identity_mismatch.activation_probe_rows != 1 {
        violations.push("identity mismatch activated write authority".to_owned());
    }
    if identity_mismatch.rejected_grants
        != vec![(
            expected_complete_scopes,
            Vec::new(),
            "expired".to_owned(),
            "provider_identity_mismatch".to_owned(),
        )]
    {
        violations.push(format!(
            "identity mismatch did not retain secret-free grant evidence: {:?}",
            identity_mismatch.rejected_grants
        ));
    }

    assert!(
        violations.is_empty(),
        "bookmark-write callback matrix violations: {violations:?}"
    );
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
