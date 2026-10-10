//! Shared fixtures of the public capture tests.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use ratatoskr_event_envelope::EventEnvelope;
use ratatoskr_operation_contracts::{OperationReported, OperationStatus};
use ratatoskr_social_contracts::SocialSourceCaptured;
use uuid::Uuid;
use x_budget::gate::Clock;
use x_persistence::test_support::TestDatabase;
use x_sync::{
    CapturePolicy, PublicAuthor, PublicCaptureWorker, PublicPost, PublicPostFailure,
    PublicPostResolver,
};

pub(crate) const POST: &str = "1900000000000000001";
pub(crate) const START: &str = "2026-08-27T12:00:00Z";

#[derive(Debug)]
pub(crate) struct TestClock(Mutex<DateTime<Utc>>);

impl TestClock {
    pub(crate) fn at(instant: &str) -> Arc<Self> {
        Arc::new(Self(Mutex::new(
            instant.parse().expect("the instant parses"),
        )))
    }

    pub(crate) fn set(&self, instant: DateTime<Utc>) {
        *self.0.lock().expect("the clock is not poisoned") = instant;
    }

    pub(crate) fn advance(&self, seconds: i64) {
        let mut now = self.0.lock().expect("the clock is not poisoned");
        *now += chrono::Duration::seconds(seconds);
    }

    pub(crate) fn current(&self) -> DateTime<Utc> {
        *self.0.lock().expect("the clock is not poisoned")
    }
}

impl Clock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        self.current()
    }
}

/// Answers each call with the next scripted outcome and counts the calls.
pub(crate) struct ScriptedResolver {
    script: Mutex<VecDeque<Result<PublicPost, PublicPostFailure>>>,
    calls: AtomicUsize,
}

impl ScriptedResolver {
    pub(crate) fn new(
        script: impl IntoIterator<Item = Result<PublicPost, PublicPostFailure>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(script.into_iter().collect()),
            calls: AtomicUsize::new(0),
        })
    }

    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl PublicPostResolver for ScriptedResolver {
    fn resolve<'a>(
        &'a self,
        _provider_post_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<PublicPost, PublicPostFailure>> + Send + 'a>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let next = self
            .script
            .lock()
            .expect("the script is not poisoned")
            .pop_front()
            .expect("the test scripted an outcome for this call");
        Box::pin(async move { next })
    }
}

pub(crate) fn post(text: &str) -> PublicPost {
    PublicPost {
        provider_id: POST.to_owned(),
        author: PublicAuthor {
            id: "42".to_owned(),
            username: Some("ada".to_owned()),
            name: Some("Ada Example".to_owned()),
        },
        text: text.to_owned(),
        long_text: None,
        published_at: Some(
            "2026-08-16T09:30:00.120Z"
                .parse()
                .expect("the publication time parses"),
        ),
        expanded_urls: Vec::new(),
    }
}

pub(crate) fn worker(
    test: &TestDatabase,
    resolver: &Arc<ScriptedResolver>,
    clock: &Arc<TestClock>,
) -> PublicCaptureWorker {
    PublicCaptureWorker::new(
        test.database.clone(),
        Arc::<ScriptedResolver>::clone(resolver),
        Arc::<TestClock>::clone(clock),
        CapturePolicy::new(5, 8),
    )
}

pub(crate) fn uuid(group: u32, n: u32) -> Uuid {
    Uuid::parse_str(&format!("018f0000-0000-7000-8{group:03x}-{n:012x}")).expect("a UUID")
}

/// One queued capture: `n` distinguishes captures, `owner` is the tenant.
#[derive(Clone, Copy)]
pub(crate) struct Seeded {
    pub(crate) capture_id: Uuid,
    pub(crate) command_id: Uuid,
    pub(crate) operation_id: Uuid,
    pub(crate) owner: Uuid,
}

pub(crate) async fn seed_capture(test: &TestDatabase, n: u32, owner: Uuid) -> Seeded {
    let seeded = Seeded {
        capture_id: uuid(1, n),
        command_id: uuid(2, n),
        operation_id: uuid(3, n),
        owner,
    };
    sqlx::query(
        "insert into x_archive.explicit_captures \
           (capture_id, command_id, operation_id, owner, provider_post_id, original_permalink, \
            captured_at, acquisition, saved_authority, status, next_attempt_at) \
         values ($1, $2, $3, $4, $5, $6, '2026-08-27T09:30:00Z', 'browser_extension', \
                 'explicit_user_capture', 'accepted', $7)",
    )
    .bind(seeded.capture_id)
    .bind(seeded.command_id.to_string())
    .bind(seeded.operation_id)
    .bind(owner)
    .bind(POST)
    .bind(format!("https://x.com/ada/status/{POST}"))
    .bind(START.parse::<DateTime<Utc>>().expect("the start parses"))
    .execute(test.database.pool())
    .await
    .expect("the capture seeds");
    seeded
}

