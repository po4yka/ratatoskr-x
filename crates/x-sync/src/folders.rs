//! Native folder membership snapshots with complete-snapshot authority.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::types::Uuid;
use x_budget::gate::{BudgetError, BudgetGate, Clock};
use x_normalize::dto::Envelope;
use x_normalize::normalize::normalize;
use x_persistence::database::Database;

use crate::{
    SnapshotError, observed_at, persist_media, persist_posts, persist_relations, persist_users,
};

/// The native folder capability that the provider did not grant or expose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderCapability {
    /// The provider cannot enumerate native bookmark folders.
    Listing,
    /// The provider cannot enumerate a native folder's post membership.
    Membership,
}

/// A native X bookmark folder observed by provider identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeFolder {
    provider_id: String,
    name: Option<String>,
}

impl NativeFolder {
    /// Creates a provider folder identity and its observed display name.
    #[must_use]
    pub fn new(provider_id: impl Into<String>, name: Option<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            name,
        }
    }

    /// Returns the opaque provider identity required by the membership endpoint.
    #[must_use]
    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }
}

/// A page from one native folder's supported membership endpoint.
#[derive(Debug, Clone)]
pub struct FolderMembershipPage {
    envelope: Envelope,
    next_token: Option<String>,
}

impl FolderMembershipPage {
    /// Builds a page from an official API envelope and opaque continuation token.
    #[must_use]
    pub fn new(envelope: Envelope, next_token: Option<String>) -> Self {
        Self {
            envelope,
            next_token,
        }
    }
}

/// Fetches pages from one native folder's supported membership endpoint.
pub trait FolderMembershipPageSource: Send + Sync {
    /// Fetches the page named by the unchanged opaque continuation token.
    fn fetch_membership_page<'a>(
        &'a self,
        folder: &'a NativeFolder,
        continuation: Option<&'a str>,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<FolderMembershipPage, FolderMembershipSourceError>>
                + Send
                + 'a,
        >,
    >;
}

/// A non-sensitive provider result for one folder membership request.
#[derive(Debug, thiserror::Error)]
pub enum FolderMembershipSourceError {
    /// The provider did not return the requested page.
    #[error("the folder membership provider did not return a page")]
    Unavailable,
    /// The provider does not expose this folder operation to the account.
    #[error("the provider does not expose the requested folder capability")]
    CapabilityLimited {
        /// The capability the provider denied or does not implement.
        capability: FolderCapability,
    },
}

/// The terminal result of one folder-membership snapshot attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderSnapshotOutcome {
    /// The run is incomplete and did not gain absence authority.
    Incomplete {
        /// The resumable run.
        run_id: Uuid,
    },
    /// A complete membership snapshot became current authority for its folder.
    Completed {
        /// The completed run.
        run_id: Uuid,
        /// The complete snapshot.
        snapshot_id: Uuid,
    },
    /// A provider capability limit left current authority unchanged.
    CapabilityLimited {
        /// The terminal run.
        run_id: Uuid,
        /// The unavailable provider capability.
        capability: FolderCapability,
    },
}

/// Coordinates read-only native folder membership snapshots.
#[derive(Clone)]
pub struct FolderSnapshotService {
    database: Database,
    budget: BudgetGate,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for FolderSnapshotService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FolderSnapshotService")
            .finish_non_exhaustive()
    }
}

impl FolderSnapshotService {
    /// Creates the native folder snapshot coordinator.
    #[must_use]
    pub fn new(database: Database, budget: BudgetGate, clock: Arc<dyn Clock>) -> Self {
        Self {
            database,
            budget,
            clock,
        }
    }

