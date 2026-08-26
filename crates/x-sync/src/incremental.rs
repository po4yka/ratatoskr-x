//! Safe, bounded incremental bookmark observations.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::types::Uuid;

use x_budget::gate::{BudgetError, BudgetGate, Clock};
use x_persistence::database::Database;

use crate::{
    BookmarkPage, BookmarkPageSource, BookmarkSourceError, SnapshotError, observed_at,
    persist_media, persist_posts, persist_relations, persist_users,
};

/// The terminal result of one incremental bookmark scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncrementalOutcome {
    /// The scan reached its prior watermark and persisted a newer one.
    Completed {
        /// The durable incremental run.
        run_id: Uuid,
        /// The newest provider post identity observed by the completed scan.
        watermark_provider_post_id: Option<String>,
    },
    /// A bounded scan could not reach its prior watermark and needs full authority.
    RequiresFullSnapshot {
        /// The non-authoritative incremental run that detected the gap.
        run_id: Uuid,
    },
    /// The scan stopped without reaching the watermark and remains non-authoritative.
    Incomplete {
        /// The stopped incremental run.
        run_id: Uuid,
    },
}

/// The locally selected operation for a platform-scheduled bookmark command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduledScan {
    /// Start an observation-only bounded scan.
    Incremental,
    /// Start a complete authoritative snapshot.
    FullSnapshot,
}

/// The command type carried by the platform scheduler before it adds `cmd.`.
pub const SCHEDULED_BOOKMARK_SCAN_COMMAND: &str = "x.bookmarks.scan_requested.v1";

/// Coordinates bounded, observation-only bookmark scans.
#[derive(Clone)]
pub struct IncrementalScanService {
    database: Database,
    budget: BudgetGate,
    clock: Arc<dyn Clock>,
    page_cap: u32,
    request_cap: u32,
}

impl std::fmt::Debug for IncrementalScanService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IncrementalScanService")
            .finish_non_exhaustive()
    }
}

impl IncrementalScanService {
    /// Builds a bounded incremental scan service.
    #[must_use]
    pub fn new(
        database: Database,
        budget: BudgetGate,
        clock: Arc<dyn Clock>,
        page_cap: u32,
        request_cap: u32,
    ) -> Self {
        Self {
            database,
            budget,
            clock,
            page_cap,
            request_cap,
        }
    }

    /// Selects safe work for one platform-scheduler command.
    ///
    /// # Errors
    /// Returns an error for a command outside the platform command grammar.
    pub async fn scheduled_scan(
        &self,
        command_type: &str,
        account_id: Uuid,
    ) -> Result<ScheduledScan, SnapshotError> {
        if command_type != SCHEDULED_BOOKMARK_SCAN_COMMAND {
            return Err(SnapshotError::RunUnavailable);
        }
        let (_, requires_full_snapshot) = self.load_state(account_id).await?;
        Ok(if requires_full_snapshot {
            ScheduledScan::FullSnapshot
        } else {
            ScheduledScan::Incremental
        })
    }

    /// Runs one bounded incremental scan.
    ///
    /// # Errors
    /// A production implementation can return persistence or source errors.
    pub async fn run<S>(
        &self,
        account_id: Uuid,
        source: &S,
    ) -> Result<IncrementalOutcome, SnapshotError>
    where
        S: BookmarkPageSource,
    {
        let (watermark, requires_full_snapshot) = self.load_state(account_id).await?;
        if requires_full_snapshot {
            return Err(SnapshotError::RunUnavailable);
        }
        let run_id = self.create_run(account_id).await?;
        let mut continuation = None;
        let mut newest_provider_post_id = None;
        let mut pages_fetched = 0;
        let mut requests_made = 0;

        loop {
            if pages_fetched >= self.page_cap {
                self.mark_gap(account_id, run_id).await?;
                return Ok(IncrementalOutcome::RequiresFullSnapshot { run_id });
            }
            if requests_made >= self.request_cap {
                self.stop_run(account_id, run_id, "request_cap").await?;
                return Ok(IncrementalOutcome::Incomplete { run_id });
            }
            if let Err(error) = self.budget.reserve(account_id, 1).await {
                if matches!(error, BudgetError::Exhausted { .. }) {
                    self.stop_run(account_id, run_id, "budget_exhausted")
                        .await?;
                    return Ok(IncrementalOutcome::Incomplete { run_id });
                }
                self.mark_failed(run_id).await?;
                return Err(SnapshotError::Budget(error));
            }
            requests_made += 1;
            let page = match source.fetch_page(continuation.as_deref()).await {
                Ok(page) => page,
                Err(BookmarkSourceError::Unavailable) => {
                    self.mark_failed(run_id).await?;
                    return Err(SnapshotError::RunUnavailable);
                }
            };
            let reached_watermark = self
                .observe_page(account_id, run_id, &page, &mut newest_provider_post_id)
                .await?
                || watermark.is_none();
            pages_fetched += 1;
            continuation = page.next_token.clone();

            if reached_watermark {
                let watermark_provider_post_id = newest_provider_post_id.clone();
                self.complete_run(run_id, watermark_provider_post_id.as_deref())
                    .await?;
                return Ok(IncrementalOutcome::Completed {
                    run_id,
                    watermark_provider_post_id,
                });
            }
            if continuation.is_none() {
                self.mark_failed(run_id).await?;
                return Err(SnapshotError::RunUnavailable);
            }
        }
    }