pub(crate) async fn envelopes(test: &TestDatabase, event_type: &str) -> Vec<EventEnvelope> {
    let payloads: Vec<serde_json::Value> = sqlx::query_scalar(
        "select payload from x_archive.outbox_events where event_type = $1 \
         order by created_at, id",
    )
    .bind(event_type)
    .fetch_all(test.database.pool())
    .await
    .expect("the outbox is readable");
    payloads
        .iter()
        .map(|payload| {
            EventEnvelope::from_json(payload.to_string().as_bytes())
                .expect("every outbox row is a complete event envelope")
        })
        .collect()
}

pub(crate) async fn reports(test: &TestDatabase) -> Vec<(EventEnvelope, OperationReported)> {
    envelopes(test, "platform.operation.reported.v1")
        .await
        .into_iter()
        .map(|envelope| {
            let report = envelope.payload_as().expect("an operation report");
            (envelope, report)
        })
        .collect()
}

pub(crate) async fn count(test: &TestDatabase, sql: &str) -> i64 {
    sqlx::query_scalar(sql)
        .fetch_one(test.database.pool())
        .await
        .expect("the count is readable")
}

pub(crate) async fn capture_state(
    test: &TestDatabase,
    capture: Uuid,
) -> (String, i32, DateTime<Utc>, bool) {
    sqlx::query_as(
        "select status, attempts, next_attempt_at, reported_at is not null \
         from x_archive.explicit_captures where capture_id = $1",
    )
    .bind(capture)
    .fetch_one(test.database.pool())
    .await
    .expect("the capture is readable")
}

pub(crate) fn field(value: &serde_json::Value, name: &str) -> serde_json::Value {
    value.get(name).expect("the field exists").clone()
}

pub(crate) async fn assert_captured_fact(test: &TestDatabase, owner: Uuid, source_id: Uuid) {
    let captured = envelopes(test, "social.source.captured.v1").await;
    let [fact] = captured.as_slice() else {
        panic!("exactly one captured fact, got {}", captured.len());
    };
    assert_eq!(fact.producer.as_str(), "ratatoskr-x");
    assert_eq!(
        fact.tenant_id.map(|tenant| tenant.to_string()),
        Some(format!("user:{owner}"))
    );
    assert_eq!(
        fact.aggregate_id.to_wire(),
        format!("social_source:{source_id}")
    );
    let event: SocialSourceCaptured = fact.payload_as().expect("a captured payload");
    let snapshot = serde_json::to_value(event.source).expect("the snapshot serializes");
    assert_eq!(field(&snapshot, "acquisition"), "browser_extension");
    assert_eq!(field(&snapshot, "saved_authority"), "explicit_user_capture");
    assert_eq!(field(&snapshot, "social_source_id"), source_id.to_string());
    assert_eq!(field(&snapshot, "owner"), format!("user:{owner}"));
    assert_eq!(field(&snapshot, "published_at"), "2026-08-16T09:30:00.12Z");
}

pub(crate) async fn assert_success_report(test: &TestDatabase, capture: &Seeded, source_id: Uuid) {
    let reported = reports(test).await;
    let [(envelope, report)] = reported.as_slice() else {
        panic!("exactly one terminal report, got {}", reported.len());
    };
    assert_eq!(report.status, OperationStatus::Succeeded);
    assert_eq!(report.operation_id.0, capture.operation_id);
    let [result] = report.results.as_slice() else {
        panic!("exactly one result");
    };
    assert_eq!(result.result_kind.as_str(), "social.post");
    assert_eq!(
        result.target.to_wire(),
        format!("social_source:{source_id}")
    );
    assert_eq!(
        envelope.tenant_id.map(|tenant| tenant.to_string()),
        Some(format!("user:{}", capture.owner))
    );
    assert_eq!(
        envelope.aggregate_id.to_wire(),
        format!("capture:{}", capture.capture_id)
    );
    assert_eq!(
        envelope
            .causation_id
            .as_ref()
            .map(ratatoskr_identifiers::EntityRef::to_wire),
        Some(format!("command:{}", capture.command_id))
    );
}
