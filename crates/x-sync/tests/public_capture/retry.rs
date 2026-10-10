//! Retry, exhaustion, racing workers and atomicity of the public capture worker.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ratatoskr_operation_contracts::OperationStatus;
use x_persistence::test_support::TestDatabase;
use x_sync::{
    CapturePolicy, PublicCaptureWorker, PublicPost, PublicPostFailure, PublicPostResolver,
};

use crate::support::*;

#[tokio::test]
async fn transient_failures_back_off_30s_2min_8min_30min_and_then_succeed() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let capture = seed_capture(&test, 1, uuid(9, 1)).await;
    let resolver = ScriptedResolver::new([
        Err(PublicPostFailure::Transient),
        Err(PublicPostFailure::Transient),
        Err(PublicPostFailure::Transient),
        Err(PublicPostFailure::Transient),
        Ok(post("Finally.")),
    ]);
    let clock = TestClock::at(START);
    let worker = worker(&test, &resolver, &clock);

    for (attempt, delay) in [(1_i32, 30_i64), (2, 120), (3, 480), (4, 1800)] {
        let summary = worker.run_due_once().await.expect("the pass completes");
        assert_eq!(
            (summary.claimed, summary.deferred),
            (1, 1),
            "attempt {attempt}"
        );
        let state = capture_state(&test, capture.capture_id).await;
        assert_eq!(state.0, "accepted");
        assert_eq!(state.1, attempt);
        assert!(!state.3, "no report while retries remain");
        assert_eq!(
            state.2,
            clock.current() + chrono::Duration::seconds(delay),
            "attempt {attempt} backs off {delay}s"
        );
        if attempt == 1 {
            clock.advance(29);
            let early = worker
                .run_due_once()
                .await
                .expect("the early pass completes");
            assert_eq!(
                early.claimed, 0,
                "a capture is not retried before it is due"
            );
            assert_eq!(resolver.calls(), 1);
        }
        clock.set(state.2);
    }
    assert_eq!(
        reports(&test).await.len(),
        0,
        "no terminal report before the outcome is final"
    );

    let summary = worker
        .run_due_once()
        .await
        .expect("the final pass completes");
    assert_eq!(summary.preserved, 1);
    let reported = reports(&test).await;
    assert_eq!(reported.len(), 1);
    assert_eq!(
        reported.first().expect("the element exists").1.status,
        OperationStatus::Succeeded
    );
    assert_eq!(resolver.calls(), 5);
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn the_fifth_transient_failure_is_a_retryable_terminal_report() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let capture = seed_capture(&test, 1, uuid(9, 1)).await;
    let resolver = ScriptedResolver::new((0..5).map(|_| Err(PublicPostFailure::Transient)));
    let clock = TestClock::at(START);
    let worker = worker(&test, &resolver, &clock);

    for _ in 0..4 {
        worker.run_due_once().await.expect("a deferred pass");
        let state = capture_state(&test, capture.capture_id).await;
        clock.set(state.2);
    }
    assert_eq!(reports(&test).await.len(), 0);
    let summary = worker.run_due_once().await.expect("the fifth pass");
    assert_eq!((summary.deferred, summary.unavailable), (0, 1));

    let reported = reports(&test).await;
    assert_eq!(reported.len(), 1);
    assert_eq!(
        reported.first().expect("the element exists").1.status,
        OperationStatus::Failed
    );
    let error = reported
        .first()
        .expect("the element exists")
        .1
        .error
        .clone()
        .expect("an error");
    assert_eq!(error.code.as_str(), "social.source.unavailable");
    assert!(error.retryable, "the user may try again later");
    let state = capture_state(&test, capture.capture_id).await;
    assert_eq!(
        (state.0.as_str(), state.1, state.3),
        ("unavailable", 5, true)
    );
    let again = worker.run_due_once().await.expect("nothing is left");
    assert_eq!(again.claimed, 0);
    assert_eq!(resolver.calls(), 5);
    test.cleanup().await.expect("cleanup drops the database");
}

/// Parks the first caller inside `resolve` until released.
struct BlockingResolver {
    calls: AtomicUsize,
    release: tokio::sync::Notify,
}

impl PublicPostResolver for BlockingResolver {
    fn resolve<'a>(
        &'a self,
        _provider_post_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<PublicPost, PublicPostFailure>> + Send + 'a>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            self.release.notified().await;
            Ok(post("Raced."))
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_workers_racing_on_one_capture_resolve_and_report_it_once() {
    let test = TestDatabase::create().await.expect("a disposable database");
    seed_capture(&test, 1, uuid(9, 1)).await;
    let resolver = Arc::new(BlockingResolver {
        calls: AtomicUsize::new(0),
        release: tokio::sync::Notify::new(),
    });
    let clock = TestClock::at(START);
    let build = || {
        PublicCaptureWorker::new(
            test.database.clone(),
            Arc::<BlockingResolver>::clone(&resolver),
            Arc::<TestClock>::clone(&clock),
            CapturePolicy::new(5, 8),
        )
    };
    let first = build();
    let holder = tokio::spawn(async move { first.run_due_once().await });
    tokio::time::timeout(Duration::from_secs(10), async {
        while resolver.calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the first worker claims the capture");

    let second = build()
        .run_due_once()
        .await
        .expect("the second worker completes");
    assert_eq!(
        second.claimed, 0,
        "a leased capture is invisible to other workers"
    );

    resolver.release.notify_one();
    let first = holder
        .await
        .expect("the first worker does not panic")
        .expect("the first worker completes");
    assert_eq!(first.preserved, 1);
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    assert_eq!(reports(&test).await.len(), 1);
    assert_eq!(envelopes(&test, "social.source.captured.v1").await.len(), 1);
    test.cleanup().await.expect("cleanup drops the database");
}

#[tokio::test]
async fn a_failed_report_rolls_back_the_source_and_the_event_and_the_work_resumes() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let capture = seed_capture(&test, 1, uuid(9, 1)).await;
    sqlx::raw_sql(
        "create function x_archive.refuse_reports() returns trigger language plpgsql as $$ \
         begin \
           if new.event_type = 'platform.operation.reported.v1' then \
             raise exception 'refused for the test'; \
           end if; \
           return new; \
         end $$; \
         create trigger refuse_reports before insert on x_archive.outbox_events \
         for each row execute function x_archive.refuse_reports();",
    )
    .execute(test.database.pool())
    .await
    .expect("the failure trigger installs");
    let resolver = ScriptedResolver::new([Ok(post("Once.")), Ok(post("Once."))]);
    let clock = TestClock::at(START);
    let worker = worker(&test, &resolver, &clock);

    worker
        .run_due_once()
        .await
        .expect_err("the report insert fails, so the pass fails");
    assert_eq!(
        count(&test, "select count(*) from x_archive.explicit_sources").await,
        0,
        "the source rolled back with the report"
    );
    assert_eq!(
        count(&test, "select count(*) from x_archive.outbox_events").await,
        0,
        "no social event without its report"
    );
    let state = capture_state(&test, capture.capture_id).await;
    assert_eq!((state.0.as_str(), state.3), ("accepted", false));

    sqlx::raw_sql("drop trigger refuse_reports on x_archive.outbox_events")
        .execute(test.database.pool())
        .await
        .expect("the trigger drops");
    clock.advance(121);
    let summary = worker
        .run_due_once()
        .await
        .expect("the work resumes after the lease");
    assert_eq!(summary.preserved, 1);
    assert_eq!(reports(&test).await.len(), 1);
    assert_eq!(resolver.calls(), 2);
    test.cleanup().await.expect("cleanup drops the database");
}