    async fn load_state(&self, account_id: Uuid) -> Result<(Option<String>, bool), SnapshotError> {
        let state = sqlx::query_as(
            "select watermark_provider_post_id, requires_full_snapshot \
             from x_archive.bookmark_incremental_state where account_id = $1",
        )
        .bind(account_id)
        .fetch_optional(self.database.pool())
        .await
        .map_err(SnapshotError::Query)?;
        Ok(state.unwrap_or((None, false)))
    }

    async fn create_run(&self, account_id: Uuid) -> Result<Uuid, SnapshotError> {
        sqlx::query_scalar(
            "insert into x_archive.sync_runs (account_id, run_type, state) \
             values ($1, 'incremental', 'running') returning id",
        )
        .bind(account_id)
        .fetch_one(self.database.pool())
        .await
        .map_err(SnapshotError::Query)
    }

    async fn observe_page(
        &self,
        account_id: Uuid,
        run_id: Uuid,
        page: &BookmarkPage,
        newest_provider_post_id: &mut Option<String>,
    ) -> Result<bool, SnapshotError> {
        let normalized = x_normalize::normalize::normalize(&page.envelope)?;
        let provider_ids: Vec<&str> = normalized
            .posts()
            .iter()
            .map(|post| post.provider_id.as_str())
            .collect();
        if newest_provider_post_id.is_none() {
            *newest_provider_post_id = provider_ids.first().map(|id| (*id).to_owned());
        }
        let prior_watermark = self.load_state(account_id).await?.0;
        let reached_watermark = prior_watermark
            .as_deref()
            .is_some_and(|watermark| provider_ids.contains(&watermark));
        let observation_time = observed_at(self.clock.as_ref());
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(SnapshotError::Query)?;
        let users = persist_users(&mut transaction, normalized.users()).await?;
        let posts = persist_posts(&mut transaction, normalized.posts(), &users).await?;
        persist_relations(&mut transaction, normalized.relations(), &posts).await?;
        persist_media(&mut transaction, normalized.media(), &posts).await?;
        upsert_observed_bookmarks(
            &mut transaction,
            account_id,
            run_id,
            posts.values().copied(),
            observation_time,
        )
        .await?;
        sqlx::query(
            "update x_archive.sync_runs set pages_fetched = pages_fetched + 1, \
             items_observed = items_observed + $2 where id = $1",
        )
        .bind(run_id)
        .bind(i32::try_from(posts.len()).map_err(|_| SnapshotError::StatisticsOverflow)?)
        .execute(&mut *transaction)
        .await
        .map_err(SnapshotError::Query)?;
        transaction.commit().await.map_err(SnapshotError::Query)?;
        Ok(reached_watermark)
    }

    async fn complete_run(
        &self,
        run_id: Uuid,
        watermark_provider_post_id: Option<&str>,
    ) -> Result<(), SnapshotError> {
        let completed_at = observed_at(self.clock.as_ref());
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(SnapshotError::Query)?;
        let account_id: Uuid = sqlx::query_scalar(
            "update x_archive.sync_runs set state = 'completed', finished_at = $2 where id = $1 \
             returning account_id",
        )
        .bind(run_id)
        .bind(completed_at)
        .fetch_one(&mut *transaction)
        .await
        .map_err(SnapshotError::Query)?;
        sqlx::query(
            "insert into x_archive.bookmark_incremental_state \
             (account_id, watermark_provider_post_id, last_outcome, last_incremental_run_id, updated_at) \
             values ($1, $2, 'completed', $3, $4) on conflict (account_id) do update set \
             watermark_provider_post_id = excluded.watermark_provider_post_id, \
             last_outcome = excluded.last_outcome, last_incremental_run_id = excluded.last_incremental_run_id, \
             updated_at = excluded.updated_at",
        )
        .bind(account_id)
        .bind(watermark_provider_post_id)
        .bind(run_id)
        .bind(completed_at)
        .execute(&mut *transaction)
        .await
        .map_err(SnapshotError::Query)?;
        transaction.commit().await.map_err(SnapshotError::Query)
    }

