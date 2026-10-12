//! No path may delete a `social.source.removed.v1` outbox row that Knowledge has not acknowledged
//! (XR-021 CONTRACTS.md R5-11 rule 4): the events stream drops its oldest messages, so the row is
//! the only copy of the deletion the service can send again. The repository has no code that
//! deletes outbox rows today; the guard lives in the schema, where every present and future path
//! meets it.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use sqlx::types::Uuid;
use x_persistence::outbox::{AckOutcome, REMOVAL_EVENT_TYPE, acknowledge};
use x_persistence::test_support::TestDatabase;

const OWNER: &str = "user:018f0000-0000-7000-8000-000000000005";

async fn insert_row(test: &TestDatabase, event_type: &str, published: bool) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into x_archive.outbox_events \
           (id, aggregate, event_type, payload, published_at) \
         values ($1, $2, $3, $4::jsonb, case when $5 then now() end)",
    )
    .bind(id)
    .bind(format!("social_source:{id}"))
    .bind(event_type)
    .bind(serde_json::json!({"tenant_id": OWNER}).to_string())
    .bind(published)
    .execute(test.database.pool())
    .await
    .expect("the outbox row seeds");
    id
}

async fn exists(test: &TestDatabase, id: Uuid) -> bool {
    sqlx::query_scalar("select count(*) = 1 from x_archive.outbox_events where id = $1")
        .bind(id)
        .fetch_one(test.database.pool())
        .await
        .expect("the row count is readable")
}

#[tokio::test]
async fn an_unacknowledged_removal_cannot_be_deleted_published_or_not() {
    let test = TestDatabase::create().await.expect("a disposable database");
    for published in [false, true] {
        let id = insert_row(&test, REMOVAL_EVENT_TYPE, published).await;

        let result = sqlx::query("delete from x_archive.outbox_events where id = $1")
            .bind(id)
            .execute(test.database.pool())
            .await;

        assert!(
            result.is_err(),
            "published={published}: the delete of an unacknowledged removal must fail"
        );
        assert!(
            exists(&test, id).await,
            "published={published}: the row stays"
        );
    }
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn an_acknowledged_removal_and_every_other_event_type_can_be_deleted() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let removal = insert_row(&test, REMOVAL_EVENT_TYPE, true).await;
    acknowledge(test.database.pool(), removal, OWNER, AckOutcome::Refused)
        .await
        .expect("the removal is acknowledged");
    let mut others = Vec::new();
    for event_type in [
        "social.source.captured.v1",
        "social.source.updated.v1",
        "platform.operation.reported.v1",
        "content.capture.requested.v1",
    ] {
        others.push(insert_row(&test, event_type, false).await);
    }

    sqlx::query("delete from x_archive.outbox_events where id = any($1)")
        .bind([vec![removal], others.clone()].concat())
        .execute(test.database.pool())
        .await
        .expect("acknowledged removals and other facts are deletable");

    assert!(!exists(&test, removal).await);
    for id in others {
        assert!(!exists(&test, id).await);
    }
    test.cleanup().await.expect("cleanup drops the database");
}

/// A retention-style sweep that does not exclude the unacknowledged removal is refused as a whole,
/// and the predicate of the rule lets the same sweep through.
#[tokio::test]
async fn a_sweep_must_exclude_unacknowledged_removals() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let waiting = insert_row(&test, REMOVAL_EVENT_TYPE, true).await;
    let captured = insert_row(&test, "social.source.captured.v1", true).await;

    let blind = sqlx::query("delete from x_archive.outbox_events where published_at is not null")
        .execute(test.database.pool())
        .await;
    assert!(blind.is_err(), "a sweep that ignores the rule is refused");
    assert!(
        exists(&test, captured).await,
        "the refused sweep deleted nothing"
    );

    sqlx::query(
        "delete from x_archive.outbox_events where published_at is not null \
            and not (event_type = 'social.source.removed.v1' and acknowledged_at is null)",
    )
    .execute(test.database.pool())
    .await
    .expect("the sweep with the rule's predicate succeeds");
    assert!(!exists(&test, captured).await);
    assert!(
        exists(&test, waiting).await,
        "the unacknowledged removal survives"
    );
    test.cleanup().await.expect("cleanup drops the database");
}
