//! Official bookmark mutation request shapes against a redacted local provider fixture.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "provider scenario tests keep complete request, failure, and secrecy assertions visible"
)]

use x_oauth::cipher::{Purpose, TokenCipher};
use x_oauth::payload::CredentialPayload;
use x_persistence::test_support::TestDatabase;
use x_sync::{BookmarkMutationProvider, OfficialBookmarkProvider};

#[tokio::test]
async fn add_uses_authenticated_account_post_endpoint_and_expected_body() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let provider_user_id = "44196397";
    let provider_post_id = "1890123456789012400";
    let account = test
        .seed_account(provider_user_id)
        .await
        .expect("the connected provider account seeds");
    let cipher = TokenCipher::new(&[0x6au8; 32]).expect("the test cipher key is valid");
    let marker_access_token = "MARKER-bookmark-provider-access-token";
    let encrypted_payload = cipher.seal(
        account,
        Purpose::Credential,
        &CredentialPayload {
            access_token: marker_access_token.to_owned(),
            refresh_token: "MARKER-bookmark-provider-refresh-token".to_owned(),
        }
        .encode(),
    );
    let scopes = vec![
        "users.read".to_owned(),
        "tweet.read".to_owned(),
        "bookmark.read".to_owned(),
        "offline.access".to_owned(),
        "bookmark.write".to_owned(),
    ];
    x_persistence::credentials::insert_with_status(
        &test.database,
        &x_persistence::credentials::NewCredential {
            account_id: account,
            encrypted_payload: &encrypted_payload,
            granted_scopes: &scopes,
            expires_at: None,
        },
        "active",
    )
    .await
    .expect("the active encrypted marker credential seeds");
    let intent_id: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.oauth_intents \
         (internal_user_id, account_id, purpose, state_hash, code_verifier_encrypted, nonce, \
          redirect_uri, requested_scopes, created_at, expires_at) \
         select internal_user_id, id, 'bookmark_write', \
                'bbfb0fb46ccb0b4e30e4a64ab0fcb0822b6575936d14f7222f47de334ec0100f', \
                'REDACTED'::bytea, 'provider-test', 'https://app.example/callback', $2, \
                '2026-08-27T12:00:00Z', '2026-08-27T12:10:00Z' \
         from x_archive.accounts where id = $1 returning id",
    )
    .bind(account)
    .bind(&scopes)
    .fetch_one(test.database.pool())
    .await
    .expect("the account-bound write intent seeds");
    sqlx::query(
        "insert into x_archive.bookmark_write_authorizations \
         (account_id, oauth_intent_id, granted_scopes, status, authorized_at) \
         values ($1, $2, $3, 'active', '2026-08-27T12:00:00Z')",
    )
    .bind(account)
    .bind(intent_id)
    .bind(&scopes)
    .execute(test.database.pool())
    .await
    .expect("the local bookmark-write authorization seeds");

    let server = wiremock::MockServer::start().await;
    let expected_path = format!("/2/users/{provider_user_id}/bookmarks");
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path(expected_path.clone()))
        .and(wiremock::matchers::header(
            "authorization",
            format!("Bearer {marker_access_token}"),
        ))
        .and(wiremock::matchers::body_json(serde_json::json!({
            "tweet_id": provider_post_id
        })))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "bookmarked": true }
            })),
        )
        .mount(&server)
        .await;
    let provider = OfficialBookmarkProvider::new(test.database.clone(), cipher, server.uri());

    let outcome = provider.add_bookmark(account, provider_post_id).await;
    let requests = server
        .received_requests()
        .await
        .expect("request recording remains enabled");

    test.cleanup().await.expect("cleanup drops the database");

    let mut violations = Vec::new();
    if outcome.is_err() {
        violations.push(format!(
            "confirmed bookmarked=true response was not accepted: {outcome:?}"
        ));
    }
    if requests.len() != 1 {
        violations.push(format!(
            "provider received {} requests instead of exactly one",
            requests.len()
        ));
    } else if let Some(request) = requests.first() {
        if request.method.as_str() != "POST" {
            violations.push(format!("provider method was {}", request.method));
        }
        if request.url.path() != expected_path {
            violations.push(format!("provider path was {}", request.url.path()));
        }
        let expected_authorization = format!("Bearer {marker_access_token}");
        let actual_authorization = request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok());
        if actual_authorization != Some(expected_authorization.as_str()) {
            violations.push("provider authorization was absent or not account-bound".to_owned());
        }
        match request.body_json::<serde_json::Value>() {
            Ok(body) if body == serde_json::json!({ "tweet_id": provider_post_id }) => {}
            Ok(_) => violations.push("provider JSON contained fields beyond tweet_id".to_owned()),
            Err(_) => violations.push("provider body was not valid JSON".to_owned()),
        }
    }
    assert!(
        violations.is_empty(),
        "bookmark add adapter violations: {violations:?}"
    );
}

