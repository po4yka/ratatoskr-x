//! Outcomes of one capture: preserved, recaptured, shared between tenants, or unavailable.

use ratatoskr_event_envelope::EventEnvelope;
use ratatoskr_operation_contracts::OperationStatus;
use ratatoskr_social_contracts::SocialSourceUpdated;
use uuid::Uuid;
use x_persistence::test_support::TestDatabase;
use x_sync::{PublicPost, PublicPostFailure};

use crate::support::*;

#[tokio::test]
async fn a_preserved_capture_publishes_the_owner_source_and_reports_once() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let owner = uuid(9, 1);
    let capture = seed_capture(&test, 1, owner).await;
    let resolver = ScriptedResolver::new([Ok(post("A public post."))]);
    let clock = TestClock::at(START);
    let worker = worker(&test, &resolver, &clock);

    let summary = worker.run_due_once().await.expect("the pass completes");
    assert_eq!((summary.claimed, summary.preserved), (1, 1));

    let (source_id, stored_owner, stored_text): (Uuid, Uuid, String) =
        sqlx::query_as("select social_source_id, owner, text from x_archive.explicit_sources")
            .fetch_one(test.database.pool())
            .await
            .expect("exactly one owner source exists");
    assert_eq!(stored_owner, owner);
    assert_eq!(stored_text, "A public post.");
    assert_captured_fact(&test, owner, source_id).await;
    assert_success_report(&test, &capture, source_id).await;

    let state = capture_state(&test, capture.capture_id).await;
    assert_eq!((state.0.as_str(), state.3), ("resolved", true));
    let linked: Option<Uuid> = sqlx::query_scalar(
        "select social_source_id from x_archive.explicit_captures where capture_id = $1",
    )
    .bind(capture.capture_id)
    .fetch_one(test.database.pool())
    .await
    .expect("the link is readable");
    assert_eq!(linked, Some(source_id));

    let again = worker
        .run_due_once()
        .await
        .expect("the second pass completes");
    assert_eq!(
        again.claimed, 0,
        "a reported capture is never claimed again"
    );
    assert_eq!(resolver.calls(), 1);
    assert_eq!(
        count(&test, "select count(*) from x_archive.outbox_events").await,
        2
    );
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn recapturing_the_same_post_reports_success_and_updates_only_when_the_text_changed() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let owner = uuid(9, 1);
    let first = seed_capture(&test, 1, owner).await;
    let resolver = ScriptedResolver::new([
        Ok(post("Version one.")),
        Ok(post("Version one.")),
        Ok(post("Version two.")),
    ]);
    let clock = TestClock::at(START);
    let worker = worker(&test, &resolver, &clock);
    worker.run_due_once().await.expect("the first capture");
    let source_id: Uuid =
        sqlx::query_scalar("select social_source_id from x_archive.explicit_sources")
            .fetch_one(test.database.pool())
            .await
            .expect("the source exists");

    let second = seed_capture(&test, 2, owner).await;
    worker
        .run_due_once()
        .await
        .expect("the unchanged recapture");
    assert_eq!(
        envelopes(&test, "social.source.captured.v1").await.len(),
        1,
        "no second captured event"
    );
    assert_eq!(
        envelopes(&test, "social.source.updated.v1").await.len(),
        0,
        "unchanged content emits nothing"
    );
    let reported = reports(&test).await;
    assert_eq!(reported.len(), 2, "the new operation still gets its report");
    assert_eq!(
        reported
            .get(1)
            .expect("the element exists")
            .1
            .operation_id
            .0,
        second.operation_id
    );
    assert_eq!(
        reported.get(1).expect("the element exists").1.status,
        OperationStatus::Succeeded
    );
    assert_eq!(
        reported
            .get(1)
            .expect("the element exists")
            .1
            .results
            .first()
            .expect("the element exists")
            .target
            .to_wire(),
        format!("social_source:{source_id}"),
        "the same owner keeps one source identity"
    );

    let third = seed_capture(&test, 3, owner).await;
    worker.run_due_once().await.expect("the changed recapture");
    let updated = envelopes(&test, "social.source.updated.v1").await;
    assert_eq!(updated.len(), 1, "changed content emits one updated event");
    let event: SocialSourceUpdated = updated
        .first()
        .expect("the element exists")
        .payload_as()
        .expect("an updated payload");
    assert_eq!(
        serde_json::to_value(event.source).expect("serializes")["text"],
        "Version two."
    );
    let current: String = sqlx::query_scalar("select text from x_archive.explicit_sources")
        .fetch_one(test.database.pool())
        .await
        .expect("one source row");
    assert_eq!(current, "Version two.");
    assert_eq!(reports(&test).await.len(), 3);
    assert_eq!(
        reports(&test)
            .await
            .get(2)
            .expect("the element exists")
            .1
            .operation_id
            .0,
        third.operation_id
    );
    assert_eq!(first.owner, owner);
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn two_tenants_capturing_one_public_post_get_distinct_sources_and_their_own_events() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let (alice, bob) = (uuid(9, 1), uuid(9, 2));
    seed_capture(&test, 1, alice).await;
    seed_capture(&test, 2, bob).await;
    let resolver = ScriptedResolver::new([
        Ok(post("Shared public text.")),
        Ok(post("Shared public text.")),
    ]);
    let clock = TestClock::at(START);

    let summary = worker(&test, &resolver, &clock)
        .run_due_once()
        .await
        .expect("the pass completes");
    assert_eq!(summary.preserved, 2);

    let sources: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "select social_source_id, owner from x_archive.explicit_sources order by owner",
    )
    .fetch_all(test.database.pool())
    .await
    .expect("sources are readable");
    assert_eq!(sources.len(), 2);
    assert_ne!(
        sources.first().expect("the element exists").0,
        sources.get(1).expect("the element exists").0,
        "identities are per owner"
    );
    let captured = envelopes(&test, "social.source.captured.v1").await;
    assert_eq!(captured.len(), 2);
    for (source_id, owner) in &sources {
        let own: Vec<&EventEnvelope> = captured
            .iter()
            .filter(|envelope| {
                envelope.aggregate_id.to_wire() == format!("social_source:{source_id}")
            })
            .collect();
        assert_eq!(own.len(), 1, "each source has its own event");
        assert_eq!(
            own.first()
                .expect("the element exists")
                .tenant_id
                .map(|tenant| tenant.to_string()),
            Some(format!("user:{owner}")),
            "the event is addressed to its owner only"
        );
    }
    let reported = reports(&test).await;
    assert_eq!(reported.len(), 2);
    for (envelope, report) in &reported {
        let source = sources
            .iter()
            .find(|(_, owner)| {
                envelope.tenant_id.map(|tenant| tenant.to_string()) == Some(format!("user:{owner}"))
            })
            .expect("the report names one of the owners");
        assert_eq!(
            report
                .results
                .first()
                .expect("the element exists")
                .target
                .to_wire(),
            format!("social_source:{}", source.0),
            "a report points at the reporting tenant's own source"
        );
    }
    test.cleanup().await.expect("cleanup drops the database");
}

