//! The durable budget gate: fixed per-account request windows charged in the owned
//! schema before any provider call happens.

use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};

use x_persistence::budget_windows;
use x_persistence::database::Database;

/// The source of the current instant for window targeting.
///
/// Every timestamp the gate derives flows through this seam, so tests pin time
/// instead of waiting on the wall clock.
pub trait Clock: Send + Sync {
    /// The current instant.
    fn now(&self) -> DateTime<Utc>;
}

/// The production [`Clock`], reading the system wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Why a reservation was not granted.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BudgetError {
    /// The current window's allowance is spent; the caller must wait for the reset
    /// instant before reserving again.
    #[error("the api budget for the current window is exhausted")]
    Exhausted {
        /// The instant the current window ends and the allowance restores.
        reset_at: DateTime<Utc>,
    },
    /// The requested cost alone can never fit inside one window.
    #[error("reservation cost {cost} exceeds the per-window cap {cap}")]
    CostExceedsCap {
        /// The refused cost.
        cost: u32,
        /// The configured cap it exceeds.
        cap: u32,
    },
    /// The gate was constructed with settings it cannot enforce.
    #[error("invalid budget gate configuration: {reason}")]
    InvalidConfiguration {
        /// What the constructor refused.
        reason: &'static str,
    },
    /// A database operation backing the gate failed.
    #[error(transparent)]
    Persistence(#[from] x_persistence::error::PersistenceError),
}

/// An accepted charge against one account's window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reservation {
    /// The window the cost was charged to; callers refund against this window when
    /// their provider call fails.
    pub window_start: DateTime<Utc>,
    /// The cost charged.
    pub cost: u32,
}

/// Gates provider calls behind durable per-account request windows.
#[derive(Clone)]
pub struct BudgetGate {
    database: Database,
    request_cap_per_window: u32,
    window_seconds: i32,
    reset_offset: TimeDelta,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for BudgetGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BudgetGate")
            .field("request_cap_per_window", &self.request_cap_per_window)
            .field("window_seconds", &self.window_seconds)
            .finish_non_exhaustive()
    }
}

impl BudgetGate {
    /// Builds a gate over `database` that charges at most `request_cap_per_window`
    /// units inside windows of `window_seconds` length, reading time from the system
    /// clock.
    ///
    /// # Errors
    /// When `window_seconds` is not a positive number of seconds small enough for the
    /// owned schema's integer column.
    pub fn new(
        database: Database,
        request_cap_per_window: u32,
        window_seconds: i64,
    ) -> Result<Self, BudgetError> {
        Self::with_clock(
            database,
            request_cap_per_window,
            window_seconds,
            Arc::new(SystemClock),
        )
    }

    /// Builds the same gate as [`BudgetGate::new`] but reads time from the injected
    /// [`Clock`].
    ///
    /// # Errors
    /// Same conditions as [`BudgetGate::new`].
    pub fn with_clock(
        database: Database,
        request_cap_per_window: u32,
        window_seconds: i64,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, BudgetError> {
        if window_seconds < 1 || window_seconds > i64::from(i32::MAX) {
            return Err(BudgetError::InvalidConfiguration {
                reason: "window_seconds must be between 1 and 2147483647",
            });
        }
        let reset_offset =
            TimeDelta::try_seconds(window_seconds).ok_or(BudgetError::InvalidConfiguration {
                reason: "window_seconds is too large to add to a timestamp",
            })?;
        Ok(Self {
            database,
            request_cap_per_window,
            window_seconds: i32::try_from(window_seconds).unwrap_or(i32::MAX),
            reset_offset,
            clock,
        })
    }

    /// Reserves `cost` request units for `account` inside the account's current
    /// window.
    ///
    /// # Errors
    /// [`BudgetError::CostExceedsCap`] when the cost alone exceeds the cap,
    /// [`BudgetError::Exhausted`] carrying the reset instant when the window cannot
    /// admit the cost, and [`BudgetError::Persistence`] when the database fails.
    pub async fn reserve(
        &self,
        account: uuid::Uuid,
        cost: u32,
    ) -> Result<Reservation, BudgetError> {
        if cost > self.request_cap_per_window {
            return Err(BudgetError::CostExceedsCap {
                cost,
                cap: self.request_cap_per_window,
            });
        }
        let now = self.clock.now();
        let window_start = window_start_for(now, i64::from(self.window_seconds));
        match budget_windows::charge(
            self.database.pool(),
            account,
            window_start,
            i64::from(self.window_seconds),
            i64::from(self.request_cap_per_window),
            i64::from(cost),
        )
        .await?
        {
            budget_windows::WindowCharge::Accepted { .. } => Ok(Reservation { window_start, cost }),
            budget_windows::WindowCharge::Exhausted => Err(BudgetError::Exhausted {
                reset_at: window_start.checked_add_signed(self.reset_offset).ok_or(
                    BudgetError::InvalidConfiguration {
                        reason: "window_seconds is too large for the window's reset instant",
                    },
                )?,
            }),
        }
    }

    /// Releases cost previously charged by a reservation whose provider call failed.
    ///
    /// Refunding more than the window currently carries clamps at zero usage instead
    /// of driving it negative.
    ///
    /// # Errors
    /// [`BudgetError::Persistence`] when the database fails.
    pub async fn refund(
        &self,
        account: uuid::Uuid,
        window_start: DateTime<Utc>,
        cost: u32,
    ) -> Result<(), BudgetError> {
        let bounded_cost = i32::try_from(cost).unwrap_or(i32::MAX);
        budget_windows::refund(
            self.database.pool(),
            account,
            window_start,
            i64::from(bounded_cost),
        )
        .await?;
        Ok(())
    }
}

/// Aligns `now` down to the start of its window of `window_seconds` length.
///
/// Falls back to the instant itself when the aligned start is unrepresentable, which
/// cannot happen for validated positive window lengths.
fn window_start_for(now: DateTime<Utc>, window_seconds: i64) -> DateTime<Utc> {
    let epoch_seconds = now.timestamp();
    let start = epoch_seconds.div_euclid(window_seconds) * window_seconds;
    DateTime::from_timestamp(start, 0).unwrap_or(now)
}