#[tokio::test]
async fn remove_uses_authenticated_account_and_target_path() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let provider_user_id = "44196397";
    let provider_post_id = "1890123456789012400";
    let account = test
        .seed_account(provider_user_id)
        .await
        .expect("the connected provider account seeds");
    let cipher = TokenCipher::new(&[0x6au8; 32]).expect("the test cipher key is valid");
    let marker_access_token = "MARKER-bookmark-provider-access-token";
    let encrypted_payload = cipher.seal(
        account,
        Purpose::Credential,
        &CredentialPayload {
            access_token: marker_access_token.to_owned(),
            refresh_token: "MARKER-bookmark-provider-refresh-token".to_owned(),
        }
        .encode(),
    );
    let scopes = vec![
        "users.read".to_owned(),
        "tweet.read".to_owned(),
        "bookmark.read".to_owned(),
        "offline.access".to_owned(),
        "bookmark.write".to_owned(),
    ];
    x_persistence::credentials::insert_with_status(
        &test.database,
        &x_persistence::credentials::NewCredential {
            account_id: account,
            encrypted_payload: &encrypted_payload,
            granted_scopes: &scopes,
            expires_at: None,
        },
        "active",
    )
    .await
    .expect("the active encrypted marker credential seeds");
    let intent_id: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.oauth_intents \
         (internal_user_id, account_id, purpose, state_hash, code_verifier_encrypted, nonce, \
          redirect_uri, requested_scopes, created_at, expires_at) \
         select internal_user_id, id, 'bookmark_write', \
                'bbfb0fb46ccb0b4e30e4a64ab0fcb0822b6575936d14f7222f47de334ec0100f', \
                'REDACTED'::bytea, 'provider-remove-test', 'https://app.example/callback', $2, \
                '2026-08-27T12:00:00Z', '2026-08-27T12:10:00Z' \
         from x_archive.accounts where id = $1 returning id",
    )
    .bind(account)
    .bind(&scopes)
    .fetch_one(test.database.pool())
    .await
    .expect("the account-bound write intent seeds");
    sqlx::query(
        "insert into x_archive.bookmark_write_authorizations \
         (account_id, oauth_intent_id, granted_scopes, status, authorized_at) \
         values ($1, $2, $3, 'active', '2026-08-27T12:00:00Z')",
    )
    .bind(account)
    .bind(intent_id)
    .bind(&scopes)
    .execute(test.database.pool())
    .await
    .expect("the local bookmark-write authorization seeds");

    let server = wiremock::MockServer::start().await;
    let expected_path = format!("/2/users/{provider_user_id}/bookmarks/{provider_post_id}");
    wiremock::Mock::given(wiremock::matchers::method("DELETE"))
        .and(wiremock::matchers::path(expected_path.clone()))
        .and(wiremock::matchers::header(
            "authorization",
            format!("Bearer {marker_access_token}"),
        ))
        .and(wiremock::matchers::body_bytes(Vec::<u8>::new()))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "data": { "bookmarked": false }
            })),
        )
        .mount(&server)
        .await;
    let provider = OfficialBookmarkProvider::new(test.database.clone(), cipher, server.uri());

    let outcome = provider.remove_bookmark(account, provider_post_id).await;
    let requests = server
        .received_requests()
        .await
        .expect("request recording remains enabled");

    test.cleanup().await.expect("cleanup drops the database");

    let mut violations = Vec::new();
    if outcome.is_err() {
        violations.push(format!(
            "confirmed bookmarked=false response was not accepted: {outcome:?}"
        ));
    }
    if requests.len() != 1 {
        violations.push(format!(
            "provider received {} requests instead of exactly one",
            requests.len()
        ));
    } else if let Some(request) = requests.first() {
        if request.method.as_str() != "DELETE" {
            violations.push(format!("provider method was {}", request.method));
        }
        if request.url.path() != expected_path {
            violations.push(format!("provider path was {}", request.url.path()));
        }
        let expected_authorization = format!("Bearer {marker_access_token}");
        let actual_authorization = request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok());
        if actual_authorization != Some(expected_authorization.as_str()) {
            violations.push("provider authorization was absent or not account-bound".to_owned());
        }
        if !request.body.is_empty() {
            violations.push("provider DELETE request unexpectedly carried a body".to_owned());
        }
    }
    assert!(
        violations.is_empty(),
        "bookmark remove adapter violations: {violations:?}"
    );
}

