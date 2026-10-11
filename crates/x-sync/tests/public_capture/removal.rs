//! A provider deletion found by a later capture removes the owner's source (XR-021 CONTRACTS.md
//! R2-09).

use ratatoskr_operation_contracts::OperationStatus;
use ratatoskr_social_contracts::SocialSourceRemoved;
use uuid::Uuid;
use x_persistence::test_support::TestDatabase;
use x_sync::PublicPostFailure;

use crate::support::*;

async fn source_ids(test: &TestDatabase, owner: Uuid) -> Vec<Uuid> {
    sqlx::query_scalar("select social_source_id from x_archive.explicit_sources where owner = $1")
        .bind(owner)
        .fetch_all(test.database.pool())
        .await
        .expect("the sources are readable")
}

#[tokio::test]
async fn a_recapture_of_a_deleted_post_removes_the_owner_source_and_queues_removal() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let (owner, other) = (uuid(9, 1), uuid(9, 2));
    seed_capture(&test, 1, owner).await;
    seed_capture(&test, 2, other).await;
    let resolver = ScriptedResolver::new([Ok(post("Kept text.")), Ok(post("Kept text."))]);
    let clock = TestClock::at(START);
    let worker = worker(&test, &resolver, &clock);
    worker.run_due_once().await.expect("both owners preserve");
    let removed_id = *source_ids(&test, owner)
        .await
        .first()
        .expect("the owner preserved a source");
    let untouched_id = *source_ids(&test, other)
        .await
        .first()
        .expect("the other tenant preserved a source");

    let deleted = seed_capture(&test, 3, owner).await;
    let resolver = ScriptedResolver::new([Err(PublicPostFailure::Deleted)]);
    let summary = self::worker(&test, &resolver, &clock)
        .run_due_once()
        .await
        .expect("the pass completes");
    assert_eq!((summary.claimed, summary.unavailable), (1, 1));

    assert!(
        source_ids(&test, owner).await.is_empty(),
        "the owner source is gone"
    );
    assert_eq!(source_ids(&test, other).await, vec![untouched_id]);
    let removals = envelopes(&test, "social.source.removed.v1").await;
    let [removal] = removals.as_slice() else {
        panic!("exactly one removal, got {}", removals.len());
    };
    assert_eq!(removal.producer.as_str(), "ratatoskr-x");
    assert_eq!(
        removal.aggregate_id.to_wire(),
        format!("social_source:{removed_id}")
    );
    assert_eq!(
        removal.tenant_id.map(|tenant| tenant.to_string()),
        Some(format!("user:{owner}"))
    );
    let event: SocialSourceRemoved = removal.payload_as().expect("a removal payload");
    let payload = serde_json::to_value(event).expect("the removal serializes");
    assert_eq!(field(&payload, "social_source_id"), removed_id.to_string());
    assert_eq!(field(&payload, "owner"), format!("user:{owner}"));
    assert_eq!(field(&payload, "reason"), "retention_policy");

    let reported: Vec<_> = reports(&test)
        .await
        .into_iter()
        .filter(|(_, report)| report.operation_id.0 == deleted.operation_id)
        .collect();
    let [(_, report)] = reported.as_slice() else {
        panic!("exactly one report for the deleted capture");
    };
    assert_eq!(report.status, OperationStatus::Failed);
    assert_eq!(
        report.error.clone().expect("an error").code.as_str(),
        "social.source.deleted"
    );
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn a_first_capture_of_a_deleted_post_removes_nothing() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let owner = uuid(9, 1);
    let other = uuid(9, 2);
    seed_capture(&test, 1, other).await;
    worker(
        &test,
        &ScriptedResolver::new([Ok(post("Someone else's source."))]),
        &TestClock::at(START),
    )
    .run_due_once()
    .await
    .expect("another tenant preserves the post");
    let queued_before = envelopes(&test, "social.source.removed.v1").await.len();

    let capture = seed_capture(&test, 2, owner).await;
    let resolver = ScriptedResolver::new([Err(PublicPostFailure::Deleted)]);
    worker(&test, &resolver, &TestClock::at(START))
        .run_due_once()
        .await
        .expect("the pass completes");

    assert_eq!(queued_before, 0);
    assert!(
        envelopes(&test, "social.source.removed.v1")
            .await
            .is_empty(),
        "no removal for an owner who never preserved the post"
    );
    assert_eq!(source_ids(&test, other).await.len(), 1);
    let state = capture_state(&test, capture.capture_id).await;
    assert_eq!((state.0.as_str(), state.3), ("unavailable", true));
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn inaccessible_and_transient_outcomes_remove_nothing() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let owner = uuid(9, 1);
    seed_capture(&test, 1, owner).await;
    let clock = TestClock::at(START);
    worker(
        &test,
        &ScriptedResolver::new([Ok(post("Kept text."))]),
        &clock,
    )
    .run_due_once()
    .await
    .expect("the owner preserves the post");
    let kept = source_ids(&test, owner).await;
    assert_eq!(kept.len(), 1);

    // One capture ends inaccessible; another exhausts its transient attempts.
    seed_capture(&test, 2, owner).await;
    worker(
        &test,
        &ScriptedResolver::new([Err(PublicPostFailure::Inaccessible)]),
        &clock,
    )
    .run_due_once()
    .await
    .expect("the inaccessible pass completes");
    seed_capture(&test, 3, owner).await;
    let transient = ScriptedResolver::new((0..5).map(|_| Err(PublicPostFailure::Transient)));
    let worker = worker(&test, &transient, &clock);
    for _ in 0..5 {
        worker.run_due_once().await.expect("a transient pass");
        clock.advance(31 * 60);
    }
    assert_eq!(transient.calls(), 5, "the attempts ran out");

    assert_eq!(source_ids(&test, owner).await, kept, "the source stays");
    assert!(
        envelopes(&test, "social.source.removed.v1")
            .await
            .is_empty(),
        "no removal for inaccessible or transient outcomes"
    );
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn a_restored_post_is_preserved_again_as_a_new_source() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let owner = uuid(9, 1);
    let clock = TestClock::at(START);
    seed_capture(&test, 1, owner).await;
    worker(
        &test,
        &ScriptedResolver::new([Ok(post("Before the deletion."))]),
        &clock,
    )
    .run_due_once()
    .await
    .expect("the first preservation");
    let first = source_ids(&test, owner).await;
    seed_capture(&test, 2, owner).await;
    worker(
        &test,
        &ScriptedResolver::new([Err(PublicPostFailure::Deleted)]),
        &clock,
    )
    .run_due_once()
    .await
    .expect("the deletion");
    assert!(source_ids(&test, owner).await.is_empty());

    seed_capture(&test, 3, owner).await;
    worker(
        &test,
        &ScriptedResolver::new([Ok(post("After the restore."))]),
        &clock,
    )
    .run_due_once()
    .await
    .expect("the restored post");

    let restored = source_ids(&test, owner).await;
    assert_eq!(restored.len(), 1);
    assert_ne!(restored, first, "a new source identity");
    let captured = envelopes(&test, "social.source.captured.v1").await;
    assert_eq!(captured.len(), 2, "the restore is a captured fact again");
    assert_eq!(
        envelopes(&test, "social.source.removed.v1").await.len(),
        1,
        "the earlier deletion was the only removal"
    );
    test.cleanup().await.expect("cleanup drops the database");
}