    async fn mark_failed(&self, run_id: Uuid) -> Result<(), SnapshotError> {
        sqlx::query(
            "update x_archive.sync_runs set state = 'failed', finished_at = $2 where id = $1",
        )
        .bind(run_id)
        .bind(observed_at(self.clock.as_ref()))
        .execute(self.database.pool())
        .await
        .map_err(SnapshotError::Query)?;
        Ok(())
    }

    async fn mark_gap(&self, account_id: Uuid, run_id: Uuid) -> Result<(), SnapshotError> {
        let observed_at = observed_at(self.clock.as_ref());
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(SnapshotError::Query)?;
        sqlx::query(
            "update x_archive.sync_runs set state = 'failed', finished_at = $2 where id = $1",
        )
        .bind(run_id)
        .bind(observed_at)
        .execute(&mut *transaction)
        .await
        .map_err(SnapshotError::Query)?;
        sqlx::query(
            "insert into x_archive.bookmark_incremental_state \
             (account_id, requires_full_snapshot, last_outcome, last_incremental_run_id, updated_at) \
             values ($1, true, 'gap', $2, $3) on conflict (account_id) do update set \
             requires_full_snapshot = true, last_outcome = 'gap', \
             last_incremental_run_id = excluded.last_incremental_run_id, updated_at = excluded.updated_at",
        )
        .bind(account_id)
        .bind(run_id)
        .bind(observed_at)
        .execute(&mut *transaction)
        .await
        .map_err(SnapshotError::Query)?;
        transaction.commit().await.map_err(SnapshotError::Query)
    }

    async fn stop_run(
        &self,
        account_id: Uuid,
        run_id: Uuid,
        outcome: &'static str,
    ) -> Result<(), SnapshotError> {
        let observed_at = observed_at(self.clock.as_ref());
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(SnapshotError::Query)?;
        sqlx::query(
            "update x_archive.sync_runs set state = 'failed', finished_at = $2 where id = $1",
        )
        .bind(run_id)
        .bind(observed_at)
        .execute(&mut *transaction)
        .await
        .map_err(SnapshotError::Query)?;
        sqlx::query(
            "insert into x_archive.bookmark_incremental_state \
             (account_id, last_outcome, last_incremental_run_id, updated_at) \
             values ($1, $2, $3, $4) on conflict (account_id) do update set \
             last_outcome = excluded.last_outcome, last_incremental_run_id = excluded.last_incremental_run_id, \
             updated_at = excluded.updated_at",
        )
        .bind(account_id)
        .bind(outcome)
        .bind(run_id)
        .bind(observed_at)
        .execute(&mut *transaction)
        .await
        .map_err(SnapshotError::Query)?;
        transaction.commit().await.map_err(SnapshotError::Query)
    }
}

async fn upsert_observed_bookmarks<I>(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    run_id: Uuid,
    post_ids: I,
    observed_at: DateTime<Utc>,
) -> Result<(), SnapshotError>
where
    I: IntoIterator<Item = Uuid>,
{
    for post_id in post_ids {
        sqlx::query(
            "insert into x_archive.bookmarks (account_id, post_id, first_observed_saved_at, \
             last_observed_saved_at, observed_removed_at, observed_removed_snapshot_id, \
             last_incremental_run_id) values ($1, $2, $3, $3, null, null, $4) \
             on conflict (account_id, post_id) do update set \
             last_observed_saved_at = excluded.last_observed_saved_at, observed_removed_at = null, \
             observed_removed_snapshot_id = null, last_incremental_run_id = excluded.last_incremental_run_id",
        )
        .bind(account_id)
        .bind(post_id)
        .bind(observed_at)
        .bind(run_id)
        .execute(&mut **transaction)
        .await
        .map_err(SnapshotError::Query)?;
    }
    Ok(())
}
