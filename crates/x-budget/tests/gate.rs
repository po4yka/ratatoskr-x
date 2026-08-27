//! Budget-gate integration tests against a disposable `x_archive` database: every
//! timestamp flows through an injected fake clock, so no assertion depends on the
//! wall clock.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeDelta, Utc};
use x_budget::gate::{BudgetClass, BudgetError, BudgetGate, Clock};
use x_persistence::budget_windows::window_usage;
use x_persistence::database::Database;
use x_persistence::test_support::TestDatabase;

/// The fixed instant every test starts from; deliberately not aligned to any
/// 900-second boundary.
const BASE_INSTANT: &str = "2026-08-25T00:07:23Z";

/// The window length every test configures its gate with.
const WINDOW_SECONDS: i64 = 900;

/// A mutable fake clock the tests advance by hand.
struct FakeClock {
    current: Mutex<DateTime<Utc>>,
}

impl FakeClock {
    fn at_base_instant() -> Self {
        Self {
            current: Mutex::new(base_instant()),
        }
    }

    fn advance_seconds(&self, seconds: i64) {
        let mut guard = self.current.lock().expect("clock mutex is not poisoned");
        *guard += TimeDelta::try_seconds(seconds).expect("test advances by a small step");
    }

    fn instant(&self) -> DateTime<Utc> {
        *self.current.lock().expect("clock mutex is not poisoned")
    }
}

impl Clock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        self.instant()
    }
}

/// Parses the suite's fixed base instant.
fn base_instant() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(BASE_INSTANT)
        .expect("the suite base instant parses")
        .with_timezone(&Utc)
}

/// Computes the expected aligned window start for `from`, independently of the
/// gate's own implementation.
fn expected_window_start(from: DateTime<Utc>) -> DateTime<Utc> {
    let remainder = from.timestamp().rem_euclid(WINDOW_SECONDS);
    from - TimeDelta::try_seconds(remainder).expect("the remainder is a small step")
}

/// Seeds one account row and returns its generated identifier.
async fn seed_account(database: &Database, provider_user_id: &str) -> uuid::Uuid {
    let account = uuid::Uuid::now_v7();
    database
        .query_raw(&format!(
            "insert into x_archive.accounts (id, provider_user_id) \
             values ('{account}', '{provider_user_id}')"
        ))
        .await
        .expect("the account row seeds");
    account
}

/// Builds a gate over `database` with the suite's window length and the fake clock.
fn gate_with_fake_clock(database: Database, clock: &Arc<FakeClock>, cap: u32) -> BudgetGate {
    BudgetGate::with_clock(
        database,
        BudgetClass::Read,
        cap,
        WINDOW_SECONDS,
        Arc::clone(clock) as Arc<dyn Clock>,
    )
    .expect("the gate constructs from valid settings")
}

#[tokio::test]
async fn bookmark_write_budget_is_isolated_and_inspection_is_non_consuming() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = seed_account(&test.database, "budget-gate-write-isolation").await;
    let clock = Arc::new(FakeClock::at_base_instant());
    let read = BudgetGate::with_clock(
        test.database.clone(),
        BudgetClass::Read,
        10,
        WINDOW_SECONDS,
        Arc::clone(&clock) as Arc<dyn Clock>,
    )
    .expect("the read gate constructs");
    let write = BudgetGate::with_clock(
        test.database.clone(),
        BudgetClass::BookmarkWrite,
        10,
        WINDOW_SECONDS,
        Arc::clone(&clock) as Arc<dyn Clock>,
    )
    .expect("the bookmark-write gate constructs");

    read.reserve(account, 7)
        .await
        .expect("read usage fits its class");
    write
        .reserve(account, 3)
        .await
        .expect("write usage fits its independent class");
    let read_follow_up = read.reserve(account, 3).await;
    let over_cap_write_inspection = write
        .inspect(account, 8)
        .await
        .expect("write eligibility can be inspected");
    let inspect_account = seed_account(&test.database, "budget-gate-inspection-only").await;
    let before_inspect = window_usage(
        test.database.pool(),
        inspect_account,
        BudgetClass::BookmarkWrite.as_str(),
        expected_window_start(base_instant()),
    )
    .await
    .expect("absence of an inspection-only window is readable");
    let inspection = write
        .inspect(inspect_account, 1)
        .await
        .expect("inspection returns an admission result");
    let after_inspect = window_usage(
        test.database.pool(),
        inspect_account,
        BudgetClass::BookmarkWrite.as_str(),
        expected_window_start(base_instant()),
    )
    .await
    .expect("absence remains readable after inspection");

    test.cleanup().await.expect("cleanup drops the database");

    assert_eq!(
        before_inspect, None,
        "an unused account has no durable window before inspection"
    );
    assert_eq!(
        after_inspect, before_inspect,
        "inspection neither creates nor changes durable usage"
    );
    assert!(
        read_follow_up.is_ok(),
        "bookmark-write usage cannot spend the read allowance: {read_follow_up:?}"
    );
    assert!(
        !over_cap_write_inspection.eligible,
        "cost above the write allowance's remainder is ineligible"
    );
    assert!(inspection.eligible, "an unused write allowance is eligible");
    assert_eq!(
        inspection.reset_at,
        expected_window_start(base_instant())
            + TimeDelta::try_seconds(WINDOW_SECONDS).expect("a small window"),
        "inspection reports the current class window reset"
    );
}