async fn terminal_failure(failure: PublicPostFailure) -> (String, String, bool, i64) {
    let test = TestDatabase::create().await.expect("a disposable database");
    let capture = seed_capture(&test, 1, uuid(9, 1)).await;
    let resolver = ScriptedResolver::new([Err(failure)]);
    let clock = TestClock::at(START);

    let summary = worker(&test, &resolver, &clock)
        .run_due_once()
        .await
        .expect("the pass completes");
    assert_eq!((summary.claimed, summary.unavailable), (1, 1));

    let reported = reports(&test).await;
    assert_eq!(reported.len(), 1, "exactly one terminal report");
    assert_eq!(
        reported.first().expect("the element exists").1.status,
        OperationStatus::Failed
    );
    assert!(
        reported
            .first()
            .expect("the element exists")
            .1
            .results
            .is_empty()
    );
    let error = reported
        .first()
        .expect("the element exists")
        .1
        .error
        .clone()
        .expect("a failed report carries its error");
    let state = capture_state(&test, capture.capture_id).await;
    assert_eq!((state.0.as_str(), state.3), ("unavailable", true));
    let social = count(
        &test,
        "select (select count(*) from x_archive.explicit_sources) \
              + (select count(*) from x_archive.outbox_events \
                  where event_type like 'social.source.%')",
    )
    .await;
    test.cleanup().await.expect("cleanup drops the database");
    (
        error.code.as_str().to_owned(),
        reported
            .first()
            .expect("the element exists")
            .1
            .stage
            .clone()
            .map_or_else(String::new, |stage| stage.to_string()),
        error.retryable,
        social,
    )
}

