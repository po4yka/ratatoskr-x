//! Bookmark write-back budget projection scenarios.

use super::*;

#[tokio::test]
async fn write_budget_refusal_leaves_consent_and_read_allowance_untouched() {
    let (test, account, owner, service, instant) = authorized_writeback_fixture().await;
    let provider = FakeBookmarkProvider::default();
    let target = "1890123456789012370";
    let reset_at = instant + chrono::TimeDelta::hours(1);
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
        .expect("the exact live consent records");
    let clock: Arc<dyn Clock> = Arc::new(FixedClock(instant));
    let write_budget = BudgetGate::with_clock(
        test.database.clone(),
        BudgetClass::BookmarkWrite,
        10,
        3_600,
        Arc::clone(&clock),
    )
    .expect("the hard bookmark-write budget constructs");
    write_budget
        .reserve(account, 10)
        .await
        .expect("the fixture exhausts the write allowance");
    let read_budget =
        BudgetGate::with_clock(test.database.clone(), BudgetClass::Read, 10, 3_600, clock)
            .expect("the isolated read budget constructs");
    read_budget
        .reserve(account, 2)
        .await
        .expect("the fixture retains read allowance after prior reads");
    let write_usage_before = x_persistence::budget_windows::window_usage(
        test.database.pool(),
        account,
        BudgetClass::BookmarkWrite.as_str(),
        instant,
    )
    .await
    .expect("the capped write usage is readable");
    let read_usage_before = x_persistence::budget_windows::window_usage(
        test.database.pool(),
        account,
        BudgetClass::Read.as_str(),
        instant,
    )
    .await
    .expect("the read usage is readable");
    let read_inspection_before = read_budget
        .inspect(account, 1)
        .await
        .expect("the retained read allowance is inspectable");

    let outcome = service
        .add_bookmark(
            BookmarkWriteRequest {
                internal_user_id: owner,
                account_id: account,
                consent_id: consent.id,
                provider_post_id: target,
                idempotency_key: "write-budget-refusal-at-cap",
            },
            &provider,
        )
        .await;

    let consumed_at: Option<DateTime<Utc>> = sqlx::query_scalar(
        "select consumed_at from x_archive.bookmark_write_consents where id = $1",
    )
    .bind(consent.id)
    .fetch_one(test.database.pool())
    .await
    .expect("the consent consumption evidence is readable");
    let write_usage_after = x_persistence::budget_windows::window_usage(
        test.database.pool(),
        account,
        BudgetClass::BookmarkWrite.as_str(),
        instant,
    )
    .await
    .expect("the refused write usage is readable");
    let read_usage_after = x_persistence::budget_windows::window_usage(
        test.database.pool(),
        account,
        BudgetClass::Read.as_str(),
        instant,
    )
    .await
    .expect("the post-refusal read usage is readable");
    let read_inspection_after = read_budget
        .inspect(account, 1)
        .await
        .expect("the post-refusal read allowance is inspectable");
    let operation: Option<(uuid::Uuid, String, Option<String>, Option<DateTime<Utc>>)> =
        sqlx::query_as(
            "select id, status, outcome, rate_limit_reset_at \
         from x_archive.bookmark_write_operations \
         where account_id = $1 and consent_id = $2 and provider_post_id = $3",
        )
        .bind(account)
        .bind(consent.id)
        .bind(target)
        .fetch_optional(test.database.pool())
        .await
        .expect("the refused operation evidence is readable");
    let audit_events: Vec<(String, serde_json::Value)> =
        if let Some((operation_id, ..)) = operation.as_ref() {
            sqlx::query_as(
                "select event_class, details from x_archive.bookmark_write_audit_events \
             where operation_id = $1 order by occurred_at, id",
            )
            .bind(*operation_id)
            .fetch_all(test.database.pool())
            .await
            .expect("the budget-refusal audit evidence is readable")
        } else {
            Vec::new()
        };
    let provider_calls = provider.calls.load(Ordering::Relaxed);

    test.cleanup().await.expect("cleanup drops the database");

    let mut violations = Vec::new();
    if !matches!(
        outcome,
        Err(BookmarkWritebackError::Budget(BudgetError::Exhausted { reset_at: actual }))
            if actual == reset_at
    ) {
        violations.push(format!(
            "write exhaustion did not return its typed reset evidence: {outcome:?}"
        ));
    }
    if provider_calls != 0 {
        violations.push(format!(
            "write exhaustion called the provider {provider_calls} time(s)"
        ));
    }
    if consumed_at.is_some() {
        violations.push(format!(
            "write exhaustion consumed reusable consent at {consumed_at:?}"
        ));
    }
    if write_usage_before != Some(10) || write_usage_after != write_usage_before {
        violations.push(format!(
            "write usage moved away from the hard cap: before={write_usage_before:?}, after={write_usage_after:?}"
        ));
    }
    if read_usage_before != Some(2) || read_usage_after != read_usage_before {
        violations.push(format!(
            "write refusal changed isolated read usage: before={read_usage_before:?}, after={read_usage_after:?}"
        ));
    }
    if !read_inspection_before.eligible
        || !read_inspection_after.eligible
        || read_inspection_after != read_inspection_before
    {
        violations.push(format!(
            "write refusal borrowed or changed read allowance: before={read_inspection_before:?}, after={read_inspection_after:?}"
        ));
    }
    match &operation {
        Some((_, status, operation_outcome, operation_reset))
            if status == "refused"
                && operation_outcome.as_deref() == Some("budget_exhausted")
                && *operation_reset == Some(reset_at) => {}
        _ => violations.push(format!(
            "budget refusal was not stored as a terminal reset-bearing operation: {operation:?}"
        )),
    }
    let budget_audit = audit_events
        .iter()
        .find(|(event_class, _)| event_class == "budget_refused");
    match budget_audit {
        Some((_, details)) => {
            let carries_reset = details
                .as_object()
                .into_iter()
                .flat_map(|object| object.values())
                .filter_map(serde_json::Value::as_str)
                .filter_map(|value| value.parse::<DateTime<Utc>>().ok())
                .any(|value| value == reset_at);
            if !carries_reset {
                violations.push(format!(
                    "budget-refused audit omitted reset evidence: {details}"
                ));
            }
        }
        None => violations.push(format!(
            "budget refusal audit event is absent: {audit_events:?}"
        )),
    }
    assert!(
        violations.is_empty(),
        "isolated write-budget refusal violations: {violations:?}"
    );
}