#[tokio::test]
async fn provider_failures_are_bounded_redacted_and_not_retried() {
    use std::time::{Duration, Instant};

    let test = TestDatabase::create().await.expect("a disposable database");
    let provider_user_id = "44196397";
    let provider_post_id = "1890123456789012400";
    let account = test
        .seed_account(provider_user_id)
        .await
        .expect("the connected provider account seeds");
    let cipher = TokenCipher::new(&[0x6au8; 32]).expect("the test cipher key is valid");
    let marker_access_token = "MARKER-private-access-token";
    let marker_refresh_token = "MARKER-private-refresh-token";
    let private_response_content = "PRIVATE-post-content-must-not-escape";
    let encrypted_payload = cipher.seal(
        account,
        Purpose::Credential,
        &CredentialPayload {
            access_token: marker_access_token.to_owned(),
            refresh_token: marker_refresh_token.to_owned(),
        }
        .encode(),
    );
    let scopes = vec![
        "users.read".to_owned(),
        "tweet.read".to_owned(),
        "bookmark.read".to_owned(),
        "offline.access".to_owned(),
        "bookmark.write".to_owned(),
    ];
    x_persistence::credentials::insert_with_status(
        &test.database,
        &x_persistence::credentials::NewCredential {
            account_id: account,
            encrypted_payload: &encrypted_payload,
            granted_scopes: &scopes,
            expires_at: None,
        },
        "active",
    )
    .await
    .expect("the active encrypted marker credential seeds");
    let intent_id: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.oauth_intents \
         (internal_user_id, account_id, purpose, state_hash, code_verifier_encrypted, nonce, \
          redirect_uri, requested_scopes, created_at, expires_at) \
         select internal_user_id, id, 'bookmark_write', \
                'bbfb0fb46ccb0b4e30e4a64ab0fcb0822b6575936d14f7222f47de334ec0100f', \
                'REDACTED'::bytea, 'provider-failure-test', 'https://app.example/callback', $2, \
                '2026-08-27T12:00:00Z', '2026-08-27T12:10:00Z' \
         from x_archive.accounts where id = $1 returning id",
    )
    .bind(account)
    .bind(&scopes)
    .fetch_one(test.database.pool())
    .await
    .expect("the account-bound write intent seeds");
    sqlx::query(
        "insert into x_archive.bookmark_write_authorizations \
         (account_id, oauth_intent_id, granted_scopes, status, authorized_at) \
         values ($1, $2, $3, 'active', '2026-08-27T12:00:00Z')",
    )
    .bind(account)
    .bind(intent_id)
    .bind(&scopes)
    .execute(test.database.pool())
    .await
    .expect("the local bookmark-write authorization seeds");

    let rate_reset = "1787851200";
    let rate_request_id = "redacted-request-id-429";
    let cases = vec![
        (
            "authorization loss",
            wiremock::ResponseTemplate::new(401).set_body_string(private_response_content),
            "authorization",
            None,
        ),
        (
            "rate limit",
            wiremock::ResponseTemplate::new(429)
                .insert_header("x-rate-limit-reset", rate_reset)
                .insert_header("x-request-id", rate_request_id)
                .set_body_string(private_response_content),
            "rate",
            Some((rate_reset, rate_request_id)),
        ),
        (
            "definite refusal",
            wiremock::ResponseTemplate::new(400).set_body_string(private_response_content),
            "refusal",
            None,
        ),
        (
            "oversized success",
            wiremock::ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 64 * 1024 + 1]),
            "uncertain",
            None,
        ),
        (
            "malformed success",
            wiremock::ResponseTemplate::new(200)
                .set_body_string(format!("{{not-json:{private_response_content}")),
            "uncertain",
            None,
        ),
        (
            "ambiguous server failure",
            wiremock::ResponseTemplate::new(500).set_body_string(private_response_content),
            "uncertain",
            None,
        ),
    ];
    let expected_path = format!("/2/users/{provider_user_id}/bookmarks");
    let mut violations = Vec::new();

    for (case_name, response, expected_class, expected_rate_metadata) in cases {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path(expected_path.clone()))
            .respond_with(response)
            .mount(&server)
            .await;
        let provider =
            OfficialBookmarkProvider::new(test.database.clone(), cipher.clone(), server.uri());

        let outcome = provider.add_bookmark(account, provider_post_id).await;
        let requests = server
            .received_requests()
            .await
            .expect("request recording remains enabled");
        let diagnostic = match &outcome {
            Ok(_) => format!("{provider:?} {outcome:?}"),
            Err(error) => format!("{provider:?} {error:?} {error}"),
        };
        let diagnostic_lower = diagnostic.to_ascii_lowercase();

        if !diagnostic_lower.contains(expected_class) {
            violations.push(format!(
                "{case_name} classified as {diagnostic} instead of {expected_class}"
            ));
        }
        if requests.len() != 1 {
            violations.push(format!(
                "{case_name} made {} attempts instead of exactly one",
                requests.len()
            ));
        }
        if let Some((expected_reset, expected_request_id)) = expected_rate_metadata
            && (!diagnostic.contains(expected_reset) || !diagnostic.contains(expected_request_id))
        {
            violations.push(format!(
                "{case_name} omitted bounded reset/request-id evidence: {diagnostic}"
            ));
        }
        for forbidden in [
            marker_access_token,
            marker_refresh_token,
            private_response_content,
            &format!("Bearer {marker_access_token}"),
        ] {
            if diagnostic.contains(forbidden) {
                violations.push(format!("{case_name} diagnostic leaked forbidden evidence"));
            }
        }
    }

    let unavailable_listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("an unused local port binds");
    let unavailable_address = unavailable_listener
        .local_addr()
        .expect("the unused local address is known");
    drop(unavailable_listener);
    let unavailable_provider = OfficialBookmarkProvider::new(
        test.database.clone(),
        cipher.clone(),
        format!("http://{unavailable_address}"),
    );
    let connect_outcome = tokio::time::timeout(
        Duration::from_secs(2),
        unavailable_provider.add_bookmark(account, provider_post_id),
    )
    .await;
    let connect_diagnostic = format!("{unavailable_provider:?} {connect_outcome:?}");
    if !connect_diagnostic
        .to_ascii_lowercase()
        .contains("transient")
    {
        violations.push(format!(
            "connect failure classified as {connect_diagnostic} instead of transient"
        ));
    }
    if connect_outcome.is_err() {
        violations.push("connect failure exceeded the two-second test bound".to_owned());
    }

    let delayed_server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path(expected_path))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(1_250))
                .set_body_json(serde_json::json!({ "data": { "bookmarked": true } })),
        )
        .mount(&delayed_server)
        .await;
    let delayed_provider = OfficialBookmarkProvider::with_timeout(
        test.database.clone(),
        cipher,
        delayed_server.uri(),
        Duration::from_secs(1),
    );
    let delayed_started = Instant::now();
    let delayed_outcome = tokio::time::timeout(
        Duration::from_secs(2),
        delayed_provider.add_bookmark(account, provider_post_id),
    )
    .await;
    let delayed_elapsed = delayed_started.elapsed();
    let delayed_requests = delayed_server
        .received_requests()
        .await
        .expect("request recording remains enabled");
    let delayed_diagnostic = format!("{delayed_provider:?} {delayed_outcome:?}");
    if !delayed_diagnostic
        .to_ascii_lowercase()
        .contains("uncertain")
    {
        violations.push(format!(
            "delayed response classified as {delayed_diagnostic} instead of uncertain timeout"
        ));
    }
    if delayed_elapsed > Duration::from_secs(2) {
        violations.push(format!(
            "delayed response exceeded its two-second bound: {delayed_elapsed:?}"
        ));
    }
    if delayed_requests.len() != 1 {
        violations.push(format!(
            "delayed response made {} attempts instead of exactly one",
            delayed_requests.len()
        ));
    }
    for forbidden in [
        marker_access_token,
        marker_refresh_token,
        private_response_content,
        &format!("Bearer {marker_access_token}"),
    ] {
        if connect_diagnostic.contains(forbidden) || delayed_diagnostic.contains(forbidden) {
            violations.push("transport/timeout diagnostic leaked forbidden evidence".to_owned());
        }
    }

    test.cleanup().await.expect("cleanup drops the database");

    assert!(
        violations.is_empty(),
        "bookmark provider failure-policy violations: {violations:?}"
    );
}