    /// Starts a complete folder-membership snapshot or resumes an unfinished one.
    ///
    /// # Errors
    /// Returns an error when normalization, persistence, the durable budget, or the selected run
    /// cannot be used.
    pub async fn run<S>(
        &self,
        account_id: Uuid,
        folder: &NativeFolder,
        source: &S,
        resume_run_id: Option<Uuid>,
    ) -> Result<FolderSnapshotOutcome, SnapshotError>
    where
        S: FolderMembershipPageSource,
    {
        let mut run = self
            .load_or_create_run(account_id, folder, resume_run_id)
            .await?;
        loop {
            if let Err(error) = self.budget.reserve(account_id, 1).await {
                self.mark_failed(run.id).await?;
                if matches!(error, BudgetError::Exhausted { .. }) {
                    return Ok(FolderSnapshotOutcome::Incomplete { run_id: run.id });
                }
                return Err(SnapshotError::Budget(error));
            }
            let page = match source
                .fetch_membership_page(folder, run.checkpoint.as_deref())
                .await
            {
                Ok(page) => page,
                Err(FolderMembershipSourceError::Unavailable) => {
                    self.mark_failed(run.id).await?;
                    return Ok(FolderSnapshotOutcome::Incomplete { run_id: run.id });
                }
                Err(FolderMembershipSourceError::CapabilityLimited { capability }) => {
                    self.mark_capability_limited(run.id, capability).await?;
                    return Ok(FolderSnapshotOutcome::CapabilityLimited {
                        run_id: run.id,
                        capability,
                    });
                }
            };
            let next_token = page.next_token.clone();
            if let Err(error) = self.stage_page(&run, page).await {
                self.mark_failed(run.id).await?;
                return Err(error);
            }
            run.checkpoint = next_token;
            if run.checkpoint.is_none() {
                self.complete_run(&run).await?;
                return Ok(FolderSnapshotOutcome::Completed {
                    run_id: run.id,
                    snapshot_id: run.snapshot_id,
                });
            }
        }
    }

    /// Records a folder API capability limit without asserting any folder authority.
    ///
    /// # Errors
    /// Returns an error when the terminal run cannot be stored.
    pub async fn record_capability_limit(
        &self,
        account_id: Uuid,
        capability: FolderCapability,
    ) -> Result<FolderSnapshotOutcome, SnapshotError> {
        let run_type = folder_run_type(capability);
        let run_id: Uuid = sqlx::query_scalar(
            "insert into x_archive.sync_runs (account_id, run_type, state) \
             values ($1, $2, 'running') returning id",
        )
        .bind(account_id)
        .bind(run_type)
        .fetch_one(self.database.pool())
        .await
        .map_err(SnapshotError::Query)?;
        self.mark_capability_limited(run_id, capability).await?;
        Ok(FolderSnapshotOutcome::CapabilityLimited { run_id, capability })
    }

    async fn load_or_create_run(
        &self,
        account_id: Uuid,
        folder: &NativeFolder,
        resume_run_id: Option<Uuid>,
    ) -> Result<FolderRun, SnapshotError> {
        match resume_run_id {
            Some(run_id) => self.resume_run(account_id, folder, run_id).await,
            None => self.create_run(account_id, folder).await,
        }
    }