#[tokio::test]
async fn confirmed_add_remove_update_known_projection_with_write_evidence() {
    let (test, account, owner, service, instant) = authorized_writeback_fixture().await;
    let provider = EvidencedBookmarkProvider::default();
    let add_target = "1890123456789012371";
    let remove_target = "1890123456789012372";
    let unknown_target = "1890123456789012373";
    let historical_saved_at = instant - chrono::TimeDelta::hours(2);
    let author: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.users (provider_id, parser_version) \
         values ('writeback-projection-author', 1) returning id",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("the normalized projection author seeds");
    let add_post: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.posts (provider_id, author_user_id, parser_version) \
         values ($1, $2, 1) returning id",
    )
    .bind(add_target)
    .bind(author)
    .fetch_one(test.database.pool())
    .await
    .expect("the known add target seeds");
    let remove_post: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.posts (provider_id, author_user_id, parser_version) \
         values ($1, $2, 1) returning id",
    )
    .bind(remove_target)
    .bind(author)
    .fetch_one(test.database.pool())
    .await
    .expect("the known remove target seeds");
    sqlx::query(
        "insert into x_archive.bookmarks \
         (account_id, post_id, first_observed_saved_at, last_observed_saved_at) \
         values ($1, $2, $3, $3)",
    )
    .bind(account)
    .bind(remove_post)
    .bind(historical_saved_at)
    .execute(test.database.pool())
    .await
    .expect("the active remove projection seeds");
    let surface = || {
        ConsentSurfaceId::parse("web.bookmark-menu")
            .expect("the trusted surface identifier is valid")
    };
    let add_consent = service
        .record_consent(
            owner,
            account,
            BookmarkAction::Add,
            add_target,
            instant,
            surface(),
        )
        .await
        .expect("the known add consent records");
    let remove_consent = service
        .record_consent(
            owner,
            account,
            BookmarkAction::Remove,
            remove_target,
            instant,
            surface(),
        )
        .await
        .expect("the known remove consent records");
    let unknown_consent = service
        .record_consent(
            owner,
            account,
            BookmarkAction::Add,
            unknown_target,
            instant,
            surface(),
        )
        .await
        .expect("the unknown add consent records");

    let add_result = service
        .add_bookmark(
            BookmarkWriteRequest {
                internal_user_id: owner,
                account_id: account,
                consent_id: add_consent.id,
                provider_post_id: add_target,
                idempotency_key: "confirmed-known-add",
            },
            &provider,
        )
        .await;
    let remove_result = service
        .remove_bookmark(
            BookmarkWriteRequest {
                internal_user_id: owner,
                account_id: account,
                consent_id: remove_consent.id,
                provider_post_id: remove_target,
                idempotency_key: "confirmed-known-remove",
            },
            &provider,
        )
        .await;
    let unknown_result = service
        .add_bookmark(
            BookmarkWriteRequest {
                internal_user_id: owner,
                account_id: account,
                consent_id: unknown_consent.id,
                provider_post_id: unknown_target,
                idempotency_key: "confirmed-unknown-add",
            },
            &provider,
        )
        .await;

    type ProjectionEvidence = (
        DateTime<Utc>,
        DateTime<Utc>,
        Option<DateTime<Utc>>,
        Option<uuid::Uuid>,
        Option<DateTime<Utc>>,
        Option<uuid::Uuid>,
    );
    let add_projection: Option<ProjectionEvidence> = sqlx::query_as(
        "select first_observed_saved_at, last_observed_saved_at, observed_removed_at, \
                last_write_operation_id, last_write_observed_at, \
                observed_removed_write_operation_id \
         from x_archive.bookmarks where account_id = $1 and post_id = $2",
    )
    .bind(account)
    .bind(add_post)
    .fetch_optional(test.database.pool())
    .await
    .expect("the confirmed add projection is readable");
    let remove_projection: Option<ProjectionEvidence> = sqlx::query_as(
        "select first_observed_saved_at, last_observed_saved_at, observed_removed_at, \
                last_write_operation_id, last_write_observed_at, \
                observed_removed_write_operation_id \
         from x_archive.bookmarks where account_id = $1 and post_id = $2",
    )
    .bind(account)
    .bind(remove_post)
    .fetch_optional(test.database.pool())
    .await
    .expect("the confirmed remove projection is readable");
    type OperationEvidence = (
        uuid::Uuid,
        String,
        Option<String>,
        Option<String>,
        Option<DateTime<Utc>>,
    );
    let add_operation: Option<OperationEvidence> = sqlx::query_as(
        "select id, status, outcome, provider_request_id, projection_observed_at \
         from x_archive.bookmark_write_operations \
         where account_id = $1 and consent_id = $2",
    )
    .bind(account)
    .bind(add_consent.id)
    .fetch_optional(test.database.pool())
    .await
    .expect("the known add operation evidence is readable");
    let remove_operation: Option<OperationEvidence> = sqlx::query_as(
        "select id, status, outcome, provider_request_id, projection_observed_at \
         from x_archive.bookmark_write_operations \
         where account_id = $1 and consent_id = $2",
    )
    .bind(account)
    .bind(remove_consent.id)
    .fetch_optional(test.database.pool())
    .await
    .expect("the known remove operation evidence is readable");
    let unknown_operation: Option<OperationEvidence> = sqlx::query_as(
        "select id, status, outcome, provider_request_id, projection_observed_at \
         from x_archive.bookmark_write_operations \
         where account_id = $1 and consent_id = $2",
    )
    .bind(account)
    .bind(unknown_consent.id)
    .fetch_optional(test.database.pool())
    .await
    .expect("the unknown add operation evidence is readable");
    let unknown_posts: i64 =
        sqlx::query_scalar("select count(*) from x_archive.posts where provider_id = $1")
            .bind(unknown_target)
            .fetch_one(test.database.pool())
            .await
            .expect("the unknown target post count is readable");
    let unknown_bookmarks: i64 = sqlx::query_scalar(
        "select count(*) from x_archive.bookmarks bookmark \
         join x_archive.posts post on post.id = bookmark.post_id \
         where bookmark.account_id = $1 and post.provider_id = $2",
    )
    .bind(account)
    .bind(unknown_target)
    .fetch_one(test.database.pool())
    .await
    .expect("the unknown target bookmark count is readable");
    let provider_calls = provider.calls.load(Ordering::Relaxed);

    test.cleanup().await.expect("cleanup drops the database");

    let mut violations = Vec::new();
    if !matches!(
        &add_result,
        Ok(result) if result.status == BookmarkWriteStatus::Succeeded
    ) {
        violations.push(format!("known add did not confirm: {add_result:?}"));
    }
    if !matches!(
        &remove_result,
        Ok(result) if result.status == BookmarkWriteStatus::Succeeded
    ) {
        violations.push(format!("known remove did not confirm: {remove_result:?}"));
    }
    if unknown_result.is_err() {
        violations.push(format!(
            "confirmed unknown add was not retained: {unknown_result:?}"
        ));
    }
    let add_operation_id = add_result.as_ref().ok().map(|result| result.operation_id);
    let remove_operation_id = remove_result
        .as_ref()
        .ok()
        .map(|result| result.operation_id);
    let unknown_operation_id = unknown_result
        .as_ref()
        .ok()
        .map(|result| result.operation_id);
    match &add_projection {
        Some((first, last, None, write_operation, write_observed_at, None))
            if *first == instant
                && *last == instant
                && *write_operation == add_operation_id
                && *write_observed_at == Some(instant) => {}
        _ => violations.push(format!(
            "confirmed add did not create honest write-observation evidence: {add_projection:?}"
        )),
    }
    match &remove_projection {
        Some((first, last, Some(removed_at), None, None, removed_operation))
            if *first == historical_saved_at
                && *last == historical_saved_at
                && *removed_at == instant
                && *removed_operation == remove_operation_id => {}
        _ => violations.push(format!(
            "confirmed remove did not retain honest removal write evidence: {remove_projection:?}"
        )),
    }
    let expected_add_request_id = format!("request-add-{add_target}");
    match &add_operation {
        Some((operation_id, status, outcome, request_id, projection_at))
            if Some(*operation_id) == add_operation_id
                && status == "succeeded"
                && outcome.as_deref() == Some("confirmed")
                && request_id.as_deref() == Some(expected_add_request_id.as_str())
                && *projection_at == Some(instant) => {}
        _ => violations.push(format!(
            "known add operation and projection are inconsistent: {add_operation:?}"
        )),
    }
    let expected_remove_request_id = format!("request-remove-{remove_target}");
    match &remove_operation {
        Some((operation_id, status, outcome, request_id, projection_at))
            if Some(*operation_id) == remove_operation_id
                && status == "succeeded"
                && outcome.as_deref() == Some("confirmed")
                && request_id.as_deref() == Some(expected_remove_request_id.as_str())
                && *projection_at == Some(instant) => {}
        _ => violations.push(format!(
            "known remove operation and projection are inconsistent: {remove_operation:?}"
        )),
    }
    let expected_unknown_request_id = format!("request-add-{unknown_target}");
    match &unknown_operation {
        Some((operation_id, status, outcome, request_id, projection_at))
            if Some(*operation_id) == unknown_operation_id
                && status == "projection_pending"
                && outcome.as_deref() == Some("confirmed")
                && request_id.as_deref() == Some(expected_unknown_request_id.as_str())
                && projection_at.is_none() => {}
        _ => violations.push(format!(
            "unknown confirmed add was not retained as projection-pending: {unknown_operation:?}"
        )),
    }
    if unknown_posts != 0 || unknown_bookmarks != 0 {
        violations.push(format!(
            "unknown confirmed add fabricated placeholders: posts={unknown_posts}, bookmarks={unknown_bookmarks}"
        ));
    }
    if provider_calls != 3 {
        violations.push(format!(
            "projection scenarios called the provider {provider_calls} times"
        ));
    }
    assert!(
        violations.is_empty(),
        "confirmed bookmark projection violations: {violations:?}"
    );
}
