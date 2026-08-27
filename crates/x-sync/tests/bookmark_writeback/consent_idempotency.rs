//! Bookmark write-back consent idempotency scenarios.

use super::*;

#[tokio::test]
async fn consent_gate_blocks_unconsented_writes_before_budget_and_provider() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = test
        .seed_account("writeback-consent-gate-account")
        .await
        .expect("the connected account seeds");
    let owner: uuid::Uuid =
        sqlx::query_scalar("select internal_user_id from x_archive.accounts where id = $1")
            .bind(account)
            .fetch_one(test.database.pool())
            .await
            .expect("the account owner is readable");
    let scopes = vec![
        "users.read".to_owned(),
        "tweet.read".to_owned(),
        "bookmark.read".to_owned(),
        "offline.access".to_owned(),
        "bookmark.write".to_owned(),
    ];
    let intent_id = x_persistence::oauth_intents::insert_intent(
        &test.database,
        &x_persistence::oauth_intents::NewIntent {
            internal_user_id: owner,
            account_id: Some(account),
            purpose: "bookmark_write",
            state_hash: "f838e199c727e2cb37b94f11d83c850c7043f872b0b5e8d1b40c8be38e75a15a",
            code_verifier_encrypted: b"REDACTED-writeback-verifier-envelope",
            nonce: "writeback-consent-gate-nonce",
            redirect_uri: "https://app.example/callback",
            requested_scopes: &scopes,
            created_at: "2026-08-27T11:55:00Z"
                .parse()
                .expect("the creation instant parses"),
            expires_at: "2026-08-27T12:05:00Z"
                .parse()
                .expect("the expiry instant parses"),
        },
    )
    .await
    .expect("the bookmark-write intent seeds");
    x_persistence::credentials::insert_with_status(
        &test.database,
        &x_persistence::credentials::NewCredential {
            account_id: account,
            encrypted_payload: b"REDACTED-active-write-credential-envelope",
            granted_scopes: &scopes,
            expires_at: None,
        },
        "active",
    )
    .await
    .expect("the active write credential seeds");
    sqlx::query(
        "insert into x_archive.bookmark_write_authorizations \
         (account_id, oauth_intent_id, granted_scopes, status, authorized_at) \
         values ($1, $2, $3, 'active', $4)",
    )
    .bind(account)
    .bind(intent_id)
    .bind(&scopes)
    .bind(
        "2026-08-27T12:00:00Z"
            .parse::<DateTime<Utc>>()
            .expect("the authorization instant parses"),
    )
    .execute(test.database.pool())
    .await
    .expect("the local write authorization seeds");

    let instant: DateTime<Utc> = "2026-08-27T12:00:00Z"
        .parse()
        .expect("the budget instant parses");
    let clock: Arc<dyn Clock> = Arc::new(FixedClock(instant));
    let budget = BudgetGate::with_clock(
        test.database.clone(),
        BudgetClass::BookmarkWrite,
        10,
        3_600,
        Arc::clone(&clock),
    )
    .expect("the isolated bookmark-write budget constructs");
    let before = x_persistence::budget_windows::window_usage(
        test.database.pool(),
        account,
        BudgetClass::BookmarkWrite.as_str(),
        instant,
    )
    .await
    .expect("the pre-request write usage is readable");
    let service = BookmarkWritebackService::new(test.database.clone(), budget, clock, 300)
        .expect("the consent lifetime is valid");
    let provider = FakeBookmarkProvider::default();

    let outcome = service
        .add_bookmark(
            BookmarkWriteRequest {
                internal_user_id: owner,
                account_id: account,
                consent_id: uuid::Uuid::nil(),
                provider_post_id: "1890123456789012345",
                idempotency_key: "unconsented-add-attempt",
            },
            &provider,
        )
        .await;
    let provider_calls = provider.calls.load(Ordering::Relaxed);
    let after = x_persistence::budget_windows::window_usage(
        test.database.pool(),
        account,
        BudgetClass::BookmarkWrite.as_str(),
        instant,
    )
    .await
    .expect("the post-request write usage is readable");

    test.cleanup().await.expect("cleanup drops the database");

    let mut violations = Vec::new();
    if before.is_some() {
        violations.push(format!(
            "write usage existed before the request: {before:?}"
        ));
    }
    if !matches!(outcome, Err(BookmarkWritebackError::ConsentRequired)) {
        violations.push(format!("unconsented request was not refused: {outcome:?}"));
    }
    if provider_calls != 0 {
        violations.push(format!("provider was called {provider_calls} time(s)"));
    }
    if after.unwrap_or_default() != 0 {
        violations.push(format!("write budget usage changed to {after:?}"));
    }
    assert!(
        violations.is_empty(),
        "consent gate violations: {violations:?}"
    );
}