#[tokio::test]
async fn reservation_within_cap_counts_durably_across_instances() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = seed_account(&test.database, "budget-gate-within-cap").await;
    let clock = Arc::new(FakeClock::at_base_instant());
    let first = gate_with_fake_clock(test.database.clone(), &clock, 10);

    let initial = first
        .reserve(account, 4)
        .await
        .expect("an under-cap reservation is accepted");

    let second = gate_with_fake_clock(test.database.clone(), &clock, 10);
    let follow_up = second
        .reserve(account, 4)
        .await
        .expect("an independently constructed gate continues charging the same window");
    let to_the_cap = second
        .reserve(account, 2)
        .await
        .expect("reservations stay accepted until the cap is reached");
    let persisted = window_usage(
        test.database.pool(),
        account,
        BudgetClass::Read.as_str(),
        initial.window_start,
    )
    .await
    .expect("the usage query succeeds");

    test.cleanup().await.expect("cleanup drops the database");

    assert_eq!(
        initial.window_start,
        expected_window_start(base_instant()),
        "reservations land in the window computed from the clock"
    );
    assert_eq!(
        follow_up.window_start, initial.window_start,
        "an independent gate targets the same window"
    );
    assert_eq!(
        to_the_cap.window_start, initial.window_start,
        "charging up to the cap stays inside one window"
    );
    assert_eq!(
        persisted,
        Some(10),
        "accepted reservations accumulate durably in the owned schema"
    );
}

#[tokio::test]
async fn over_cap_reservation_is_blocked_before_any_call_and_charges_nothing() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = seed_account(&test.database, "budget-gate-over-cap").await;
    let clock = Arc::new(FakeClock::at_base_instant());
    let gate = gate_with_fake_clock(test.database.clone(), &clock, 10);

    let charged = gate
        .reserve(account, 7)
        .await
        .expect("the first reservation fits under the cap");
    let blocked = gate.reserve(account, 5).await;
    let persisted = window_usage(
        test.database.pool(),
        account,
        BudgetClass::Read.as_str(),
        charged.window_start,
    )
    .await
    .expect("the usage query succeeds");

    test.cleanup().await.expect("cleanup drops the database");

    let reset_at = match &blocked {
        Err(BudgetError::Exhausted { reset_at }) => *reset_at,
        other => panic!(
            "an over-cap reservation must be refused as exhausted with a reset instant, got {other:?}"
        ),
    };
    assert_eq!(
        reset_at,
        charged.window_start + TimeDelta::try_seconds(WINDOW_SECONDS).expect("a small window"),
        "the reset instant names the end of the current window"
    );
    assert_eq!(
        persisted,
        Some(7),
        "a refused reservation charges nothing to the persisted usage"
    );
}

