//! The relay keeps a published removal and publishes it again until Knowledge acknowledges it
//! (XR-021 CONTRACTS.md R5-11 rule 4).

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use uuid::Uuid;
use x_capture::relay::{OutboxPublisher, OutboxRelay, PublishFailure};
use x_persistence::database::Database;
use x_persistence::outbox::{AckOutcome, ResendPolicy, acknowledge};
use x_persistence::test_support::TestDatabase;

const OWNER: &str = "user:018f0000-0000-7000-8000-000000000005";
const REMOVAL: &str = "social.source.removed.v1";

fn policy() -> ResendPolicy {
    ResendPolicy::new(Duration::from_mins(10), Duration::from_mins(50))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Call {
    subject: String,
    message_id: String,
    payload: Vec<u8>,
}

/// Records every publish and fails them while `refusing` is set.
#[derive(Default)]
struct Recorder {
    refusing: Mutex<bool>,
    calls: Mutex<Vec<Call>>,
}

impl Recorder {
    fn refuse(&self, refusing: bool) {
        *self.refusing.lock().expect("not poisoned") = refusing;
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().expect("not poisoned").clone()
    }
}

impl OutboxPublisher for Recorder {
    fn publish<'a>(
        &'a self,
        subject: &'a str,
        message_id: &'a str,
        payload: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<(), PublishFailure>> + Send + 'a>> {
        Box::pin(async move {
            if *self.refusing.lock().expect("not poisoned") {
                return Err(PublishFailure {
                    class: "ack_timeout",
                });
            }
            self.calls.lock().expect("not poisoned").push(Call {
                subject: subject.to_owned(),
                message_id: message_id.to_owned(),
                payload,
            });
            Ok(())
        })
    }
}

async fn insert_row(test: &TestDatabase, event_type: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into x_archive.outbox_events (id, aggregate, event_type, payload) \
         values ($1, $2, $3, $4::jsonb)",
    )
    .bind(id)
    .bind(format!("social_source:{id}"))
    .bind(event_type)
    .bind(
        serde_json::json!({"event_id": id, "event_type": event_type, "tenant_id": OWNER})
            .to_string(),
    )
    .execute(test.database.pool())
    .await
    .expect("the outbox row seeds");
    id
}

/// Makes the armed resend of `id` due now and clears any failure backoff.
async fn make_due(database: &Database, id: Uuid) {
    sqlx::query(
        "update x_archive.outbox_events \
            set next_resend_at = now() - interval '1 second', next_attempt_at = now() \
          where id = $1",
    )
    .bind(id)
    .execute(database.pool())
    .await
    .expect("the resend is made due");
}

#[derive(Debug, PartialEq, Eq)]
struct Ledger {
    published_at: String,
    resend_count: i32,
    next_resend_at: Option<String>,
    resend_in_future: Option<bool>,
    attempts: i32,
    retry_in_future: bool,
}

async fn ledger(database: &Database, id: Uuid) -> Ledger {
    let (published_at, resend_count, next_resend_at, resend_in_future, attempts, retry_in_future): (
        String,
        i32,
        Option<String>,
        Option<bool>,
        i32,
        bool,
    ) = sqlx::query_as(
        "select published_at::text, resend_count, next_resend_at::text, next_resend_at > now(), \
                attempt_count, next_attempt_at > now() \
           from x_archive.outbox_events where id = $1",
    )
    .bind(id)
    .fetch_one(database.pool())
    .await
    .expect("the row is readable");
    Ledger {
        published_at,
        resend_count,
        next_resend_at,
        resend_in_future,
        attempts,
        retry_in_future,
    }
}