#[tokio::test]
async fn consent_is_bound_to_exact_action_and_consumed_once() {
    let (test, account, owner, service, instant) = authorized_writeback_fixture().await;
    let provider = FakeBookmarkProvider::default();
    let target = "1890123456789012345";
    let wrong_target = "1890123456789012346";
    let approved_at = instant;
    let surface = ConsentSurfaceId::parse("web.bookmark-menu")
        .expect("the trusted surface identifier is valid");
    let consent = service
        .record_consent(
            owner,
            account,
            BookmarkAction::Add,
            target,
            approved_at,
            surface,
        )
        .await
        .expect("the explicit add consent records");
    let retained: (
        uuid::Uuid,
        uuid::Uuid,
        String,
        String,
        DateTime<Utc>,
        DateTime<Utc>,
        String,
    ) = sqlx::query_as(
        "select internal_user_id, account_id, action, provider_post_id, \
                approved_at, expires_at, surface \
         from x_archive.bookmark_write_consents where id = $1",
    )
    .bind(consent.id)
    .fetch_one(test.database.pool())
    .await
    .expect("the durable consent evidence is readable");

    let wrong_target_outcome = service
        .add_bookmark(
            BookmarkWriteRequest {
                internal_user_id: owner,
                account_id: account,
                consent_id: consent.id,
                provider_post_id: wrong_target,
                idempotency_key: "wrong-target-attempt",
            },
            &provider,
        )
        .await;
    let wrong_action_outcome = service
        .remove_bookmark(
            BookmarkWriteRequest {
                internal_user_id: owner,
                account_id: account,
                consent_id: consent.id,
                provider_post_id: target,
                idempotency_key: "wrong-action-attempt",
            },
            &provider,
        )
        .await;
    let exact_request = BookmarkWriteRequest {
        internal_user_id: owner,
        account_id: account,
        consent_id: consent.id,
        provider_post_id: target,
        idempotency_key: "concurrent-exact-attempt",
    };
    let (first, second) = tokio::join!(
        service.add_bookmark(exact_request, &provider),
        service.add_bookmark(exact_request, &provider),
    );
    let consumed_at: Option<DateTime<Utc>> = sqlx::query_scalar(
        "select consumed_at from x_archive.bookmark_write_consents where id = $1",
    )
    .bind(consent.id)
    .fetch_one(test.database.pool())
    .await
    .expect("the consent consumption evidence is readable");
    let usage = x_persistence::budget_windows::window_usage(
        test.database.pool(),
        account,
        BudgetClass::BookmarkWrite.as_str(),
        instant,
    )
    .await
    .expect("the bookmark-write usage is readable");
    let provider_calls = provider.calls.load(Ordering::Relaxed);

    test.cleanup().await.expect("cleanup drops the database");

    let mut violations = Vec::new();
    let expected_expiry = approved_at + chrono::TimeDelta::minutes(5);
    if retained
        != (
            owner,
            account,
            "add".to_owned(),
            target.to_owned(),
            approved_at,
            expected_expiry,
            "web.bookmark-menu".to_owned(),
        )
    {
        violations.push(format!(
            "durable consent evidence is incomplete: {retained:?}"
        ));
    }
    if !matches!(
        &wrong_target_outcome,
        Err(BookmarkWritebackError::ConsentRequired)
    ) {
        violations.push(format!(
            "wrong target reused the add consent: {wrong_target_outcome:?}"
        ));
    }
    if !matches!(
        &wrong_action_outcome,
        Err(BookmarkWritebackError::ConsentRequired)
    ) {
        violations.push(format!(
            "wrong action reused the add consent: {wrong_action_outcome:?}"
        ));
    }
    let exact_successes = [&first, &second]
        .into_iter()
        .filter(|outcome| outcome.is_ok())
        .count();
    if exact_successes != 2 {
        violations.push(format!(
            "concurrent exact consumers succeeded {exact_successes} times: {first:?}, {second:?}"
        ));
    }
    if provider_calls != 1 {
        violations.push(format!("provider was called {provider_calls} times"));
    }
    if consumed_at.is_none() {
        violations.push("consent was never marked consumed".to_owned());
    }
    if usage != Some(1) {
        violations.push(format!(
            "write budget usage was {usage:?}, expected Some(1)"
        ));
    }
    assert!(
        violations.is_empty(),
        "consent binding/consumption violations: {violations:?}"
    );
}