#[tokio::test]
async fn expired_window_opens_fresh_zeroed_window() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = seed_account(&test.database, "budget-gate-rollover").await;
    let clock = Arc::new(FakeClock::at_base_instant());
    let gate = gate_with_fake_clock(test.database.clone(), &clock, 10);

    let superseded = gate
        .reserve(account, 6)
        .await
        .expect("the first window admits the reservation");
    clock.advance_seconds(WINDOW_SECONDS);
    let fresh = gate
        .reserve(account, 10)
        .await
        .expect("a fresh window evaluates against the full cap");
    let superseded_usage = window_usage(
        test.database.pool(),
        account,
        BudgetClass::Read.as_str(),
        superseded.window_start,
    )
    .await
    .expect("the superseded window is readable");
    let fresh_usage = window_usage(
        test.database.pool(),
        account,
        BudgetClass::Read.as_str(),
        fresh.window_start,
    )
    .await
    .expect("the fresh window is readable");

    test.cleanup().await.expect("cleanup drops the database");

    assert_eq!(
        fresh.window_start,
        expected_window_start(clock.instant()),
        "the reservation lands in a new window starting at the computed boundary"
    );
    assert_ne!(
        fresh.window_start, superseded.window_start,
        "an expired window must not keep being charged"
    );
    assert_eq!(
        superseded_usage,
        Some(6),
        "the superseded window's usage stays readable unchanged"
    );
    assert_eq!(
        fresh_usage,
        Some(10),
        "the fresh window starts from zeroed usage"
    );
}

#[tokio::test]
async fn racing_reservations_never_exceed_the_cap() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = seed_account(&test.database, "budget-gate-racing").await;
    let clock = Arc::new(FakeClock::at_base_instant());
    let gate = gate_with_fake_clock(test.database.clone(), &clock, 10);

    let seeded = gate
        .reserve(account, 4)
        .await
        .expect("the first seed charge fits");
    gate.reserve(account, 3)
        .await
        .expect("the second seed charge fits");

    let mut racers = tokio::task::JoinSet::new();
    for _ in 0..10 {
        let racer = gate.clone();
        racers.spawn(async move { racer.reserve(account, 1).await });
    }
    let mut outcomes = Vec::new();
    while let Some(joined) = racers.join_next().await {
        outcomes.push(joined.expect("no racer task panics"));
    }
    let persisted = window_usage(
        test.database.pool(),
        account,
        BudgetClass::Read.as_str(),
        seeded.window_start,
    )
    .await
    .expect("the usage query succeeds");

    test.cleanup().await.expect("cleanup drops the database");

    let accepted_costs: Vec<u32> = outcomes
        .iter()
        .filter_map(|outcome| outcome.as_ref().ok().map(|reservation| reservation.cost))
        .collect();
    let exhausted = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, Err(BudgetError::Exhausted { .. })))
        .count();
    assert_eq!(
        accepted_costs.iter().sum::<u32>(),
        3,
        "exactly the remaining allowance is accepted in total, got {accepted_costs:?}"
    );
    assert_eq!(
        accepted_costs.len(),
        3,
        "exactly three single-cost reservations are accepted"
    );
    assert_eq!(
        exhausted, 7,
        "every refused racer sees the exhausted outcome, got {outcomes:?}"
    );
    assert_eq!(
        persisted,
        Some(10),
        "persisted usage equals the seeded charges plus the accepted racer sum"
    );
}

#[tokio::test]
async fn refund_releases_failed_cost_without_driving_usage_negative() {
    let test = TestDatabase::create().await.expect("a disposable database");
    let account = seed_account(&test.database, "budget-gate-refund").await;
    let clock = Arc::new(FakeClock::at_base_instant());
    let gate = gate_with_fake_clock(test.database.clone(), &clock, 10);

    let charged = gate
        .reserve(account, 6)
        .await
        .expect("the reservation fits under the cap");
    gate.refund(account, charged.window_start, 4)
        .await
        .expect("a partial refund succeeds");
    let after_partial = window_usage(
        test.database.pool(),
        account,
        BudgetClass::Read.as_str(),
        charged.window_start,
    )
    .await
    .expect("the usage query succeeds");
    gate.refund(account, charged.window_start, 5)
        .await
        .expect("an over-large refund still succeeds");
    let after_over_refund = window_usage(
        test.database.pool(),
        account,
        BudgetClass::Read.as_str(),
        charged.window_start,
    )
    .await
    .expect("the usage query succeeds again");

    test.cleanup().await.expect("cleanup drops the database");

    assert_eq!(
        after_partial,
        Some(2),
        "refunding part of a reservation lowers usage by exactly that amount"
    );
    assert_eq!(
        after_over_refund,
        Some(0),
        "refunding more than charged clamps at zero rather than going negative"
    );
}
