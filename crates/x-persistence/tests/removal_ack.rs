//! The outbox transitions of a removal that waits for Knowledge's acknowledgement (XR-021
//! CONTRACTS.md R5-11 rule 4): armed when published, rescheduled with a doubling delay when
//! published again, ended by an acknowledgement of the same tenant.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::time::Duration;

use sqlx::types::Uuid;
use x_persistence::outbox::{
    AckOutcome, REMOVAL_EVENT_TYPE, ResendPolicy, acknowledge, mark_published, record_resend,
    unacknowledged_removals,
};
use x_persistence::test_support::TestDatabase;

const OWNER: &str = "user:018f0000-0000-7000-8000-000000000005";
const OTHER_OWNER: &str = "user:018f0000-0000-7000-8000-000000000006";

fn policy() -> ResendPolicy {
    ResendPolicy::new(Duration::from_secs(600), Duration::from_secs(3_000))
}

async fn insert_row(test: &TestDatabase, event_type: &str, tenant: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into x_archive.outbox_events (id, aggregate, event_type, payload) \
         values ($1, $2, $3, $4::jsonb)",
    )
    .bind(id)
    .bind(format!("social_source:{id}"))
    .bind(event_type)
    .bind(serde_json::json!({"event_type": event_type, "tenant_id": tenant}).to_string())
    .execute(test.database.pool())
    .await
    .expect("the outbox row seeds");
    id
}

#[derive(Debug, PartialEq)]
struct State {
    published: bool,
    resend_count: i32,
    acknowledged: bool,
    outcome: Option<String>,
}

async fn state(test: &TestDatabase, id: Uuid) -> State {
    let (published, resend_count, acknowledged, outcome): (bool, i32, bool, Option<String>) =
        sqlx::query_as(
            "select published_at is not null, resend_count, acknowledged_at is not null, \
                    ack_outcome from x_archive.outbox_events where id = $1",
        )
        .bind(id)
        .fetch_one(test.database.pool())
        .await
        .expect("the row is readable");
    State {
        published,
        resend_count,
        acknowledged,
        outcome,
    }
}

/// Seconds from now until `next_resend_at`, or `None` while it is null.
async fn resend_in(test: &TestDatabase, id: Uuid) -> Option<f64> {
    sqlx::query_scalar(
        "select extract(epoch from next_resend_at - now())::float8 \
           from x_archive.outbox_events where id = $1",
    )
    .bind(id)
    .fetch_one(test.database.pool())
    .await
    .expect("the row is readable")
}