#[tokio::test]
async fn an_unacknowledged_removal_is_published_again_unchanged_with_the_same_message_id() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let recorder = Arc::new(Recorder::default());
    let relay = OutboxRelay::new(test.database.clone(), recorder.clone(), policy());
    let id = insert_row(&test, REMOVAL).await;

    let first = relay.run_once().await.expect("the first pass");
    assert_eq!((first.published, first.resent), (1, 0));
    let armed = ledger(&test.database, id).await;
    assert_eq!(armed.resend_in_future, Some(true), "armed for later");
    assert_eq!(armed.resend_count, 0);

    let idle = relay.run_once().await.expect("a pass before the delay");
    assert_eq!(
        (idle.published, idle.resent),
        (0, 0),
        "nothing is resent before next_resend_at"
    );

    make_due(&test.database, id).await;
    let resend = relay.run_once().await.expect("the resend pass");
    assert_eq!((resend.published, resend.resent), (0, 1));

    let calls = recorder.calls();
    assert_eq!(calls.len(), 2, "one publish and one republish");
    assert_eq!(
        calls[0], calls[1],
        "same subject, same message id, same bytes"
    );
    assert_eq!(calls[1].message_id, id.to_string());
    assert_eq!(calls[1].subject, format!("evt.{REMOVAL}"));
    let after = ledger(&test.database, id).await;
    assert_eq!(
        after.published_at, armed.published_at,
        "published_at is not touched"
    );
    assert_eq!(after.resend_count, 1);
    assert_eq!(after.resend_in_future, Some(true));
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn an_acknowledgement_ends_the_resend_and_other_event_types_are_never_resent() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let recorder = Arc::new(Recorder::default());
    let relay = OutboxRelay::new(test.database.clone(), recorder.clone(), policy());
    let removal = insert_row(&test, REMOVAL).await;
    let captured = insert_row(&test, "social.source.captured.v1").await;
    let updated = insert_row(&test, "social.source.updated.v1").await;
    relay.run_once().await.expect("the first pass");
    assert_eq!(recorder.calls().len(), 3);
    for id in [removal, captured, updated] {
        make_due(&test.database, id).await;
    }
    let unanswered = relay.run_once().await.expect("the pass before the answer");
    assert_eq!(
        unanswered.resent, 1,
        "only the unacknowledged removal is published again"
    );
    assert_eq!(recorder.calls().len(), 4);

    acknowledge(test.database.pool(), removal, OWNER, AckOutcome::Applied)
        .await
        .expect("the acknowledgement is recorded");
    make_due(&test.database, removal).await;
    let pass = relay.run_once().await.expect("the pass after the answer");

    assert_eq!(pass.resent, 0, "nothing is left to resend");
    assert_eq!(recorder.calls().len(), 4, "no further publish");
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn a_refused_acknowledgement_also_ends_the_resend() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let recorder = Arc::new(Recorder::default());
    let relay = OutboxRelay::new(test.database.clone(), recorder.clone(), policy());
    let removal = insert_row(&test, REMOVAL).await;
    relay.run_once().await.expect("the first pass");
    make_due(&test.database, removal).await;
    let unanswered = relay.run_once().await.expect("the pass before the answer");
    assert_eq!(unanswered.resent, 1, "an unanswered removal is resent");

    acknowledge(test.database.pool(), removal, OWNER, AckOutcome::Refused)
        .await
        .expect("the refusal is recorded");
    make_due(&test.database, removal).await;
    let pass = relay.run_once().await.expect("the pass after the refusal");

    assert_eq!(pass.resent, 0);
    assert_eq!(recorder.calls().len(), 2);
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn a_failed_republish_backs_off_without_touching_the_resend_schedule_or_health() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let recorder = Arc::new(Recorder::default());
    let relay = OutboxRelay::new(test.database.clone(), recorder.clone(), policy());
    let id = insert_row(&test, REMOVAL).await;
    relay.run_once().await.expect("the first pass");
    make_due(&test.database, id).await;
    let before = ledger(&test.database, id).await;

    recorder.refuse(true);
    for _ in 0..4 {
        let pass = relay.run_once().await.expect("a refused pass completes");
        make_due_resend_only(&test.database, id).await;
        assert!(pass.failed <= 1);
    }

    let after = ledger(&test.database, id).await;
    assert_eq!(after.published_at, before.published_at);
    assert_eq!(after.resend_count, 0, "a failed republish is not counted");
    assert!(
        after.attempts >= 1,
        "the failure is recorded on the existing backoff"
    );
    assert!(
        relay.health().is_healthy(),
        "an unacknowledged removal never makes the relay unhealthy"
    );

    recorder.refuse(false);
    make_due(&test.database, id).await;
    let pass = relay.run_once().await.expect("the broker recovered");
    assert_eq!(pass.resent, 1);
    assert_eq!(ledger(&test.database, id).await.resend_count, 1);
    test.cleanup().await.expect("cleanup drops the database");
}

/// Keeps the removal due for resending but leaves the failure backoff in place.
async fn make_due_resend_only(database: &Database, id: Uuid) {
    sqlx::query(
        "update x_archive.outbox_events set next_resend_at = now() - interval '1 second' \
          where id = $1",
    )
    .bind(id)
    .execute(database.pool())
    .await
    .expect("the resend stays due");
}

#[tokio::test]
async fn a_failed_republish_is_not_attempted_again_before_its_backoff_ends() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let recorder = Arc::new(Recorder::default());
    let relay = OutboxRelay::new(test.database.clone(), recorder.clone(), policy());
    let id = insert_row(&test, REMOVAL).await;
    relay.run_once().await.expect("the first pass");
    make_due(&test.database, id).await;
    recorder.refuse(true);
    let failed = relay.run_once().await.expect("the refused pass");
    assert_eq!(failed.failed, 1);
    assert!(ledger(&test.database, id).await.retry_in_future);

    recorder.refuse(false);
    let skipped = relay.run_once().await.expect("the pass inside the backoff");

    assert_eq!(skipped.resent, 0, "the backoff holds the removal back");
    assert_eq!(
        recorder.calls().len(),
        1,
        "only the first publish got through"
    );
    test.cleanup().await.expect("cleanup drops the database");
}
