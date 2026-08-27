//! Bookmark write-back uncertainty audit scenarios.

use super::*;

#[tokio::test]
async fn uncertain_result_is_not_retried_and_only_complete_snapshot_reconciles_it() {
    let (test, account, owner, service, instant) = authorized_writeback_fixture().await;
    let provider = UncertainBookmarkProvider::default();
    let target = "1890123456789012374";
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
        .expect("the uncertain add consent records");
    let request = BookmarkWriteRequest {
        internal_user_id: owner,
        account_id: account,
        consent_id: consent.id,
        provider_post_id: target,
        idempotency_key: "uncertain-add-stable-key",
    };

    let first = service.add_bookmark(request, &provider).await;
    type OperationEvidence = (uuid::Uuid, String, Option<String>, Option<String>);
    let operation_after_first: Option<OperationEvidence> = sqlx::query_as(
        "select id, status, outcome, provider_request_id \
         from x_archive.bookmark_write_operations \
         where account_id = $1 and consent_id = $2",
    )
    .bind(account)
    .bind(consent.id)
    .fetch_optional(test.database.pool())
    .await
    .expect("the uncertain operation is readable after the first attempt");

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
    .expect("the restarted write-back service constructs");
    let retry = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        restarted.add_bookmark(request, &provider),
    )
    .await;
    let operation_after_retry: Option<OperationEvidence> = sqlx::query_as(
        "select id, status, outcome, provider_request_id \
         from x_archive.bookmark_write_operations \
         where account_id = $1 and consent_id = $2",
    )
    .bind(account)
    .bind(consent.id)
    .fetch_optional(test.database.pool())
    .await
    .expect("the uncertain operation is readable after exact retry");
    let calls_after_retry = provider.calls.load(Ordering::Relaxed);

    let snapshot_clock: Arc<dyn Clock> = Arc::new(FixedClock(instant));
    let snapshot_budget = BudgetGate::with_clock(
        test.database.clone(),
        BudgetClass::Read,
        10,
        3_600,
        Arc::clone(&snapshot_clock),
    )
    .expect("the isolated snapshot read budget constructs");
    let snapshot_service =
        BookmarkSnapshotService::new(test.database.clone(), snapshot_budget, snapshot_clock);
    let partial_source = SequencedBookmarkPages::new(vec![
        Ok(snapshot_page(target, Some("uncertain-next-page"))),
        Err(BookmarkSourceError::Unavailable),
    ]);
    let partial_outcome = snapshot_service
        .run(account, &partial_source, None)
        .await
        .expect("the interrupted snapshot is a resumable outcome");
    let partial_run_id = match partial_outcome {
        SnapshotOutcome::Incomplete { run_id } => Some(run_id),
        SnapshotOutcome::Completed { .. } => None,
    };
    let operation_after_partial: Option<OperationEvidence> = sqlx::query_as(
        "select id, status, outcome, provider_request_id \
         from x_archive.bookmark_write_operations \
         where account_id = $1 and consent_id = $2",
    )
    .bind(account)
    .bind(consent.id)
    .fetch_optional(test.database.pool())
    .await
    .expect("the operation is readable after partial evidence");
    let partial_reconciliation_events: i64 = sqlx::query_scalar(
        "select count(*) from x_archive.bookmark_write_audit_events \
         where operation_id = $1 and event_class = 'snapshot_reconciled'",
    )
    .bind(operation_after_first.as_ref().map(|operation| operation.0))
    .fetch_one(test.database.pool())
    .await
    .expect("partial reconciliation audit absence is readable");

    let resume_source =
        SequencedBookmarkPages::new(vec![Ok(snapshot_page("1890123456789012375", None))]);
    let completed = if let Some(run_id) = partial_run_id {
        snapshot_service
            .run(account, &resume_source, Some(run_id))
            .await
            .expect("the resumed complete snapshot finalizes")
    } else {
        partial_outcome
    };
    let completed_snapshot_id = match completed {
        SnapshotOutcome::Completed { snapshot_id, .. } => Some(snapshot_id),
        SnapshotOutcome::Incomplete { .. } => None,
    };
    let operation_after_complete: Option<OperationEvidence> = sqlx::query_as(
        "select id, status, outcome, provider_request_id \
         from x_archive.bookmark_write_operations \
         where account_id = $1 and consent_id = $2",
    )
    .bind(account)
    .bind(consent.id)
    .fetch_optional(test.database.pool())
    .await
    .expect("the operation is readable after complete authority");
    let bookmark_active: bool = sqlx::query_scalar(
        "select exists(select 1 from x_archive.bookmarks bookmark \
         join x_archive.posts post on post.id = bookmark.post_id \
         where bookmark.account_id = $1 and post.provider_id = $2 \
           and bookmark.observed_removed_at is null)",
    )
    .bind(account)
    .bind(target)
    .fetch_one(test.database.pool())
    .await
    .expect("the authoritative bookmark projection is readable");
    let reconciliation_events: Vec<serde_json::Value> = sqlx::query_scalar(
        "select details from x_archive.bookmark_write_audit_events \
         where operation_id = $1 and event_class = 'snapshot_reconciled' \
         order by occurred_at, id",
    )
    .bind(operation_after_first.as_ref().map(|operation| operation.0))
    .fetch_all(test.database.pool())
    .await
    .expect("complete snapshot reconciliation audit is readable");
    let provider_calls = provider.calls.load(Ordering::Relaxed);
    let partial_calls = partial_source.calls();
    let resume_calls = resume_source.calls();

    test.cleanup().await.expect("cleanup drops the database");

    let first_debug = format!("{first:?}");
    let retry_debug = match &retry {
        Ok(result) => format!("{result:?}"),
        Err(_) => "timeout waiting for stored uncertain result".to_owned(),
    };
    let mut violations = Vec::new();
    if !first_debug.contains("Uncertain") || first_debug.contains("Provider(") {
        violations.push(format!(
            "first uncertain mutation did not return the operation result type: {first_debug}"
        ));
    }
    match &operation_after_first {
        Some((_, status, outcome, request_id))
            if status == "uncertain"
                && outcome.as_deref() == Some("uncertain")
                && request_id.as_deref() == Some("request-uncertain-add") => {}
        _ => violations.push(format!(
            "first uncertain result was not durably classified with bounded evidence: {operation_after_first:?}"
        )),
    }
    if retry.is_err()
        || !retry_debug.contains("Uncertain")
        || retry_debug.contains("OperationInProgress")
        || retry_debug.contains("Provider(")
        || retry_debug != first_debug
    {
        violations.push(format!(
            "exact retry did not return the stored uncertain result: first={first_debug}, retry={retry_debug}"
        ));
    }
    if calls_after_retry != 1 || provider_calls != 1 {
        violations.push(format!(
            "uncertain operation was sent to the provider {provider_calls} times (after retry: {calls_after_retry})"
        ));
    }
    if operation_after_retry != operation_after_first {
        violations.push(format!(
            "exact retry rewrote uncertain evidence: first={operation_after_first:?}, retry={operation_after_retry:?}"
        ));
    }
    if partial_run_id.is_none()
        || partial_calls != vec![None, Some("uncertain-next-page".to_owned())]
    {
        violations.push(format!(
            "partial snapshot fixture did not stop after staged presence: outcome={partial_outcome:?}, calls={partial_calls:?}"
        ));
    }
    if operation_after_partial != operation_after_first || partial_reconciliation_events != 0 {
        violations.push(format!(
            "partial evidence resolved or audited the uncertain operation: before={operation_after_first:?}, after={operation_after_partial:?}, events={partial_reconciliation_events}"
        ));
    }
    if completed_snapshot_id.is_none()
        || resume_calls != vec![Some("uncertain-next-page".to_owned())]
    {
        violations.push(format!(
            "resumed snapshot did not complete from its opaque checkpoint: outcome={completed:?}, calls={resume_calls:?}"
        ));
    }
    match (&operation_after_complete, &operation_after_first) {
        (Some((completed_id, status, outcome, request_id)), Some((initial_id, _, _, _)))
            if completed_id == initial_id
                && status == "reconciled_succeeded"
                && outcome.is_some()
                && request_id.as_deref() == Some("request-uncertain-add") => {}
        _ => violations.push(format!(
            "complete presence authority did not resolve uncertain add: initial={operation_after_first:?}, completed={operation_after_complete:?}"
        )),
    }
    if !bookmark_active {
        violations
            .push("complete presence authority did not project an active bookmark".to_owned());
    }
    let snapshot_evidence = completed_snapshot_id.map(|id| id.to_string());
    if reconciliation_events.is_empty()
        || snapshot_evidence.as_ref().is_none_or(|snapshot_id| {
            reconciliation_events
                .iter()
                .all(|details| !details.to_string().contains(snapshot_id))
        })
    {
        violations.push(format!(
            "complete reconciliation omitted append-only snapshot evidence: snapshot={completed_snapshot_id:?}, events={reconciliation_events:?}"
        ));
    }
    assert!(
        violations.is_empty(),
        "uncertain bookmark reconciliation violations: {violations:?}"
    );
}