#[tokio::test]
async fn idempotent_retry_returns_stored_result_without_second_provider_call() {
    let (test, account, owner, service, instant) = authorized_writeback_fixture().await;
    let provider = FakeBookmarkProvider::default();
    let target = "1890123456789012347";
    let consent = service
        .record_consent(
            owner,
            account,
            BookmarkAction::Add,
            target,
            instant,
            ConsentSurfaceId::parse("web.bookmark-menu")
                .expect("the trusted surface identifier is valid"),
        )
        .await
        .expect("the explicit add consent records");
    let request = BookmarkWriteRequest {
        internal_user_id: owner,
        account_id: account,
        consent_id: consent.id,
        provider_post_id: target,
        idempotency_key: "retry-stable-user-key",
    };

    let first = service.add_bookmark(request, &provider).await;
    let restarted_clock: Arc<dyn Clock> = Arc::new(FixedClock(instant));
    let restarted_budget = BudgetGate::with_clock(
        test.database.clone(),
        BudgetClass::BookmarkWrite,
        10,
        3_600,
        Arc::clone(&restarted_clock),
    )
    .expect("the restarted write budget constructs");
    let restarted = BookmarkWritebackService::new(
        test.database.clone(),
        restarted_budget,
        restarted_clock,
        300,
    )
    .expect("the restarted service constructs");
    let second = restarted.add_bookmark(request, &provider).await;

    let stored_operation_id: Option<uuid::Uuid> = sqlx::query_scalar(
        "select id from x_archive.bookmark_write_operations \
         where account_id = $1 and action = 'add' and provider_post_id = $2 \
         order by created_at, id limit 1",
    )
    .bind(account)
    .bind(target)
    .fetch_optional(test.database.pool())
    .await
    .expect("the durable operation identity is readable");
    let consumed_at: Option<DateTime<Utc>> = sqlx::query_scalar(
        "select consumed_at from x_archive.bookmark_write_consents where id = $1",
    )
    .bind(consent.id)
    .fetch_one(test.database.pool())
    .await
    .expect("the consent consumption evidence is readable");
    let usage = x_persistence::budget_windows::window_usage(
        test.database.pool(),
        account,
        BudgetClass::BookmarkWrite.as_str(),
        instant,
    )
    .await
    .expect("the bookmark-write usage is readable");
    let provider_calls = provider.calls.load(Ordering::Relaxed);

    test.cleanup().await.expect("cleanup drops the database");

    let mut violations = Vec::new();
    match (&first, &second) {
        (Ok(first_result), Ok(second_result)) if first_result == second_result => {}
        _ => violations.push(format!(
            "exact retry did not return the stored first result: first={first:?}, second={second:?}"
        )),
    }
    let first_operation_id = first.as_ref().ok().map(|result| result.operation_id);
    if stored_operation_id != first_operation_id {
        violations.push(format!(
            "returned operation identity was not durable: returned={first_operation_id:?}, stored={stored_operation_id:?}"
        ));
    }
    if provider_calls != 1 {
        violations.push(format!("provider was called {provider_calls} times"));
    }
    if usage != Some(1) {
        violations.push(format!(
            "write budget usage was {usage:?}, expected Some(1)"
        ));
    }
    if consumed_at != Some(instant) {
        violations.push(format!(
            "consent consumption evidence was {consumed_at:?}, expected {instant}"
        ));
    }
    assert!(
        violations.is_empty(),
        "idempotent retry violations: {violations:?}"
    );
}

