//! Bookmark write-back dry run scenarios.

use super::*;

#[tokio::test]
async fn dry_run_fidelity_matches_live_admission_without_side_effects() {
    let (test, account, owner, service, instant) = authorized_writeback_fixture().await;
    let provider = FakeBookmarkProvider::default();
    let reset_at = instant + chrono::TimeDelta::hours(1);
    let surface = || {
        ConsentSurfaceId::parse("web.bookmark-menu")
            .expect("the trusted surface identifier is valid")
    };
    let mut effect_checks = Vec::new();

    let eligible_target = "1890123456789012360";
    let eligible_consent = service
        .record_consent(
            owner,
            account,
            BookmarkAction::Add,
            eligible_target,
            instant,
            surface(),
        )
        .await
        .expect("the eligible consent records");
    let eligible_request = BookmarkWriteRequest {
        internal_user_id: owner,
        account_id: account,
        consent_id: eligible_consent.id,
        provider_post_id: eligible_target,
        idempotency_key: "dry-run-eligible",
    };
    let (eligible, before, after) = observe_dry_run(
        &test,
        account,
        instant,
        &service,
        eligible_request,
        BookmarkAction::Add,
    )
    .await;
    effect_checks.push(("eligible", before, after));

    let already_target = "1890123456789012361";
    let observed_at = instant - chrono::TimeDelta::seconds(45);
    let author: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.users (provider_id, parser_version) \
         values ('dry-run-author', 1) returning id",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("the already-satisfied author seeds");
    let post: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.posts (provider_id, author_user_id, parser_version) \
         values ($1, $2, 1) returning id",
    )
    .bind(already_target)
    .bind(author)
    .fetch_one(test.database.pool())
    .await
    .expect("the already-satisfied post seeds");
    sqlx::query(
        "insert into x_archive.bookmarks \
         (account_id, post_id, first_observed_saved_at, last_observed_saved_at) \
         values ($1, $2, $3, $3)",
    )
    .bind(account)
    .bind(post)
    .bind(observed_at)
    .execute(test.database.pool())
    .await
    .expect("the durable bookmark observation seeds");
    let already_consent = service
        .record_consent(
            owner,
            account,
            BookmarkAction::Add,
            already_target,
            instant,
            surface(),
        )
        .await
        .expect("the already-satisfied consent records");
    let (already_satisfied, before, after) = observe_dry_run(
        &test,
        account,
        instant,
        &service,
        BookmarkWriteRequest {
            internal_user_id: owner,
            account_id: account,
            consent_id: already_consent.id,
            provider_post_id: already_target,
            idempotency_key: "dry-run-already-satisfied",
        },
        BookmarkAction::Add,
    )
    .await;
    effect_checks.push(("already-satisfied", before, after));

    let missing_consent_request = BookmarkWriteRequest {
        internal_user_id: owner,
        account_id: account,
        consent_id: eligible_consent.id,
        provider_post_id: "1890123456789012362",
        idempotency_key: "dry-run-missing-consent",
    };
    let (missing_consent, before, after) = observe_dry_run(
        &test,
        account,
        instant,
        &service,
        missing_consent_request,
        BookmarkAction::Add,
    )
    .await;
    effect_checks.push(("missing-consent", before, after));
    let live_missing_consent = service
        .add_bookmark(
            BookmarkWriteRequest {
                idempotency_key: "live-missing-consent",
                ..missing_consent_request
            },
            &provider,
        )
        .await;

    let scope_target = "1890123456789012363";
    let scope_consent = service
        .record_consent(
            owner,
            account,
            BookmarkAction::Add,
            scope_target,
            instant,
            surface(),
        )
        .await
        .expect("the missing-scope consent records");
    let read_scopes = vec![
        "users.read".to_owned(),
        "tweet.read".to_owned(),
        "bookmark.read".to_owned(),
        "offline.access".to_owned(),
    ];
    sqlx::query(
        "update x_archive.bookmark_write_authorizations \
         set granted_scopes = $2 where account_id = $1",
    )
    .bind(account)
    .bind(&read_scopes)
    .execute(test.database.pool())
    .await
    .expect("the local write grant is downgraded");
    let scope_request = BookmarkWriteRequest {
        internal_user_id: owner,
        account_id: account,
        consent_id: scope_consent.id,
        provider_post_id: scope_target,
        idempotency_key: "dry-run-missing-scope",
    };
    let (missing_scope, before, after) = observe_dry_run(
        &test,
        account,
        instant,
        &service,
        scope_request,
        BookmarkAction::Add,
    )
    .await;
    effect_checks.push(("missing-scope", before, after));
    let live_missing_scope = service
        .add_bookmark(
            BookmarkWriteRequest {
                idempotency_key: "live-missing-scope",
                ..scope_request
            },
            &provider,
        )
        .await;
    let complete_scopes = vec![
        "users.read".to_owned(),
        "tweet.read".to_owned(),
        "bookmark.read".to_owned(),
        "offline.access".to_owned(),
        "bookmark.write".to_owned(),
    ];
    sqlx::query(
        "update x_archive.bookmark_write_authorizations \
         set granted_scopes = $2 where account_id = $1",
    )
    .bind(account)
    .bind(&complete_scopes)
    .execute(test.database.pool())
    .await
    .expect("the complete local write grant is restored");

    let authorization_target = "1890123456789012364";
    let authorization_consent = service
        .record_consent(
            owner,
            account,
            BookmarkAction::Add,
            authorization_target,
            instant,
            surface(),
        )
        .await
        .expect("the missing-authorization consent records");
    sqlx::query(
        "update x_archive.bookmark_write_authorizations \
         set status = 'revoked', revoked_at = $2 where account_id = $1",
    )
    .bind(account)
    .bind(instant)
    .execute(test.database.pool())
    .await
    .expect("the local write authorization is revoked");
    let authorization_request = BookmarkWriteRequest {
        internal_user_id: owner,
        account_id: account,
        consent_id: authorization_consent.id,
        provider_post_id: authorization_target,
        idempotency_key: "dry-run-missing-authorization",
    };
    let (missing_authorization, before, after) = observe_dry_run(
        &test,
        account,
        instant,
        &service,
        authorization_request,
        BookmarkAction::Add,
    )
    .await;
    effect_checks.push(("missing-authorization", before, after));
    let live_missing_authorization = service
        .add_bookmark(
            BookmarkWriteRequest {
                idempotency_key: "live-missing-authorization",
                ..authorization_request
            },
            &provider,
        )
        .await;
    sqlx::query(
        "update x_archive.bookmark_write_authorizations \
         set status = 'active', revoked_at = null where account_id = $1",
    )
    .bind(account)
    .execute(test.database.pool())
    .await
    .expect("the local write authorization is restored");

    let foreign_target = "1890123456789012365";
    let foreign_consent = service
        .record_consent(
            owner,
            account,
            BookmarkAction::Add,
            foreign_target,
            instant,
            surface(),
        )
        .await
        .expect("the foreign-owner consent fixture records");
    let foreign_request = BookmarkWriteRequest {
        internal_user_id: uuid::Uuid::from_u128(0xfeed),
        account_id: account,
        consent_id: foreign_consent.id,
        provider_post_id: foreign_target,
        idempotency_key: "dry-run-foreign-owner",
    };
    let (foreign_owner, before, after) = observe_dry_run(
        &test,
        account,
        instant,
        &service,
        foreign_request,
        BookmarkAction::Add,
    )
    .await;
    effect_checks.push(("foreign-owner", before, after));
    let live_foreign_owner = service
        .add_bookmark(
            BookmarkWriteRequest {
                idempotency_key: "live-foreign-owner",
                ..foreign_request
            },
            &provider,
        )
        .await;

    let exhausted_target = "1890123456789012366";
    let exhausted_consent = service
        .record_consent(
            owner,
            account,
            BookmarkAction::Add,
            exhausted_target,
            instant,
            surface(),
        )
        .await
        .expect("the exhausted-budget consent records");
    let budget_clock: Arc<dyn Clock> = Arc::new(FixedClock(instant));
    let budget = BudgetGate::with_clock(
        test.database.clone(),
        BudgetClass::BookmarkWrite,
        10,
        3_600,
        budget_clock,
    )
    .expect("the exhaustion fixture budget constructs");
    budget
        .reserve(account, 10)
        .await
        .expect("the write allowance is exhausted by the fixture");
    let exhausted_request = BookmarkWriteRequest {
        internal_user_id: owner,
        account_id: account,
        consent_id: exhausted_consent.id,
        provider_post_id: exhausted_target,
        idempotency_key: "dry-run-exhausted-budget",
    };
    let (exhausted_budget, before, after) = observe_dry_run(
        &test,
        account,
        instant,
        &service,
        exhausted_request,
        BookmarkAction::Add,
    )
    .await;
    effect_checks.push(("exhausted-budget", before, after));
    let live_exhausted_budget = service
        .add_bookmark(
            BookmarkWriteRequest {
                idempotency_key: "live-exhausted-budget",
                ..exhausted_request
            },
            &provider,
        )
        .await;

    let provider_calls = provider.calls.load(Ordering::Relaxed);
    test.cleanup().await.expect("cleanup drops the database");

    let mut violations = Vec::new();
    let expected_eligible = BookmarkDryRunResult {
        outcome: BookmarkDryRunOutcome::WouldSubmit(BookmarkAction::Add),
        evaluated_at: instant,
        observed_at: None,
        budget_reset_at: Some(reset_at),
        advisory: true,
    };
    if !matches!(&eligible, Ok(result) if *result == expected_eligible) {
        violations.push(format!(
            "eligible preview was not faithful: expected={expected_eligible:?}, actual={eligible:?}"
        ));
    }
    let expected_already = BookmarkDryRunResult {
        outcome: BookmarkDryRunOutcome::WouldAlreadySatisfy(BookmarkAction::Add),
        evaluated_at: instant,
        observed_at: Some(observed_at),
        budget_reset_at: None,
        advisory: true,
    };
    if !matches!(&already_satisfied, Ok(result) if *result == expected_already) {
        violations.push(format!(
            "durable bookmark was not recognized: expected={expected_already:?}, actual={already_satisfied:?}"
        ));
    }
    let refusal_cases = [
        (
            "missing-consent",
            &missing_consent,
            &live_missing_consent,
            BookmarkAdmissionRefusal::ConsentRequired,
            None,
        ),
        (
            "missing-scope",
            &missing_scope,
            &live_missing_scope,
            BookmarkAdmissionRefusal::WriteScopeRequired,
            None,
        ),
        (
            "missing-authorization",
            &missing_authorization,
            &live_missing_authorization,
            BookmarkAdmissionRefusal::WriteAuthorizationRequired,
            None,
        ),
        (
            "foreign-owner",
            &foreign_owner,
            &live_foreign_owner,
            BookmarkAdmissionRefusal::OwnershipMismatch,
            None,
        ),
        (
            "exhausted-budget",
            &exhausted_budget,
            &live_exhausted_budget,
            BookmarkAdmissionRefusal::BudgetExhausted,
            Some(reset_at),
        ),
    ];
    for (name, dry_run, live, expected_refusal, expected_reset) in refusal_cases {
        let expected = BookmarkDryRunResult {
            outcome: BookmarkDryRunOutcome::WouldRefuse(expected_refusal),
            evaluated_at: instant,
            observed_at: None,
            budget_reset_at: expected_reset,
            advisory: true,
        };
        if !matches!(dry_run, Ok(result) if *result == expected) {
            violations.push(format!(
                "{name} preview did not return the typed refusal: expected={expected:?}, actual={dry_run:?}"
            ));
        }
        let expected_live = Some((expected_refusal, expected_reset));
        let actual_live = live_refusal(live);
        if actual_live != expected_live {
            violations.push(format!(
                "{name} live admission did not match its refusal class: expected={expected_live:?}, actual={actual_live:?}, result={live:?}"
            ));
        }
    }
    for (name, before, after) in effect_checks {
        if before != after {
            violations.push(format!(
                "{name} dry run changed consent, budget, or projection: before={before:?}, after={after:?}"
            ));
        }
        if after.consumed_consents != 0 {
            violations.push(format!(
                "{name} dry run left {} consumed consent(s)",
                after.consumed_consents
            ));
        }
    }
    if provider_calls != 0 {
        violations.push(format!(
            "dry runs or corresponding live refusals called the provider {provider_calls} times"
        ));
    }
    assert!(
        violations.is_empty(),
        "dry-run fidelity violations: {violations:?}"
    );
}