#[tokio::test]
async fn audit_trail_reconstructs_consent_gate_budget_provider_and_result() {
    #[derive(Debug)]
    struct AuditEvidence {
        id: uuid::Uuid,
        account_id: uuid::Uuid,
        operation_id: Option<uuid::Uuid>,
        consent_id: Option<uuid::Uuid>,
        internal_user_id: uuid::Uuid,
        action: String,
        provider_post_id: String,
        surface: String,
        event_class: String,
        occurred_at: DateTime<Utc>,
        correlation_id: Option<String>,
        idempotency_digest: Option<String>,
        provider_request_id: Option<String>,
        details: serde_json::Value,
    }

    type ImmutableApproval = (
        uuid::Uuid,
        uuid::Uuid,
        String,
        String,
        DateTime<Utc>,
        DateTime<Utc>,
        String,
    );

    let (test, account, owner, service, instant) = authorized_writeback_fixture().await;
    let provider = EvidencedBookmarkProvider::default();
    let target = "1890123456789012376";
    let surface = "web.bookmark-menu";
    let author: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.users (provider_id, parser_version) \
         values ('writeback-audit-author', 1) returning id",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("the audit target author seeds");
    sqlx::query(
        "insert into x_archive.posts (provider_id, author_user_id, parser_version) \
         values ($1, $2, 1)",
    )
    .bind(target)
    .bind(author)
    .execute(test.database.pool())
    .await
    .expect("the known audit target seeds");
    let consent = service
        .record_consent(
            owner,
            account,
            BookmarkAction::Add,
            target,
            instant,
            ConsentSurfaceId::parse(surface).expect("the trusted surface identifier is valid"),
        )
        .await
        .expect("the exact audited consent records");
    let approval_before: ImmutableApproval = sqlx::query_as(
        "select account_id, internal_user_id, action, provider_post_id, approved_at, \
                expires_at, surface \
         from x_archive.bookmark_write_consents where id = $1",
    )
    .bind(consent.id)
    .fetch_one(test.database.pool())
    .await
    .expect("the immutable approval is readable before execution");

    let result = service
        .add_bookmark(
            BookmarkWriteRequest {
                internal_user_id: owner,
                account_id: account,
                consent_id: consent.id,
                provider_post_id: target,
                idempotency_key: "audit-success-known-add",
            },
            &provider,
        )
        .await;

    let approval_after: ImmutableApproval = sqlx::query_as(
        "select account_id, internal_user_id, action, provider_post_id, approved_at, \
                expires_at, surface \
         from x_archive.bookmark_write_consents where id = $1",
    )
    .bind(consent.id)
    .fetch_one(test.database.pool())
    .await
    .expect("the immutable approval is readable after execution");
    let consent_consumed_at: Option<DateTime<Utc>> = sqlx::query_scalar(
        "select consumed_at from x_archive.bookmark_write_consents where id = $1",
    )
    .bind(consent.id)
    .fetch_one(test.database.pool())
    .await
    .expect("the consent consumption instant is readable");
    let rows = sqlx::query(
        "select id, account_id, operation_id, consent_id, internal_user_id, action, \
                provider_post_id, surface, event_class, occurred_at, correlation_id, \
                idempotency_digest, provider_request_id, details \
         from x_archive.bookmark_write_audit_events where consent_id = $1 \
         order by occurred_at, id",
    )
    .bind(consent.id)
    .fetch_all(test.database.pool())
    .await
    .expect("the successful audit history is readable");
    let audit_events: Vec<AuditEvidence> = rows
        .into_iter()
        .map(|row| -> Result<AuditEvidence, sqlx::Error> {
            Ok(AuditEvidence {
                id: row.try_get("id")?,
                account_id: row.try_get("account_id")?,
                operation_id: row.try_get("operation_id")?,
                consent_id: row.try_get("consent_id")?,
                internal_user_id: row.try_get("internal_user_id")?,
                action: row.try_get("action")?,
                provider_post_id: row.try_get("provider_post_id")?,
                surface: row.try_get("surface")?,
                event_class: row.try_get("event_class")?,
                occurred_at: row.try_get("occurred_at")?,
                correlation_id: row.try_get("correlation_id")?,
                idempotency_digest: row.try_get("idempotency_digest")?,
                provider_request_id: row.try_get("provider_request_id")?,
                details: row.try_get("details")?,
            })
        })
        .collect::<Result<_, _>>()
        .expect("the successful audit history has its declared public shape");
    let provider_calls = provider.calls.load(Ordering::Relaxed);

    test.cleanup().await.expect("cleanup drops the database");

    let operation_id = result.as_ref().ok().map(|value| value.operation_id);
    let expected_classes = [
        "consent_recorded",
        "request_received",
        "gate_admitted",
        "provider_attempted",
        "provider_classified",
        "projection_reconciled",
        "operation_completed",
    ];
    let actual_classes: Vec<&str> = audit_events
        .iter()
        .map(|event| event.event_class.as_str())
        .collect();
    let expected_provider_request_id = format!("request-add-{target}");
    let mut violations = Vec::new();
    if !matches!(
        &result,
        Ok(value) if value.status == BookmarkWriteStatus::Succeeded
    ) {
        violations.push(format!(
            "known add did not complete successfully: {result:?}"
        ));
    }
    if provider_calls != 1 {
        violations.push(format!(
            "successful audited add called the provider {provider_calls} time(s)"
        ));
    }
    if approval_after != approval_before {
        violations.push(format!(
            "consent approval was rewritten: before={approval_before:?}, after={approval_after:?}"
        ));
    }
    if consent_consumed_at != Some(instant) {
        violations.push(format!(
            "successful consent has the wrong consumption instant: {consent_consumed_at:?}"
        ));
    }
    if actual_classes != expected_classes {
        violations.push(format!(
            "ordered audit classes cannot reconstruct the reached stages: {actual_classes:?}"
        ));
    }
    if audit_events
        .windows(2)
        .any(|pair| pair[0].occurred_at > pair[1].occurred_at)
    {
        violations.push("audit occurrence instants move backwards".to_owned());
    }
    let operation_correlation = audit_events
        .iter()
        .find(|event| event.event_class != "consent_recorded")
        .and_then(|event| event.correlation_id.as_deref());
    if operation_correlation.is_none()
        || audit_events
            .iter()
            .filter(|event| event.event_class != "consent_recorded")
            .any(|event| event.correlation_id.as_deref() != operation_correlation)
    {
        violations.push(format!(
            "operation stages lack one stable correlation identity: {:?}",
            audit_events
                .iter()
                .map(|event| (&event.event_class, &event.correlation_id))
                .collect::<Vec<_>>()
        ));
    }
    for event in &audit_events {
        let consent_stage = event.event_class == "consent_recorded";
        if event.account_id != account
            || event.consent_id != Some(consent.id)
            || event.internal_user_id != owner
            || event.action != "add"
            || event.provider_post_id != target
            || event.surface != surface
            || event.occurred_at != instant
            || (!consent_stage && event.operation_id != operation_id)
        {
            violations.push(format!(
                "audit row {} lost actor/action/target/time/surface binding: {event:?}",
                event.id
            ));
        }
        if !event.details.is_object() {
            violations.push(format!(
                "audit row {} has an unclassified details payload: {:?}",
                event.id, event.details
            ));
        }
        if consent_stage {
            if event.operation_id.is_some()
                || event.idempotency_digest.is_some()
                || event.correlation_id.is_some()
            {
                violations.push(format!(
                    "consent audit fabricated operation identity: {event:?}"
                ));
            }
        } else {
            let valid_digest = event.idempotency_digest.as_deref().is_some_and(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            });
            if !valid_digest {
                violations.push(format!(
                    "operation audit row {} lacks its SHA-256 idempotency identity: {:?}",
                    event.id, event.idempotency_digest
                ));
            }
        }
        if let Some(request_id) = event.provider_request_id.as_deref() {
            if request_id != expected_provider_request_id {
                violations.push(format!(
                    "audit row {} retained unexpected provider evidence: {request_id:?}",
                    event.id
                ));
            }
        }
        if matches!(
            event.event_class.as_str(),
            "consent_recorded" | "request_received" | "gate_admitted" | "provider_attempted"
        ) && event.provider_request_id.is_some()
        {
            violations.push(format!(
                "pre-classification audit row {} fabricated provider response evidence",
                event.id
            ));
        }
    }
    let provider_attempt = audit_events
        .iter()
        .find(|event| event.event_class == "provider_attempted");
    if provider_attempt.is_none_or(|event| {
        event
            .details
            .get("budget_class")
            .and_then(serde_json::Value::as_str)
            != Some("bookmark_write")
            || event
                .details
                .get("cost")
                .and_then(serde_json::Value::as_i64)
                != Some(1)
    }) {
        violations.push(format!(
            "provider attempt omitted isolated write-budget evidence: {provider_attempt:?}"
        ));
    }
    let provider_classified = audit_events
        .iter()
        .find(|event| event.event_class == "provider_classified");
    if provider_classified.is_none_or(|event| {
        event.provider_request_id.as_deref() != Some(expected_provider_request_id.as_str())
            || event
                .details
                .get("result")
                .and_then(serde_json::Value::as_str)
                != Some("confirmed")
    }) {
        violations.push(format!(
            "provider classification omitted bounded result evidence: {provider_classified:?}"
        ));
    }
    let projection = audit_events
        .iter()
        .find(|event| event.event_class == "projection_reconciled");
    if projection.is_none_or(|event| {
        event
            .details
            .get("projection")
            .and_then(serde_json::Value::as_str)
            != Some("updated")
    }) {
        violations.push(format!(
            "projection stage omitted the known-target result: {projection:?}"
        ));
    }
    let completed = audit_events
        .iter()
        .find(|event| event.event_class == "operation_completed");
    if completed.is_none_or(|event| {
        event
            .details
            .get("result")
            .and_then(serde_json::Value::as_str)
            != Some("confirmed")
            || event
                .details
                .get("projection")
                .and_then(serde_json::Value::as_str)
                != Some("updated")
    }) {
        violations.push(format!(
            "terminal audit omitted the returned confirmed result: {completed:?}"
        ));
    }
    assert!(
        violations.is_empty(),
        "successful bookmark audit violations: {violations:?}"
    );
}