#[tokio::test]
async fn a_deleted_post_is_reported_deleted_and_publishes_nothing() {
    let (code, stage, retryable, social) = terminal_failure(PublicPostFailure::Deleted).await;
    assert_eq!(code, "social.source.deleted");
    assert_eq!(stage, "capture_unavailable");
    assert!(!retryable);
    assert_eq!(social, 0, "no source and no social event");
}

#[tokio::test]
async fn an_inaccessible_post_is_reported_unavailable_and_publishes_nothing() {
    let (code, _, retryable, social) = terminal_failure(PublicPostFailure::Inaccessible).await;
    assert_eq!(code, "social.source.unavailable");
    assert!(!retryable);
    assert_eq!(social, 0, "no source and no social event");
}

/// A resolved post the shared social contract cannot represent is a permanent failure of that one
/// capture. It must end `Inaccessible` and must never stop the worker, the service or the batch.
async fn assert_unrepresentable_post_ends_unavailable(unrepresentable: PublicPost) {
    let test = TestDatabase::create().await.expect("a disposable database");
    let poison = seed_capture(&test, 1, uuid(9, 1)).await;
    let behind = seed_capture(&test, 2, uuid(9, 2)).await;
    let resolver = ScriptedResolver::new([Ok(unrepresentable), Ok(post("A healthy public post."))]);
    let clock = TestClock::at(START);

    let summary = worker(&test, &resolver, &clock)
        .run_due_once()
        .await
        .expect("a post the contract cannot represent does not fail the pass");
    assert_eq!(
        (summary.claimed, summary.unavailable, summary.preserved),
        (2, 1, 1),
        "the poison capture ends, the next capture in the batch is still served"
    );

    let state = capture_state(&test, poison.capture_id).await;
    assert_eq!((state.0.as_str(), state.3), ("unavailable", true));
    let behind_state = capture_state(&test, behind.capture_id).await;
    assert_eq!(
        (behind_state.0.as_str(), behind_state.3),
        ("resolved", true)
    );

    let reported = reports(&test).await;
    let poison_reports: Vec<_> = reported
        .iter()
        .filter(|(_, report)| report.operation_id.0 == poison.operation_id)
        .collect();
    let [(_, report)] = poison_reports.as_slice() else {
        panic!("exactly one report for the poison capture");
    };
    assert_eq!(report.status, OperationStatus::Failed);
    let error = report
        .error
        .clone()
        .expect("a failed report carries its error");
    assert_eq!(error.code.as_str(), "social.source.unavailable");
    assert!(!error.retryable);
    assert_eq!(
        count(
            &test,
            &format!(
                "select count(*) from x_archive.explicit_sources where owner = '{}'",
                poison.owner
            )
        )
        .await,
        0,
        "no source for the poison capture"
    );
    assert_eq!(
        count(
            &test,
            &format!(
                "select count(*) from x_archive.outbox_events \
                  where event_type like 'social.source.%' \
                    and payload->>'tenant_id' = 'user:{}'",
                poison.owner
            )
        )
        .await,
        0,
        "no social event for the poison capture"
    );
    let again = worker(&test, &resolver, &clock)
        .run_due_once()
        .await
        .expect("the next pass completes");
    assert_eq!(
        again.claimed, 0,
        "a reported poison capture is never claimed again"
    );
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn a_post_with_empty_text_ends_the_capture_unavailable_instead_of_failing_the_pass() {
    assert_unrepresentable_post_ends_unavailable(post("")).await;
}

#[tokio::test]
async fn a_post_with_a_control_character_ends_the_capture_unavailable() {
    assert_unrepresentable_post_ends_unavailable(post("bell \u{7} in the text")).await;
}

#[tokio::test]
async fn a_post_whose_handle_the_contract_rejects_ends_the_capture_unavailable() {
    let mut unrepresentable = post("A public post.");
    unrepresentable.author.username = Some("not a handle!".to_owned());
    assert_unrepresentable_post_ends_unavailable(unrepresentable).await;
}

#[tokio::test]
async fn a_display_name_with_a_control_character_ends_the_capture_unavailable() {
    let mut unrepresentable = post("A public post.");
    unrepresentable.author.name = Some("Ada \u{7}Example".to_owned());
    assert_unrepresentable_post_ends_unavailable(unrepresentable).await;
}

/// The reproduced tenant-isolation leak: a protected post stored by tenant A's sync must never
/// reach tenant B through an explicit capture.
#[tokio::test]
async fn a_protected_post_another_tenant_synced_is_never_published_to_the_capturing_tenant() {
    const PROTECTED_TEXT: &str = "text only A was allowed to see";
    let test = TestDatabase::create().await.expect("a disposable database");
    let author: Uuid = sqlx::query_scalar(
        "insert into x_archive.users (provider_id, parser_version) \
         values ('protected-author', 1) returning id",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("the author seeds");
    sqlx::query(
        "insert into x_archive.posts \
           (provider_id, author_user_id, text, parser_version, availability) \
         values ($1, $2, $3, 1, 'protected')",
    )
    .bind(POST)
    .bind(author)
    .bind(PROTECTED_TEXT)
    .execute(test.database.pool())
    .await
    .expect("the shared row, as tenant A's sync stored it, seeds");
    seed_capture(&test, 1, uuid(9, 2)).await;
    let resolver = ScriptedResolver::new([Err(PublicPostFailure::Inaccessible)]);
    let clock = TestClock::at(START);

    worker(&test, &resolver, &clock)
        .run_due_once()
        .await
        .expect("the pass completes");

    let reported = reports(&test).await;
    assert_eq!(reported.len(), 1);
    assert_eq!(
        reported.first().expect("the element exists").1.status,
        OperationStatus::Failed
    );
    assert_eq!(
        reported
            .first()
            .expect("the element exists")
            .1
            .error
            .clone()
            .expect("an error")
            .code
            .as_str(),
        "social.source.unavailable"
    );
    assert_eq!(
        count(&test, "select count(*) from x_archive.explicit_sources").await,
        0
    );
    assert_eq!(
        count(
            &test,
            "select count(*) from x_archive.outbox_events where event_type like 'social.source.%'"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &test,
            &format!(
                "select count(*) from x_archive.outbox_events \
                 where payload::text like '%{PROTECTED_TEXT}%'"
            )
        )
        .await,
        0,
        "nothing the other tenant was allowed to read leaves the service"
    );
    let untouched: (String, String) =
        sqlx::query_as("select text, availability from x_archive.posts where provider_id = $1")
            .bind(POST)
            .fetch_one(test.database.pool())
            .await
            .expect("the shared row is still there");
    assert_eq!(
        untouched,
        (PROTECTED_TEXT.to_owned(), "protected".to_owned())
    );
    test.cleanup().await.expect("cleanup drops the database");
}