    async fn create_run(
        &self,
        account_id: Uuid,
        folder: &NativeFolder,
    ) -> Result<FolderRun, SnapshotError> {
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(SnapshotError::Query)?;
        let folder_id = upsert_folder(&mut transaction, account_id, folder).await?;
        let run_id: Uuid = sqlx::query_scalar(
            "insert into x_archive.sync_runs (account_id, run_type, state) \
             values ($1, 'folder_membership', 'running') returning id",
        )
        .bind(account_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(SnapshotError::Query)?;
        let snapshot_id: Uuid = sqlx::query_scalar(
            "insert into x_archive.snapshots (sync_run_id, folder_id) values ($1, $2) returning id",
        )
        .bind(run_id)
        .bind(folder_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(SnapshotError::Query)?;
        transaction.commit().await.map_err(SnapshotError::Query)?;
        Ok(FolderRun {
            id: run_id,
            snapshot_id,
            folder_id,
            checkpoint: None,
        })
    }

    async fn resume_run(
        &self,
        account_id: Uuid,
        folder: &NativeFolder,
        run_id: Uuid,
    ) -> Result<FolderRun, SnapshotError> {
        let row: Option<(Uuid, Uuid, Option<String>)> = sqlx::query_as(
            "select snapshot.id, snapshot.folder_id, run.checkpoint from x_archive.sync_runs run \
             join x_archive.snapshots snapshot on snapshot.sync_run_id = run.id \
             join x_archive.bookmark_folders folder on folder.id = snapshot.folder_id \
             where run.id = $1 and run.account_id = $2 and run.run_type = 'folder_membership' \
             and run.state in ('running', 'failed') and folder.provider_id = $3",
        )
        .bind(run_id)
        .bind(account_id)
        .bind(&folder.provider_id)
        .fetch_optional(self.database.pool())
        .await
        .map_err(SnapshotError::Query)?;
        let Some((snapshot_id, folder_id, checkpoint)) = row else {
            return Err(SnapshotError::RunUnavailable);
        };
        sqlx::query(
            "update x_archive.sync_runs set state = 'running', finished_at = null where id = $1",
        )
        .bind(run_id)
        .execute(self.database.pool())
        .await
        .map_err(SnapshotError::Query)?;
        Ok(FolderRun {
            id: run_id,
            snapshot_id,
            folder_id,
            checkpoint,
        })
    }

    async fn stage_page(
        &self,
        run: &FolderRun,
        page: FolderMembershipPage,
    ) -> Result<(), SnapshotError> {
        let normalized = normalize(&page.envelope)?;
        let post_count = i32::try_from(normalized.posts().len())
            .map_err(|_| SnapshotError::StatisticsOverflow)?;
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
        for post_id in posts.values() {
            sqlx::query(
                "insert into x_archive.snapshot_folder_membership_items (snapshot_id, post_id, observed_at) \
                 values ($1, $2, $3) on conflict (snapshot_id, post_id) do update \
                 set observed_at = excluded.observed_at",
            )
            .bind(run.snapshot_id)
            .bind(*post_id)
            .bind(observation_time)
            .execute(&mut *transaction)
            .await
            .map_err(SnapshotError::Query)?;
        }
        sqlx::query(
            "update x_archive.sync_runs set checkpoint = $2, pages_fetched = pages_fetched + 1, \
             items_observed = items_observed + $3 where id = $1",
        )
        .bind(run.id)
        .bind(&page.next_token)
        .bind(post_count)
        .execute(&mut *transaction)
        .await
        .map_err(SnapshotError::Query)?;
        transaction.commit().await.map_err(SnapshotError::Query)
    }

    async fn complete_run(&self, run: &FolderRun) -> Result<(), SnapshotError> {
        let completed_at = observed_at(self.clock.as_ref());
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(SnapshotError::Query)?;
        lock_finalization(&mut transaction, run).await?;
        let statistics = reconcile_memberships(&mut transaction, run, completed_at).await?;
        record_completion(&mut transaction, run, completed_at, statistics).await?;
        replace_authority(
            &mut transaction,
            run.folder_id,
            run.snapshot_id,
            completed_at,
        )
        .await?;
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

    async fn mark_capability_limited(
        &self,
        run_id: Uuid,
        capability: FolderCapability,
    ) -> Result<(), SnapshotError> {
        let recorded_at = observed_at(self.clock.as_ref());
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(SnapshotError::Query)?;
        let account_id: Uuid = sqlx::query_scalar(
            "update x_archive.sync_runs set state = 'failed', finished_at = $2 where id = $1 \
             returning account_id",
        )
        .bind(run_id)
        .bind(recorded_at)
        .fetch_one(&mut *transaction)
        .await
        .map_err(SnapshotError::Query)?;
        sqlx::query(
            "insert into x_archive.folder_capability_limits (account_id, capability, last_run_id, recorded_at) \
             values ($1, $2, $3, $4) on conflict (account_id, capability) do update set \
             last_run_id = excluded.last_run_id, recorded_at = excluded.recorded_at",
        )
        .bind(account_id)
        .bind(folder_capability_name(capability))
        .bind(run_id)
        .bind(recorded_at)
        .execute(&mut *transaction)
        .await
        .map_err(SnapshotError::Query)?;
        transaction.commit().await.map_err(SnapshotError::Query)
    }
}

#[derive(Debug)]
struct FolderRun {
    id: Uuid,
    snapshot_id: Uuid,
    folder_id: Uuid,
    checkpoint: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct MembershipStatistics {
    added: i32,
    retained: i32,
    removed: i32,
}

fn folder_capability_name(capability: FolderCapability) -> &'static str {
    match capability {
        FolderCapability::Listing => "listing",
        FolderCapability::Membership => "membership",
    }
}

fn folder_run_type(capability: FolderCapability) -> &'static str {
    match capability {
        FolderCapability::Listing => "folder_listing",
        FolderCapability::Membership => "folder_membership",
    }
}

async fn upsert_folder(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    folder: &NativeFolder,
) -> Result<Uuid, SnapshotError> {
    sqlx::query_scalar(
        "insert into x_archive.bookmark_folders (account_id, provider_id, name) values ($1, $2, $3) \
         on conflict (account_id, provider_id) do update set name = excluded.name returning id",
    )
    .bind(account_id)
    .bind(&folder.provider_id)
    .bind(&folder.name)
    .fetch_one(&mut **transaction)
    .await
    .map_err(SnapshotError::Query)
}

async fn lock_finalization(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run: &FolderRun,
) -> Result<(), SnapshotError> {
    sqlx::query("set transaction isolation level serializable")
        .execute(&mut **transaction)
        .await
        .map_err(SnapshotError::Query)?;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(run.folder_id.to_string())
        .execute(&mut **transaction)
        .await
        .map_err(SnapshotError::Query)?;
    let current: Option<(bool, String)> = sqlx::query_as(
        "select snapshot.complete, run.state from x_archive.snapshots snapshot \
         join x_archive.sync_runs run on run.id = snapshot.sync_run_id \
         where snapshot.id = $1 and snapshot.folder_id = $2 and run.id = $3 for update",
    )
    .bind(run.snapshot_id)
    .bind(run.folder_id)
    .bind(run.id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(SnapshotError::Query)?;
    if matches!(current, Some((false, state)) if state == "running") {
        Ok(())
    } else {
        Err(SnapshotError::RunUnavailable)
    }
}

async fn reconcile_memberships(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run: &FolderRun,
    completed_at: DateTime<Utc>,
) -> Result<MembershipStatistics, SnapshotError> {
    let added = count_added(transaction, run).await?;
    let retained = count_retained(transaction, run).await?;
    record_added_observations(transaction, run, completed_at).await?;
    upsert_active_memberships(transaction, run).await?;
    let removed = record_absent_memberships(transaction, run, completed_at).await?;
    Ok(MembershipStatistics {
        added: i32::try_from(added).map_err(|_| SnapshotError::StatisticsOverflow)?,
        retained: i32::try_from(retained).map_err(|_| SnapshotError::StatisticsOverflow)?,
        removed: i32::try_from(removed).map_err(|_| SnapshotError::StatisticsOverflow)?,
    })
}

async fn count_added(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run: &FolderRun,
) -> Result<i64, SnapshotError> {
    sqlx::query_scalar(
        "select count(*) from x_archive.snapshot_folder_membership_items staged \
         left join x_archive.bookmark_folder_items item on item.folder_id = $1 \
           and item.post_id = staged.post_id and item.observed_removed_from_folder_at is null \
         where staged.snapshot_id = $2 and item.post_id is null",
    )
    .bind(run.folder_id)
    .bind(run.snapshot_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(SnapshotError::Query)
}

async fn count_retained(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run: &FolderRun,
) -> Result<i64, SnapshotError> {
    sqlx::query_scalar(
        "select count(*) from x_archive.snapshot_folder_membership_items staged \
         join x_archive.bookmark_folder_items item on item.folder_id = $1 and item.post_id = staged.post_id \
           and item.observed_removed_from_folder_at is null where staged.snapshot_id = $2",
    )
    .bind(run.folder_id)
    .bind(run.snapshot_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(SnapshotError::Query)
}

async fn record_added_observations(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run: &FolderRun,
    completed_at: DateTime<Utc>,
) -> Result<(), SnapshotError> {
    sqlx::query(
        "insert into x_archive.folder_membership_observations (snapshot_id, folder_id, post_id, kind, observed_at) \
         select $2, $1, staged.post_id, 'added', $3 from x_archive.snapshot_folder_membership_items staged \
         left join x_archive.bookmark_folder_items item on item.folder_id = $1 and item.post_id = staged.post_id \
           and item.observed_removed_from_folder_at is null where staged.snapshot_id = $2 and item.post_id is null \
         on conflict do nothing",
    )
    .bind(run.folder_id)
    .bind(run.snapshot_id)
    .bind(completed_at)
    .execute(&mut **transaction)
    .await
    .map_err(SnapshotError::Query)?;
    Ok(())
}

async fn upsert_active_memberships(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run: &FolderRun,
) -> Result<(), SnapshotError> {
    sqlx::query(
        "insert into x_archive.bookmark_folder_items (folder_id, post_id, first_observed_in_folder_at, \
         observed_removed_from_folder_at, observed_removed_snapshot_id) select $1, staged.post_id, \
         staged.observed_at, null, null from x_archive.snapshot_folder_membership_items staged \
         where staged.snapshot_id = $2 on conflict (folder_id, post_id) do update set \
         observed_removed_from_folder_at = null, observed_removed_snapshot_id = null",
    )
    .bind(run.folder_id)
    .bind(run.snapshot_id)
    .execute(&mut **transaction)
    .await
    .map_err(SnapshotError::Query)?;
    Ok(())
}

async fn record_absent_memberships(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run: &FolderRun,
    completed_at: DateTime<Utc>,
) -> Result<u64, SnapshotError> {
    let removed_posts: Vec<Uuid> = sqlx::query_scalar(
        "update x_archive.bookmark_folder_items item set observed_removed_from_folder_at = $3, \
         observed_removed_snapshot_id = $2 where item.folder_id = $1 and item.observed_removed_from_folder_at is null \
         and not exists (select 1 from x_archive.snapshot_folder_membership_items staged \
         where staged.snapshot_id = $2 and staged.post_id = item.post_id) returning item.post_id",
    )
    .bind(run.folder_id)
    .bind(run.snapshot_id)
    .bind(completed_at)
    .fetch_all(&mut **transaction)
    .await
    .map_err(SnapshotError::Query)?;
    for post_id in &removed_posts {
        sqlx::query(
            "insert into x_archive.folder_membership_observations (snapshot_id, folder_id, post_id, kind, observed_at) \
             values ($1, $2, $3, 'removed', $4) on conflict do nothing",
        )
        .bind(run.snapshot_id)
        .bind(run.folder_id)
        .bind(*post_id)
        .bind(completed_at)
        .execute(&mut **transaction)
        .await
        .map_err(SnapshotError::Query)?;
    }
    u64::try_from(removed_posts.len()).map_err(|_| SnapshotError::StatisticsOverflow)
}

async fn record_completion(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run: &FolderRun,
    completed_at: DateTime<Utc>,
    statistics: MembershipStatistics,
) -> Result<(), SnapshotError> {
    sqlx::query(
        "update x_archive.sync_runs set state = 'completed', finished_at = $2, added_count = $3, \
         retained_count = $4, removed_count = $5, checkpoint = null where id = $1",
    )
    .bind(run.id)
    .bind(completed_at)
    .bind(statistics.added)
    .bind(statistics.retained)
    .bind(statistics.removed)
    .execute(&mut **transaction)
    .await
    .map_err(SnapshotError::Query)?;
    sqlx::query(
        "update x_archive.snapshots set complete = true, completed_at = $2, \
         page_count = (select pages_fetched from x_archive.sync_runs where id = $1) where id = $3",
    )
    .bind(run.id)
    .bind(completed_at)
    .bind(run.snapshot_id)
    .execute(&mut **transaction)
    .await
    .map_err(SnapshotError::Query)?;
    Ok(())
}

async fn replace_authority(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    folder_id: Uuid,
    snapshot_id: Uuid,
    completed_at: DateTime<Utc>,
) -> Result<(), SnapshotError> {
    sqlx::query(
        "insert into x_archive.folder_membership_snapshot_authority (folder_id, snapshot_id, updated_at) \
         values ($1, $2, $3) on conflict (folder_id) do update set snapshot_id = excluded.snapshot_id, \
         updated_at = excluded.updated_at",
    )
    .bind(folder_id)
    .bind(snapshot_id)
    .bind(completed_at)
    .execute(&mut **transaction)
    .await
    .map_err(SnapshotError::Query)?;
    Ok(())
}