#[tokio::test]
async fn a_published_removal_is_armed_and_no_other_event_type_is() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let removal = insert_row(&test, REMOVAL_EVENT_TYPE, OWNER).await;
    mark_published(test.database.pool(), removal, policy())
        .await
        .expect("the removal is marked");

    let gap: f64 = sqlx::query_scalar(
        "select extract(epoch from next_resend_at - published_at)::float8 \
           from x_archive.outbox_events where id = $1",
    )
    .bind(removal)
    .fetch_one(test.database.pool())
    .await
    .expect("the armed removal has both instants");
    assert!(
        (gap - 600.0).abs() < 0.001,
        "next_resend_at is published_at plus resend_after, got {gap}"
    );
    assert_eq!(state(&test, removal).await.resend_count, 0);

    for event_type in [
        "social.source.captured.v1",
        "social.source.updated.v1",
        "platform.operation.reported.v1",
    ] {
        let id = insert_row(&test, event_type, OWNER).await;
        mark_published(test.database.pool(), id, policy())
            .await
            .expect("the row is marked");
        assert!(state(&test, id).await.published, "{event_type} is marked");
        assert_eq!(
            resend_in(&test, id).await,
            None,
            "{event_type} is never resent"
        );
    }
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn each_resend_counts_and_doubles_the_delay_up_to_the_cap() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let id = insert_row(&test, REMOVAL_EVENT_TYPE, OWNER).await;
    mark_published(test.database.pool(), id, policy())
        .await
        .expect("the removal is marked");

    // 600 s doubles to 1200 s and 2400 s, then the 3000 s cap holds for every later copy.
    for (expected_count, expected_delay) in [
        (1, 600.0),
        (2, 1_200.0),
        (3, 2_400.0),
        (4, 3_000.0),
        (5, 3_000.0),
    ] {
        record_resend(test.database.pool(), id, policy())
            .await
            .expect("the resend is recorded");
        assert_eq!(state(&test, id).await.resend_count, expected_count);
        let delay = resend_in(&test, id).await.expect("a resend is scheduled");
        assert!(
            (expected_delay - 5.0..=expected_delay).contains(&delay),
            "resend {expected_count} is due in {expected_delay} s, got {delay}"
        );
    }
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn a_large_resend_count_cannot_overflow_the_delay() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let id = insert_row(&test, REMOVAL_EVENT_TYPE, OWNER).await;
    mark_published(test.database.pool(), id, policy())
        .await
        .expect("the removal is marked");
    sqlx::query("update x_archive.outbox_events set resend_count = 2000 where id = $1")
        .bind(id)
        .execute(test.database.pool())
        .await
        .expect("the count is set");
    record_resend(test.database.pool(), id, policy())
        .await
        .expect("a huge count still schedules");
    let delay = resend_in(&test, id).await.expect("a resend is scheduled");
    assert!((2_995.0..=3_000.0).contains(&delay), "capped, got {delay}");
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn an_acknowledgement_of_the_same_tenant_ends_the_resend_once() {
    let test = TestDatabase::create().await.expect("a disposable database");
    for (outcome, stored) in [
        (AckOutcome::Applied, "applied"),
        (AckOutcome::Refused, "refused"),
    ] {
        let id = insert_row(&test, REMOVAL_EVENT_TYPE, OWNER).await;
        mark_published(test.database.pool(), id, policy())
            .await
            .expect("the removal is marked");

        let changed = acknowledge(test.database.pool(), id, OWNER, outcome)
            .await
            .expect("the acknowledgement is recorded");

        assert!(changed, "{stored}: the row changed");
        let after = state(&test, id).await;
        assert!(after.acknowledged, "{stored}: acknowledged_at is set");
        assert_eq!(after.outcome.as_deref(), Some(stored));

        let again = acknowledge(test.database.pool(), id, OWNER, AckOutcome::Applied)
            .await
            .expect("a duplicate is accepted");
        assert!(!again, "{stored}: a duplicate changes nothing");
        assert_eq!(
            state(&test, id).await,
            after,
            "{stored}: the row is unchanged"
        );
    }
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn an_acknowledgement_for_another_tenant_or_no_row_changes_nothing() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let id = insert_row(&test, REMOVAL_EVENT_TYPE, OWNER).await;
    mark_published(test.database.pool(), id, policy())
        .await
        .expect("the removal is marked");
    let before = state(&test, id).await;

    let foreign = acknowledge(test.database.pool(), id, OTHER_OWNER, AckOutcome::Applied)
        .await
        .expect("another tenant's acknowledgement is accepted");
    let unknown = acknowledge(
        test.database.pool(),
        Uuid::now_v7(),
        OWNER,
        AckOutcome::Applied,
    )
    .await
    .expect("an unknown id is accepted");

    assert!(
        !foreign && !unknown,
        "neither acknowledgement changes a row"
    );
    assert_eq!(state(&test, id).await, before);

    let captured = insert_row(&test, "social.source.captured.v1", OWNER).await;
    let changed = acknowledge(test.database.pool(), captured, OWNER, AckOutcome::Applied)
        .await
        .expect("an acknowledgement of a non-removal row is accepted");
    assert!(!changed, "only a removal row can be acknowledged");

    let owner = acknowledge(test.database.pool(), id, OWNER, AckOutcome::Applied)
        .await
        .expect("the owner's acknowledgement is recorded");
    assert!(
        owner,
        "the same call with the owner's tenant does change the row"
    );
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn the_gauges_count_unacknowledged_removals_and_age_the_oldest() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let none = unacknowledged_removals(test.database.pool())
        .await
        .expect("an empty outbox reads");
    assert_eq!((none.count, none.oldest_age_seconds), (0, 0.0));

    let old = insert_row(&test, REMOVAL_EVENT_TYPE, OWNER).await;
    sqlx::query(
        "update x_archive.outbox_events set created_at = now() - interval '90 seconds' \
          where id = $1",
    )
    .bind(old)
    .execute(test.database.pool())
    .await
    .expect("the age is set");
    let newer = insert_row(&test, REMOVAL_EVENT_TYPE, OWNER).await;
    insert_row(&test, "social.source.captured.v1", OWNER).await;

    let both = unacknowledged_removals(test.database.pool())
        .await
        .expect("the gauges read");
    assert_eq!(both.count, 2, "captured facts are not removals");
    assert!(
        (90.0..120.0).contains(&both.oldest_age_seconds),
        "the oldest removal is about 90 s old, got {}",
        both.oldest_age_seconds
    );

    acknowledge(test.database.pool(), old, OWNER, AckOutcome::Applied)
        .await
        .expect("the old removal is acknowledged");
    let one = unacknowledged_removals(test.database.pool())
        .await
        .expect("the gauges read");
    assert_eq!(one.count, 1);
    assert!(
        one.oldest_age_seconds < 90.0,
        "the older one is acknowledged"
    );
    let _ = newer;
    test.cleanup().await.expect("cleanup drops the database");
}
