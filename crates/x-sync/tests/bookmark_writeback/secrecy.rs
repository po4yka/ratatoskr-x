//! Bookmark write-back secrecy scenarios.

use super::*;

#[tokio::test]
async fn audit_and_diagnostics_exclude_tokens_headers_content_and_raw_bodies() {
    let (test, account, owner, service, instant) = authorized_writeback_fixture().await;
    let marker_access_token = "MARKER-83-private-access-token";
    let marker_refresh_token = "MARKER-83-private-refresh-token";
    let marker_private_post_content = "MARKER-83-private-post-content";
    let marker_raw_provider_body = "MARKER-83-raw-provider-json-body";
    let marker_authorization_header = format!("Bearer {marker_access_token}");
    let success_request_id = "safe-success-request-83";
    let uncertain_request_id = "safe-uncertain-request-83";
    let refusal_request_id = "safe-refusal-request-83";
    let success_target = "1890123456789012380";
    let uncertain_target = "1890123456789012381";
    let refused_consent_target = "1890123456789012382";
    let refused_request_target = "1890123456789012383";
    let dry_run_target = "1890123456789012384";
    let direct_provider_target = "1890123456789012385";
    let provider_user_id: String =
        sqlx::query_scalar("select provider_user_id from x_archive.accounts where id = $1")
            .bind(account)
            .fetch_one(test.database.pool())
            .await
            .expect("the connected provider identity is readable");
    let cipher = TokenCipher::new(&[0x83; 32]).expect("the test cipher key is valid");
    let encrypted_marker_credential = cipher.seal(
        account,
        Purpose::Credential,
        &CredentialPayload {
            access_token: marker_access_token.to_owned(),
            refresh_token: marker_refresh_token.to_owned(),
        }
        .encode(),
    );
    sqlx::query(
        "update x_archive.credentials set encrypted_payload = $2 \
         where account_id = $1 and status = 'active'",
    )
    .bind(account)
    .bind(&encrypted_marker_credential)
    .execute(test.database.pool())
    .await
    .expect("the active credential is replaced by a synthetic encrypted marker envelope");
    let author: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.users (provider_id, parser_version) \
         values ('writeback-redaction-author', 1) returning id",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("the redaction target author seeds");
    sqlx::query(
        "insert into x_archive.posts (provider_id, author_user_id, text, parser_version) \
         values ($1, $2, $3, 1)",
    )
    .bind(success_target)
    .bind(author)
    .bind(marker_private_post_content)
    .execute(test.database.pool())
    .await
    .expect("the known target with private content seeds");

    let server = wiremock::MockServer::start().await;
    let provider_path = format!("/2/users/{provider_user_id}/bookmarks");
    for (target, response) in [
        (
            success_target,
            wiremock::ResponseTemplate::new(200)
                .insert_header("x-request-id", success_request_id)
                .set_body_json(serde_json::json!({
                    "data": { "bookmarked": true },
                    "raw_private_evidence": marker_raw_provider_body,
                    "private_post_echo": marker_private_post_content,
                })),
        ),
        (
            uncertain_target,
            wiremock::ResponseTemplate::new(503)
                .insert_header("x-request-id", uncertain_request_id)
                .set_body_json(serde_json::json!({
                    "raw_private_evidence": marker_raw_provider_body,
                    "authorization_echo": marker_authorization_header,
                })),
        ),
        (
            direct_provider_target,
            wiremock::ResponseTemplate::new(400)
                .insert_header("x-request-id", refusal_request_id)
                .set_body_json(serde_json::json!({
                    "raw_private_evidence": marker_raw_provider_body,
                    "private_post_echo": marker_private_post_content,
                })),
        ),
    ] {
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path(provider_path.clone()))
            .and(wiremock::matchers::header(
                "authorization",
                marker_authorization_header.clone(),
            ))
            .and(wiremock::matchers::body_json(serde_json::json!({
                "tweet_id": target
            })))
            .respond_with(response)
            .mount(&server)
            .await;
    }
    let provider = OfficialBookmarkProvider::new(test.database.clone(), cipher, server.uri());
    let surface = || {
        ConsentSurfaceId::parse("web.bookmark-menu")
            .expect("the trusted surface identifier is valid")
    };
    let success_consent = service
        .record_consent(
            owner,
            account,
            BookmarkAction::Add,
            success_target,
            instant,
            surface(),
        )
        .await
        .expect("the success consent records");
    let uncertain_consent = service
        .record_consent(
            owner,
            account,
            BookmarkAction::Add,
            uncertain_target,
            instant,
            surface(),
        )
        .await
        .expect("the uncertain consent records");
    let refusal_consent = service
        .record_consent(
            owner,
            account,
            BookmarkAction::Add,
            refused_consent_target,
            instant,
            surface(),
        )
        .await
        .expect("the refusal consent records");
    let dry_run_consent = service
        .record_consent(
            owner,
            account,
            BookmarkAction::Add,
            dry_run_target,
            instant,
            surface(),
        )
        .await
        .expect("the dry-run consent records");

    let success_result = service
        .add_bookmark(
            BookmarkWriteRequest {
                internal_user_id: owner,
                account_id: account,
                consent_id: success_consent.id,
                provider_post_id: success_target,
                idempotency_key: "audit-redaction-success",
            },
            &provider,
        )
        .await;
    let uncertain_result = service
        .add_bookmark(
            BookmarkWriteRequest {
                internal_user_id: owner,
                account_id: account,
                consent_id: uncertain_consent.id,
                provider_post_id: uncertain_target,
                idempotency_key: "audit-redaction-uncertain",
            },
            &provider,
        )
        .await;
    let refusal_result = service
        .add_bookmark(
            BookmarkWriteRequest {
                internal_user_id: owner,
                account_id: account,
                consent_id: refusal_consent.id,
                provider_post_id: refused_request_target,
                idempotency_key: "audit-redaction-refusal",
            },
            &provider,
        )
        .await;
    let dry_run_result = service
        .dry_run(
            BookmarkWriteRequest {
                internal_user_id: owner,
                account_id: account,
                consent_id: dry_run_consent.id,
                provider_post_id: dry_run_target,
                idempotency_key: "audit-redaction-dry-run",
            },
            BookmarkAction::Add,
        )
        .await;
    let direct_provider_result = provider.add_bookmark(account, direct_provider_target).await;
    let requests = server
        .received_requests()
        .await
        .expect("request recording remains enabled");
    let stored_rows: Vec<String> = sqlx::query_scalar(
        "select evidence.payload from ( \
           select to_jsonb(write_operation)::text as payload \
             from x_archive.bookmark_write_operations write_operation \
            where write_operation.account_id = $1 \
           union all \
           select to_jsonb(audit_event)::text as payload \
             from x_archive.bookmark_write_audit_events audit_event \
            where audit_event.account_id = $1 \
           union all \
           select to_jsonb(write_consent)::text as payload \
             from x_archive.bookmark_write_consents write_consent \
            where write_consent.account_id = $1 \
         ) evidence",
    )
    .bind(account)
    .fetch_all(test.database.pool())
    .await
    .expect("all persisted operation, audit, and consent fields are readable as text");
    let stored_diagnostics = stored_rows.join("\n");
    let refusal_display = refusal_result
        .as_ref()
        .err()
        .map(ToString::to_string)
        .unwrap_or_default();
    let provider_display = direct_provider_result
        .as_ref()
        .err()
        .map(ToString::to_string)
        .unwrap_or_default();
    let runtime_diagnostics = format!(
        "{service:?}\n{provider:?}\n{success_consent:?}\n{uncertain_consent:?}\n\
         {refusal_consent:?}\n{dry_run_consent:?}\n{success_result:?}\n{uncertain_result:?}\n\
         {refusal_result:?}\n{refusal_display}\n{dry_run_result:?}\n\
         {direct_provider_result:?}\n{provider_display}"
    );

    test.cleanup().await.expect("cleanup drops the database");

    let mut violations = Vec::new();
    if !matches!(
        &success_result,
        Ok(result) if result.status == BookmarkWriteStatus::Succeeded
    ) {
        violations.push(format!(
            "the marker-bearing success response did not complete: {success_result:?}"
        ));
    }
    if !matches!(
        &uncertain_result,
        Ok(result) if result.status == BookmarkWriteStatus::Uncertain
    ) {
        violations.push(format!(
            "the marker-bearing uncertain response was not classified: {uncertain_result:?}"
        ));
    }
    if !matches!(
        &refusal_result,
        Err(BookmarkWritebackError::ConsentRequired)
    ) {
        violations.push(format!(
            "the local refusal path did not execute: {refusal_result:?}"
        ));
    }
    if !matches!(
        &dry_run_result,
        Ok(result)
            if result.outcome == BookmarkDryRunOutcome::WouldSubmit(BookmarkAction::Add)
    ) {
        violations.push(format!(
            "the dry-run path did not execute: {dry_run_result:?}"
        ));
    }
    if !matches!(
        &direct_provider_result,
        Err(BookmarkProviderError::DefiniteRefusal {
            status: 400,
            evidence,
        }) if evidence.request_id.as_deref() == Some(refusal_request_id)
    ) {
        violations.push(format!(
            "the raw provider refusal was not reduced to bounded evidence: {direct_provider_result:?}"
        ));
    }
    if requests.len() != 3 {
        violations.push(format!(
            "the provider received {} requests instead of the three representative HTTP cases",
            requests.len()
        ));
    } else if requests.iter().any(|request| {
        request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            != Some(marker_authorization_header.as_str())
    }) {
        violations
            .push("the marker Authorization header did not cross the provider boundary".to_owned());
    }
    for expected_bounded_evidence in [
        success_request_id,
        uncertain_request_id,
        "provider_classified",
        "outcome_uncertain",
        "gate_refused",
        "dry_run_completed",
    ] {
        if !stored_diagnostics.contains(expected_bounded_evidence) {
            violations.push(format!(
                "persisted evidence omitted bounded class/request identity {expected_bounded_evidence:?}"
            ));
        }
    }
    if !runtime_diagnostics.contains(refusal_request_id)
        || !runtime_diagnostics.contains("DefiniteRefusal")
        || !runtime_diagnostics.contains("ConsentRequired")
    {
        violations.push(format!(
            "typed runtime diagnostics omitted safe class/request evidence: {runtime_diagnostics}"
        ));
    }
    for forbidden in [
        marker_access_token,
        marker_refresh_token,
        marker_authorization_header.as_str(),
        marker_private_post_content,
        marker_raw_provider_body,
    ] {
        if stored_diagnostics.contains(forbidden) {
            violations.push(format!(
                "operation/audit/consent persistence leaked forbidden marker {forbidden:?}"
            ));
        }
        if runtime_diagnostics.contains(forbidden) {
            violations.push(format!(
                "Debug/error/result diagnostics leaked forbidden marker {forbidden:?}"
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "bookmark write audit redaction violations: {violations:?}"
    );
}