#[tokio::test]
async fn idempotency_key_conflict_and_concurrent_duplicates_do_not_duplicate_side_effects() {
    let (test, account, owner, service, instant) = authorized_writeback_fixture().await;
    let provider = FakeBookmarkProvider::default();
    let original_target = "1890123456789012348";
    let original_consent = service
        .record_consent(
            owner,
            account,
            BookmarkAction::Add,
            original_target,
            instant,
            ConsentSurfaceId::parse("web.bookmark-menu")
                .expect("the trusted surface identifier is valid"),
        )
        .await
        .expect("the original explicit consent records");
    let original_request = BookmarkWriteRequest {
        internal_user_id: owner,
        account_id: account,
        consent_id: original_consent.id,
        provider_post_id: original_target,
        idempotency_key: "conflict-stable-user-key",
    };
    let original = service
        .add_bookmark(original_request, &provider)
        .await
        .expect("the original operation succeeds");
    let original_snapshot: (String, String, Option<String>) = sqlx::query_as(
        "select request_fingerprint, status, outcome \
         from x_archive.bookmark_write_operations where id = $1",
    )
    .bind(original.operation_id)
    .fetch_one(test.database.pool())
    .await
    .expect("the original operation evidence is readable");
    let usage_before_conflicts = x_persistence::budget_windows::window_usage(
        test.database.pool(),
        account,
        BudgetClass::BookmarkWrite.as_str(),
        instant,
    )
    .await
    .expect("the original write usage is readable");

    let alternate_consent = service
        .record_consent(
            owner,
            account,
            BookmarkAction::Add,
            original_target,
            instant,
            ConsentSurfaceId::parse("web.bookmark-menu")
                .expect("the trusted surface identifier is valid"),
        )
        .await
        .expect("the alternate explicit consent records");
    let changed_target = service
        .add_bookmark(
            BookmarkWriteRequest {
                provider_post_id: "1890123456789012349",
                ..original_request
            },
            &provider,
        )
        .await;
    let changed_consent = service
        .add_bookmark(
            BookmarkWriteRequest {
                consent_id: alternate_consent.id,
                ..original_request
            },
            &provider,
        )
        .await;
    let snapshot_after_conflicts: (String, String, Option<String>) = sqlx::query_as(
        "select request_fingerprint, status, outcome \
         from x_archive.bookmark_write_operations where id = $1",
    )
    .bind(original.operation_id)
    .fetch_one(test.database.pool())
    .await
    .expect("the operation evidence after conflicts is readable");
    let usage_after_conflicts = x_persistence::budget_windows::window_usage(
        test.database.pool(),
        account,
        BudgetClass::BookmarkWrite.as_str(),
        instant,
    )
    .await
    .expect("the usage after conflicts is readable");
    let calls_after_conflicts = provider.calls.load(Ordering::Relaxed);

    let concurrent_target = "1890123456789012350";
    let concurrent_consent = service
        .record_consent(
            owner,
            account,
            BookmarkAction::Add,
            concurrent_target,
            instant,
            ConsentSurfaceId::parse("web.bookmark-menu")
                .expect("the trusted surface identifier is valid"),
        )
        .await
        .expect("the concurrent explicit consent records");
    let concurrent_request = BookmarkWriteRequest {
        internal_user_id: owner,
        account_id: account,
        consent_id: concurrent_consent.id,
        provider_post_id: concurrent_target,
        idempotency_key: "concurrent-stable-user-key",
    };
    let gated_provider = Arc::new(GatedBookmarkProvider::default());
    let first_service = service.clone();
    let first_provider = Arc::clone(&gated_provider);
    let first_task = tokio::spawn(async move {
        first_service
            .add_bookmark(concurrent_request, first_provider.as_ref())
            .await
    });
    gated_provider.entered.notified().await;
    let second_service = service.clone();
    let second_provider = Arc::clone(&gated_provider);
    let mut second_task = tokio::spawn(async move {
        second_service
            .add_bookmark(concurrent_request, second_provider.as_ref())
            .await
    });
    let second =
        match tokio::time::timeout(std::time::Duration::from_millis(100), &mut second_task).await {
            Ok(joined) => {
                gated_provider.release.notify_one();
                joined.expect("the duplicate caller task joins")
            }
            Err(_) => {
                gated_provider.release.notify_one();
                second_task
                    .await
                    .expect("the waiting duplicate caller task joins")
            }
        };
    let first = first_task.await.expect("the first caller task joins");

    let concurrent_operation: (uuid::Uuid, String, Option<String>) = sqlx::query_as(
        "select id, status, outcome from x_archive.bookmark_write_operations \
         where account_id = $1 and action = 'add' and provider_post_id = $2",
    )
    .bind(account)
    .bind(concurrent_target)
    .fetch_one(test.database.pool())
    .await
    .expect("the concurrent operation evidence is readable");
    let concurrent_consumed_at: Option<DateTime<Utc>> = sqlx::query_scalar(
        "select consumed_at from x_archive.bookmark_write_consents where id = $1",
    )
    .bind(concurrent_consent.id)
    .fetch_one(test.database.pool())
    .await
    .expect("the concurrent consent evidence is readable");
    let usage_after_concurrent = x_persistence::budget_windows::window_usage(
        test.database.pool(),
        account,
        BudgetClass::BookmarkWrite.as_str(),
        instant,
    )
    .await
    .expect("the usage after concurrent callers is readable");
    let concurrent_provider_calls = gated_provider.calls.load(Ordering::Relaxed);

    test.cleanup().await.expect("cleanup drops the database");

    let mut violations = Vec::new();
    if !matches!(
        changed_target,
        Err(BookmarkWritebackError::IdempotencyConflict)
    ) {
        violations.push(format!(
            "changed target did not conflict with the bound key: {changed_target:?}"
        ));
    }
    if !matches!(
        changed_consent,
        Err(BookmarkWritebackError::IdempotencyConflict)
    ) {
        violations.push(format!(
            "changed consent did not conflict with the bound key: {changed_consent:?}"
        ));
    }
    if snapshot_after_conflicts != original_snapshot {
        violations.push(format!(
            "conflicts changed original operation evidence: before={original_snapshot:?}, after={snapshot_after_conflicts:?}"
        ));
    }
    if calls_after_conflicts != 1 {
        violations.push(format!(
            "conflicts changed provider calls to {calls_after_conflicts}"
        ));
    }
    if usage_before_conflicts != Some(1) || usage_after_conflicts != usage_before_conflicts {
        violations.push(format!(
            "conflicts changed write usage: before={usage_before_conflicts:?}, after={usage_after_conflicts:?}"
        ));
    }
    match (&first, &second) {
        (Ok(first_result), Ok(second_result))
            if first_result == second_result
                && first_result.operation_id == concurrent_operation.0 => {}
        _ => violations.push(format!(
            "concurrent exact callers did not converge: first={first:?}, second={second:?}, stored={concurrent_operation:?}"
        )),
    }
    if concurrent_operation.1 != "projection_pending"
        || concurrent_operation.2.as_deref() != Some("confirmed")
    {
        violations.push(format!(
            "concurrent operation did not retain one terminal result: {concurrent_operation:?}"
        ));
    }
    if concurrent_provider_calls != 1 {
        violations.push(format!(
            "concurrent duplicates called the provider {concurrent_provider_calls} times"
        ));
    }
    let concurrent_usage =
        usage_after_concurrent.unwrap_or_default() - usage_after_conflicts.unwrap_or_default();
    if concurrent_usage != 1 {
        violations.push(format!(
            "concurrent duplicates consumed {concurrent_usage} budget units"
        ));
    }
    if concurrent_consumed_at != Some(instant) {
        violations.push(format!(
            "concurrent consent consumption was {concurrent_consumed_at:?}, expected {instant}"
        ));
    }
    assert!(
        violations.is_empty(),
        "idempotency conflict/concurrency violations: {violations:?}"
    );
}
