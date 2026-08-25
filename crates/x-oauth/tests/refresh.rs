//! Rotation-aware refresh: atomic swaps, serialization under concurrency, reuse
//! detection, staleness, upstream invalidation versus transport failure.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::path::Path;
use std::sync::{Arc, Mutex};

use x_oauth::cipher::{Purpose, TokenCipher};
use x_oauth::clock::Clock;
use x_oauth::payload::CredentialPayload;
use x_oauth::service::ConnectionService;
use x_oauth::token_client::TokenClient;
use x_persistence::credentials::{self, NewCredential};
use x_persistence::test_support::TestDatabase;

/// The fixture directory at the workspace root.
fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/oauth")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("fixture {name} must be readable: {error}"))
}

/// A deterministic key so envelopes open the same way everywhere in this binary.
fn test_cipher() -> TokenCipher {
    TokenCipher::new(&[41u8; 32]).expect("a valid 32-byte test key")
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

/// Captures request bodies so assertions can inspect what was presented.
#[derive(Debug, Default, Clone)]
struct CaptureMatcher(Arc<CapturedBodies>);

#[derive(Debug, Default)]
struct CapturedBodies(Mutex<Vec<String>>);

impl wiremock::Match for CaptureMatcher {
    fn matches(&self, request: &wiremock::Request) -> bool {
        self.0
            .0
            .lock()
            .expect("an uncontended capture buffer")
            .push(String::from_utf8_lossy(&request.body).into_owned());
        true
    }
}

impl CaptureMatcher {
    fn snapshot(&self) -> Vec<String> {
        self.0
            .0
            .lock()
            .expect("an uncontended capture buffer")
            .clone()
    }
}

/// Configuration pointed at `token_url` with a confidential client registered.
fn oauth_config(token_url: String) -> x_core::config::OauthConfig {
    let mut oauth = x_core::config::XConfig::default().oauth;
    oauth.token_url.clone_from(&token_url);
    oauth.revocation_url = token_url;
    oauth.client_id = Some("test-client-id".to_owned());
    oauth.client_secret = Some("test-client-secret".to_owned());
    oauth.redirect_uri = Some("https://app.example/callback".to_owned());
    oauth
}

fn wired_service(
    test: &TestDatabase,
    token_url: String,
    clock: Arc<MutableClock>,
) -> ConnectionService {
    let client = TokenClient::new(&oauth_config(token_url.clone())).expect("an HTTP client");
    ConnectionService::new(
        test.database.clone(),
        test_cipher(),
        client,
        clock,
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

/// The lowercase SHA-256 hex digest used for retired-refresh comparisons.
fn sha256_hex(value: &str) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(value.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = std::fmt::Write::write_fmt(&mut hex, format_args!("{byte:02x}"));
    }
    hex
}

/// Seeds one active credential holding `tokens`, bound to `account`.
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
        &NewCredential {
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

const NOW: &str = "2026-08-26T12:00:00Z";

#[tokio::test]
async fn refresh_rotates_tokens_atomically_retiring_prior_hash() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let server = wiremock::MockServer::start().await;
    let captured = CaptureMatcher::default();
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/2/oauth2/token"))
        .and(captured.clone())
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_string(fixture("refresh_rotated.json")),
        )
        .mount(&server)
        .await;

    let clock = MutableClock::at(NOW);
    let service = wired_service(
        &test,
        format!("{}/2/oauth2/token", server.uri()),
        clock.clone(),
    );
    let account = seed_account(&test, "refresh-rotation-user").await;
    let current = "current-refresh-token-value";
    seed_active_credential(
        &test,
        account,
        CredentialPayload {
            access_token: "current-access-token-value".to_owned(),
            refresh_token: current.to_owned(),
        },
    )
    .await;

    service
        .refresh(account, Some(current))
        .await
        .expect("the refresh rotates the stored pair");

    let row = credentials::latest_for_account(&test.database, account)
        .await
        .expect("the credential query succeeds")
        .expect("a credential still exists after rotation");
    assert_eq!(row.status, "active", "rotation keeps the credential active");
    let opened = test_cipher()
        .open(account, Purpose::Credential, &row.encrypted_payload)
        .expect("the new envelope opens under the account binding");
    let payload = CredentialPayload::decode(&opened).expect("the rotated envelope decodes cleanly");
    assert_eq!(
        payload.access_token, "PLACEHOLDER-access-token-rotated",
        "the new access token replaced the old one atomically"
    );
    assert_eq!(
        payload.refresh_token, "PLACEHOLDER-refresh-token-rotated",
        "the new refresh token replaced the old one atomically"
    );
    let retired_hash = sha256_hex(current);
    assert_eq!(
        row.superseded_refresh_hash.as_deref(),
        Some(retired_hash.as_str()),
        "the prior refresh token's hash moved into superseded_refresh_hash"
    );

    let bodies = captured.snapshot();
    assert_eq!(bodies.len(), 1, "exactly one provider call happened");
    assert!(
        bodies[0].contains(&format!("refresh_token={current}")),
        "the current token was presented to the provider: {}",
        bodies[0]
    );

    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn retired_refresh_token_presentation_revokes_family() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let server = wiremock::MockServer::start().await;
    let captured = CaptureMatcher::default();
    // The only legitimate call is the first rotation; reuse must add none.
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::body_string_contains(
            "refresh_token=rotation-current-token",
        ))
        .and(captured.clone())
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_string(fixture("refresh_rotated.json")),
        )
        .mount(&server)
        .await;

    let clock = MutableClock::at(NOW);
    let service = wired_service(&test, format!("{}/2/oauth2/token", server.uri()), clock);
    let account = seed_account(&test, "refresh-reuse-user").await;
    let current = "rotation-current-token";
    seed_active_credential(
        &test,
        account,
        CredentialPayload {
            access_token: "rotation-access".to_owned(),
            refresh_token: current.to_owned(),
        },
    )
    .await;

    // Rotate once so `current` becomes the retired token.
    service
        .refresh(account, Some(current))
        .await
        .expect("the first rotation succeeds");

    // Presenting the retired token is reuse of a rotated family.
    let outcome = service.refresh(account, Some(current)).await;
    let error = outcome.expect_err("presenting the retired refresh token must be refused");
    assert!(
        matches!(error, x_oauth::error::FlowError::RefreshReplay),
        "the refusal must be the reuse detection error: {error:?}"
    );

    let row = credentials::latest_for_account(&test.database, account)
        .await
        .expect("the credential query succeeds")
        .expect("the credential row remains as evidence");
    assert_eq!(row.status, "revoked", "the family is revoked");
    assert!(
        row.encrypted_payload.is_empty(),
        "the secret material is scrubbed"
    );
    let state = credentials::account_state(&test.database, account)
        .await
        .expect("the account query succeeds");
    assert_eq!(
        state.as_deref(),
        Some("reauth_required"),
        "the account requires reauthorization after reuse"
    );
    assert!(
        captured.snapshot().len() == 1,
        "reuse detection must refuse locally without any further provider contact"
    );

    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn concurrent_refreshes_serialize_without_false_revocation() {
    let test = TestDatabase::create().await.expect("a disposable database");
    // The pool must be wide enough that racers contend on the row lock, not the pool.
    let server = wiremock::MockServer::start().await;
    let first_rotation = fixture("refresh_rotated.json");
    let second_rotation = serde_json::json!({
        "token_type": "bearer",
        "expires_in": 7200,
        "access_token": "PLACEHOLDER-access-token-second",
        "refresh_token": "PLACEHOLDER-refresh-token-second"
    })
    .to_string();
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/2/oauth2/token"))
        .and(wiremock::matchers::body_string_contains(
            "refresh_token=current-refresh-token-value",
        ))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(first_rotation))
        .mount(&server)
        .await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/2/oauth2/token"))
        .and(wiremock::matchers::body_string_contains(
            "refresh_token=PLACEHOLDER-refresh-token-rotated",
        ))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(second_rotation))
        .mount(&server)
        .await;

    let clock = MutableClock::at(NOW);
    let service = Arc::new(wired_service(
        &test,
        format!("{}/2/oauth2/token", server.uri()),
        clock,
    ));
    let account = seed_account(&test, "refresh-concurrent-user").await;
    seed_active_credential(
        &test,
        account,
        CredentialPayload {
            access_token: "current-access-token-value".to_owned(),
            refresh_token: "current-refresh-token-value".to_owned(),
        },
    )
    .await;

    let first = {
        let service = service.clone();
        tokio::spawn(async move { service.refresh(account, None).await })
    };
    let second = {
        let service = service.clone();
        tokio::spawn(async move { service.refresh(account, None).await })
    };
    let first_outcome = first.await.expect("the task joins");
    let second_outcome = second.await.expect("the task joins");
    first_outcome.expect("the first refresh succeeds");
    second_outcome.expect("the second refresh succeeds");

    let row = credentials::latest_for_account(&test.database, account)
        .await
        .expect("the credential query succeeds")
        .expect("the credential survives both rotations");
    assert_eq!(
        row.status, "active",
        "serialization must never produce a false revocation"
    );
    let opened = test_cipher()
        .open(account, Purpose::Credential, &row.encrypted_payload)
        .expect("the final envelope opens");
    let payload = CredentialPayload::decode(&opened).expect("the final envelope decodes cleanly");
    assert_eq!(
        payload.access_token, "PLACEHOLDER-access-token-second",
        "the final credential holds the second rotation's access token"
    );
    assert_eq!(
        payload.refresh_token, "PLACEHOLDER-refresh-token-second",
        "the final credential holds the second rotation's refresh token"
    );
    assert_eq!(
        row.superseded_refresh_hash.as_deref(),
        Some(sha256_hex("PLACEHOLDER-refresh-token-rotated").as_str()),
        "the first rotation's token is the retired one"
    );
    let state = credentials::account_state(&test.database, account)
        .await
        .expect("the account query succeeds");
    assert_eq!(
        state.as_deref(),
        Some("connected"),
        "no reauth-required state may appear from racing refreshes"
    );

    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn unknown_stale_token_is_refused_without_touching_stored_credential() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let server = wiremock::MockServer::start().await;
    let captured = CaptureMatcher::default();
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(captured.clone())
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_string(fixture("refresh_rotated.json")),
        )
        .mount(&server)
        .await;

    let clock = MutableClock::at(NOW);
    let service = wired_service(&test, format!("{}/2/oauth2/token", server.uri()), clock);
    let account = seed_account(&test, "refresh-stale-user").await;
    seed_active_credential(
        &test,
        account,
        CredentialPayload {
            access_token: "stale-case-access".to_owned(),
            refresh_token: "stale-case-current-refresh".to_owned(),
        },
    )
    .await;

    let outcome = service
        .refresh(account, Some("neither-current-nor-retired"))
        .await;
    let error = outcome.expect_err("an unknown stale presentation must be refused");
    assert!(
        matches!(error, x_oauth::error::FlowError::StaleRefreshToken),
        "the refusal must be the stale error: {error:?}"
    );

    let row = credentials::latest_for_account(&test.database, account)
        .await
        .expect("the credential query succeeds")
        .expect("the stored credential remains");
    assert_eq!(row.status, "active", "the stored credential stays active");
    assert!(
        !row.encrypted_payload.is_empty(),
        "the payload keeps its secret material"
    );
    let opened = test_cipher()
        .open(account, Purpose::Credential, &row.encrypted_payload)
        .expect("the untouched envelope still opens");
    let payload =
        CredentialPayload::decode(&opened).expect("the untouched envelope decodes cleanly");
    assert_eq!(
        payload,
        CredentialPayload {
            access_token: "stale-case-access".to_owned(),
            refresh_token: "stale-case-current-refresh".to_owned(),
        },
        "the stored pair is byte-for-byte unchanged"
    );
    assert_eq!(
        row.superseded_refresh_hash, None,
        "no retirement hash appeared from a stale refusal"
    );
    let state = credentials::account_state(&test.database, account)
        .await
        .expect("the account query succeeds");
    assert_eq!(
        state.as_deref(),
        Some("connected"),
        "the account state is untouched by a stale refusal"
    );
    assert!(
        captured.snapshot().is_empty(),
        "a stale refusal makes no provider call at all"
    );

    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn inactive_credential_refuses_refresh_without_provider_contact() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let server = wiremock::MockServer::start().await;
    let captured = CaptureMatcher::default();
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(captured.clone())
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_string(fixture("refresh_rotated.json")),
        )
        .mount(&server)
        .await;

    let clock = MutableClock::at(NOW);
    let service = wired_service(&test, format!("{}/2/oauth2/token", server.uri()), clock);
    let account = seed_account(&test, "refresh-inactive-user").await;
    credentials::insert_with_status(
        &test.database,
        &NewCredential {
            account_id: account,
            encrypted_payload: &[],
            granted_scopes: &[],
            expires_at: None,
        },
        "revoked",
    )
    .await
    .expect("the revoked credential seeds");

    let outcome = service.refresh(account, Some("any-presented-token")).await;
    let error = outcome.expect_err("an inactive credential must refuse refresh");
    assert!(
        matches!(error, x_oauth::error::FlowError::CredentialInactive),
        "the refusal must be the typed inactive error: {error:?}"
    );
    assert!(
        captured.snapshot().is_empty(),
        "the provider must receive no request for an inactive credential"
    );

    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn upstream_invalidation_differs_from_transport_failure() {
    // Case A: a completed provider rejection carrying invalid_grant.
    let test = TestDatabase::create().await.expect("a disposable database");
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/2/oauth2/token"))
        .respond_with(
            wiremock::ResponseTemplate::new(400)
                .set_body_string(fixture("token_invalid_grant.json")),
        )
        .mount(&server)
        .await;

    let clock = MutableClock::at(NOW);
    let service = wired_service(&test, format!("{}/2/oauth2/token", server.uri()), clock);
    let account = seed_account(&test, "refresh-invalidated-user").await;
    seed_active_credential(
        &test,
        account,
        CredentialPayload {
            access_token: "evidence-access".to_owned(),
            refresh_token: "evidence-refresh".to_owned(),
        },
    )
    .await;

    let outcome = service.refresh(account, Some("evidence-refresh")).await;
    let error = outcome.expect_err("an upstream invalidation must fail the refresh");
    assert!(
        matches!(error, x_oauth::error::FlowError::UpstreamInvalidation),
        "the provider's invalid_grant is an invalidation, not a transient: {error:?}"
    );

    let row = credentials::latest_for_account(&test.database, account)
        .await
        .expect("the credential query succeeds")
        .expect("the evidence payload stays stored");
    assert_eq!(row.status, "expired", "invalidation expires the credential");
    let opened = test_cipher()
        .open(account, Purpose::Credential, &row.encrypted_payload)
        .expect("the evidence payload is retained and still decrypts");
    let payload = CredentialPayload::decode(&opened).expect("the retained envelope decodes");
    assert_eq!(
        payload.refresh_token, "evidence-refresh",
        "invalidation retains the evidence rather than scrubbing it"
    );
    let state = credentials::account_state(&test.database, account)
        .await
        .expect("the account query succeeds");
    assert_eq!(
        state.as_deref(),
        Some("reauth_required"),
        "invalidation marks the account reauth-required"
    );
    test.cleanup().await.expect("cleanup drops the database");

    // Case B: a refused endpoint is a transient failure changing nothing.
    let test = TestDatabase::create().await.expect("a disposable database");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("an ephemeral bind");
    let port = listener.local_addr().expect("a bound port").port();
    drop(listener);
    let refused_uri = format!("http://127.0.0.1:{port}/2/oauth2/token");

    let clock = MutableClock::at(NOW);
    let service = wired_service(&test, refused_uri, clock);
    let account = seed_account(&test, "refresh-transport-user").await;
    seed_active_credential(
        &test,
        account,
        CredentialPayload {
            access_token: "transport-access".to_owned(),
            refresh_token: "transport-refresh".to_owned(),
        },
    )
    .await;

    let outcome = service.refresh(account, Some("transport-refresh")).await;
    let error = outcome.expect_err("a connection-refused endpoint must fail the refresh");
    assert!(
        matches!(error, x_oauth::error::FlowError::Transport),
        "a transport-level failure returns the transient variant: {error:?}"
    );
    let row = credentials::latest_for_account(&test.database, account)
        .await
        .expect("the credential query succeeds")
        .expect("the credential survives a transport failure untouched");
    assert_eq!(row.status, "active", "transport failure mutates no status");
    let opened = test_cipher()
        .open(account, Purpose::Credential, &row.encrypted_payload)
        .expect("the payload survives a transport failure");
    let payload = CredentialPayload::decode(&opened).expect("the intact envelope decodes");
    assert_eq!(payload.refresh_token, "transport-refresh");
    let superseded_absent = row.superseded_refresh_hash.is_none();
    assert!(
        superseded_absent,
        "no rotation hash appears on transport failure"
    );

    test.cleanup().await.expect("cleanup drops the database");
}
