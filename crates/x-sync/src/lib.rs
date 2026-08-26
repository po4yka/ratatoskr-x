//! Complete bookmark snapshots with durable checkpoints and atomic authority.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::types::Uuid;

use x_budget::gate::{BudgetError, BudgetGate, Clock};
use x_normalize::dto::Envelope;
use x_normalize::normalize::normalize;
use x_normalize::records::{Availability, Relation};
use x_persistence::database::Database;

/// One page returned by the supported official-provider bookmark endpoint.
#[derive(Debug, Clone)]
pub struct BookmarkPage {
    envelope: Envelope,
    next_token: Option<String>,
}

impl BookmarkPage {
    /// Builds one provider page from its official API envelope and opaque continuation token.
    #[must_use]
    pub fn new(envelope: Envelope, next_token: Option<String>) -> Self {
        Self {
            envelope,
            next_token,
        }
    }
}

/// A bounded source of official API bookmark pages.
pub trait BookmarkPageSource: Send + Sync {
    /// Fetches the page named by `continuation`, which must be passed through unchanged.
    fn fetch_page<'a>(
        &'a self,
        continuation: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<BookmarkPage, BookmarkSourceError>> + Send + 'a>>;
}

/// A non-sensitive source failure that leaves the snapshot resumable.
#[derive(Debug, thiserror::Error)]
pub enum BookmarkSourceError {
    /// The provider did not return the requested page.
    #[error("the bookmark provider did not return a page")]
    Unavailable,
}

/// The terminal result of one snapshot attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotOutcome {
    /// The run has a durable checkpoint but is not authoritative.
    Incomplete {
        /// The run to resume with the saved checkpoint.
        run_id: Uuid,
    },
    /// A complete snapshot atomically became current account authority.
    Completed {
        /// The completed run.
        run_id: Uuid,
        /// The snapshot that now has absence authority.
        snapshot_id: Uuid,
    },
}

/// Failures that prevent the snapshot runner from recording an outcome.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SnapshotError {
    /// The source run is not resumable for the supplied account.
    #[error("the requested bookmark snapshot run is unavailable")]
    RunUnavailable,
    /// A durable budget operation failed unexpectedly.
    #[error(transparent)]
    Budget(#[from] BudgetError),
    /// The official provider payload could not be normalized.
    #[error(transparent)]
    Normalize(#[from] x_normalize::error::NormalizeError),
    /// A database query failed.
    #[error("a bookmark snapshot database query failed")]
    Query(#[source] sqlx::Error),
    /// A normalized post did not have its required normalized author row.
    #[error("a normalized post had no persisted author row")]
    PersistedAuthorMissing,
    /// A database count cannot fit in the schema's bounded integer statistics fields.
    #[error("bookmark snapshot statistics exceed the supported range")]
    StatisticsOverflow,
}

/// Coordinates one account's full bookmark snapshot.
#[derive(Clone)]
pub struct BookmarkSnapshotService {
    database: Database,
    budget: BudgetGate,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for BookmarkSnapshotService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BookmarkSnapshotService")
            .finish_non_exhaustive()
    }
}

impl BookmarkSnapshotService {
    /// Builds a snapshot service over the existing durable budget and database boundaries.
    #[must_use]
    pub fn new(database: Database, budget: BudgetGate, clock: Arc<dyn Clock>) -> Self {
        Self {
            database,
            budget,
            clock,
        }
    }

    /// Starts a fresh full snapshot or resumes the named unfinished run.
    ///
    /// A source or budget-exhaustion outcome is persisted as a resumable
    /// incomplete run. Other failures are returned after the run is marked
    /// failed, and none can acquire absence authority.
    ///
    /// # Errors
    /// Returns an error when normalization, persistence, the durable budget,
    /// or the selected resumable run cannot be used.
    pub async fn run<S>(
        &self,
        account_id: Uuid,
        source: &S,
        resume_run_id: Option<Uuid>,
    ) -> Result<SnapshotOutcome, SnapshotError>
    where
        S: BookmarkPageSource,
    {
        let mut run = self.load_or_create_run(account_id, resume_run_id).await?;

        loop {
            if let Err(error) = self.budget.reserve(account_id, 1).await {
                if matches!(error, BudgetError::Exhausted { .. }) {
                    self.mark_failed(run.id).await?;
                    return Ok(SnapshotOutcome::Incomplete { run_id: run.id });
                }
                self.mark_failed(run.id).await?;
                return Err(SnapshotError::Budget(error));
            }

            let page = match source.fetch_page(run.checkpoint.as_deref()).await {
                Ok(page) => page,
                Err(BookmarkSourceError::Unavailable) => {
                    self.mark_failed(run.id).await?;
                    return Ok(SnapshotOutcome::Incomplete { run_id: run.id });
                }
            };
            let next_token = page.next_token.clone();
            let observation_time = observed_at(self.clock.as_ref());

            if let Err(error) = self.stage_page(&run, page, observation_time).await {
                self.mark_failed(run.id).await?;
                return Err(error);
            }
            run.checkpoint = next_token;

            if run.checkpoint.is_none() {
                self.complete_run(account_id, &run).await?;
                return Ok(SnapshotOutcome::Completed {
                    run_id: run.id,
                    snapshot_id: run.snapshot_id,
                });
            }
        }
    }

    async fn load_or_create_run(
        &self,
        account_id: Uuid,
        resume_run_id: Option<Uuid>,
    ) -> Result<Run, SnapshotError> {
        match resume_run_id {
            Some(run_id) => self.resume_run(account_id, run_id).await,
            None => self.create_run(account_id).await,
        }
    }

    async fn create_run(&self, account_id: Uuid) -> Result<Run, SnapshotError> {
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(SnapshotError::Query)?;
        let run_id: Uuid = sqlx::query_scalar(
            "insert into x_archive.sync_runs (account_id, run_type, state) \
             values ($1, 'full', 'running') returning id",
        )
        .bind(account_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(SnapshotError::Query)?;
        let snapshot_id: Uuid = sqlx::query_scalar(
            "insert into x_archive.snapshots (sync_run_id) values ($1) returning id",
        )
        .bind(run_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(SnapshotError::Query)?;
        transaction.commit().await.map_err(SnapshotError::Query)?;

        Ok(Run {
            id: run_id,
            snapshot_id,
            checkpoint: None,
        })
    }

    async fn resume_run(&self, account_id: Uuid, run_id: Uuid) -> Result<Run, SnapshotError> {
        let row: Option<(Uuid, Option<String>)> = sqlx::query_as(
            "select snapshot.id, run.checkpoint \
             from x_archive.sync_runs run \
             join x_archive.snapshots snapshot on snapshot.sync_run_id = run.id \
             where run.id = $1 and run.account_id = $2 and run.run_type = 'full' \
               and run.state in ('running', 'failed')",
        )
        .bind(run_id)
        .bind(account_id)
        .fetch_optional(self.database.pool())
        .await
        .map_err(SnapshotError::Query)?;
        let Some((snapshot_id, checkpoint)) = row else {
            return Err(SnapshotError::RunUnavailable);
        };
        sqlx::query(
            "update x_archive.sync_runs set state = 'running', finished_at = null where id = $1",
        )
        .bind(run_id)
        .execute(self.database.pool())
        .await
        .map_err(SnapshotError::Query)?;

        Ok(Run {
            id: run_id,
            snapshot_id,
            checkpoint,
        })
    }

    async fn stage_page(
        &self,
        run: &Run,
        page: BookmarkPage,
        observed_at: DateTime<Utc>,
    ) -> Result<(), SnapshotError> {
        let normalized = normalize(&page.envelope)?;
        let item_count = i32::try_from(normalized.posts().len())
            .map_err(|_| SnapshotError::StatisticsOverflow)?;
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
                "insert into x_archive.snapshot_bookmark_items (snapshot_id, post_id, observed_at) \
                 values ($1, $2, $3) on conflict (snapshot_id, post_id) do update \
                 set observed_at = excluded.observed_at",
            )
            .bind(run.snapshot_id)
            .bind(*post_id)
            .bind(observed_at)
            .execute(&mut *transaction)
            .await
            .map_err(SnapshotError::Query)?;
        }
        sqlx::query(
            "update x_archive.sync_runs set checkpoint = $2, pages_fetched = pages_fetched + 1, \
             items_observed = items_observed + $3 where id = $1 and state = 'running'",
        )
        .bind(run.id)
        .bind(&page.next_token)
        .bind(item_count)
        .execute(&mut *transaction)
        .await
        .map_err(SnapshotError::Query)?;
        transaction.commit().await.map_err(SnapshotError::Query)?;
        Ok(())
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

    async fn complete_run(&self, account_id: Uuid, run: &Run) -> Result<(), SnapshotError> {
        let completed_at = observed_at(self.clock.as_ref());
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(SnapshotError::Query)?;
        lock_finalization(&mut transaction, account_id, run).await?;
        let statistics =
            reconcile_bookmarks(&mut transaction, account_id, run, completed_at).await?;
        record_completion(&mut transaction, run, completed_at, statistics).await?;
        replace_authority(&mut transaction, account_id, run.snapshot_id, completed_at).await?;
        transaction.commit().await.map_err(SnapshotError::Query)?;
        Ok(())
    }
}

#[derive(Debug)]
struct Run {
    id: Uuid,
    snapshot_id: Uuid,
    checkpoint: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct ReconciliationStatistics {
    added: i32,
    retained: i32,
    removed: i32,
}

async fn lock_finalization(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    run: &Run,
) -> Result<(), SnapshotError> {
    sqlx::query("set transaction isolation level serializable")
        .execute(&mut **transaction)
        .await
        .map_err(SnapshotError::Query)?;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(account_id.to_string())
        .execute(&mut **transaction)
        .await
        .map_err(SnapshotError::Query)?;
    let current: Option<(bool, String)> = sqlx::query_as(
        "select snapshot.complete, run.state from x_archive.snapshots snapshot \
         join x_archive.sync_runs run on run.id = snapshot.sync_run_id \
         where snapshot.id = $1 and run.id = $2 and run.account_id = $3 for update",
    )
    .bind(run.snapshot_id)
    .bind(run.id)
    .bind(account_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(SnapshotError::Query)?;
    if matches!(current, Some((false, state)) if state == "running") {
        Ok(())
    } else {
        Err(SnapshotError::RunUnavailable)
    }
}

async fn reconcile_bookmarks(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    run: &Run,
    completed_at: DateTime<Utc>,
) -> Result<ReconciliationStatistics, SnapshotError> {
    let added_count: i64 = sqlx::query_scalar(
        "select count(*) from x_archive.snapshot_bookmark_items item \
         left join x_archive.bookmarks bookmark on bookmark.account_id = $1 \
           and bookmark.post_id = item.post_id and bookmark.observed_removed_at is null \
         where item.snapshot_id = $2 and bookmark.id is null",
    )
    .bind(account_id)
    .bind(run.snapshot_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(SnapshotError::Query)?;
    let retained_count: i64 = sqlx::query_scalar(
        "select count(*) from x_archive.snapshot_bookmark_items item \
         join x_archive.bookmarks bookmark on bookmark.account_id = $1 \
           and bookmark.post_id = item.post_id and bookmark.observed_removed_at is null \
         where item.snapshot_id = $2",
    )
    .bind(account_id)
    .bind(run.snapshot_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(SnapshotError::Query)?;
    upsert_active_bookmarks(transaction, account_id, run.snapshot_id).await?;
    let removed_count =
        record_absent_bookmarks(transaction, account_id, run.snapshot_id, completed_at).await?;

    Ok(ReconciliationStatistics {
        added: i32::try_from(added_count).map_err(|_| SnapshotError::StatisticsOverflow)?,
        retained: i32::try_from(retained_count).map_err(|_| SnapshotError::StatisticsOverflow)?,
        removed: i32::try_from(removed_count).map_err(|_| SnapshotError::StatisticsOverflow)?,
    })
}

async fn upsert_active_bookmarks(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    snapshot_id: Uuid,
) -> Result<(), SnapshotError> {
    sqlx::query(
        "insert into x_archive.bookmarks (account_id, post_id, first_observed_saved_at, \
         last_observed_saved_at, observed_removed_at, observed_removed_snapshot_id) \
         select $1, item.post_id, item.observed_at, item.observed_at, null, null \
         from x_archive.snapshot_bookmark_items item where item.snapshot_id = $2 \
         on conflict (account_id, post_id) do update set \
         last_observed_saved_at = excluded.last_observed_saved_at, observed_removed_at = null, \
         observed_removed_snapshot_id = null",
    )
    .bind(account_id)
    .bind(snapshot_id)
    .execute(&mut **transaction)
    .await
    .map_err(SnapshotError::Query)?;
    Ok(())
}

async fn record_absent_bookmarks(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    snapshot_id: Uuid,
    completed_at: DateTime<Utc>,
) -> Result<u64, SnapshotError> {
    let removed_count = sqlx::query(
        "update x_archive.bookmarks bookmark set observed_removed_at = $3, \
         observed_removed_snapshot_id = $2 where bookmark.account_id = $1 \
         and bookmark.observed_removed_at is null and not exists ( \
           select 1 from x_archive.snapshot_bookmark_items item \
           where item.snapshot_id = $2 and item.post_id = bookmark.post_id
         )",
    )
    .bind(account_id)
    .bind(snapshot_id)
    .bind(completed_at)
    .execute(&mut **transaction)
    .await
    .map_err(SnapshotError::Query)?
    .rows_affected();
    Ok(removed_count)
}

async fn record_completion(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run: &Run,
    completed_at: DateTime<Utc>,
    statistics: ReconciliationStatistics,
) -> Result<(), SnapshotError> {
    sqlx::query(
        "update x_archive.sync_runs set state = 'completed', finished_at = $2, \
         added_count = $3, retained_count = $4, removed_count = $5, checkpoint = null \
         where id = $1",
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
         page_count = (select pages_fetched from x_archive.sync_runs where id = $1) \
         where id = $3",
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
    account_id: Uuid,
    snapshot_id: Uuid,
    completed_at: DateTime<Utc>,
) -> Result<(), SnapshotError> {
    sqlx::query(
        "insert into x_archive.bookmark_snapshot_authority (account_id, snapshot_id, updated_at) \
         values ($1, $2, $3) on conflict (account_id) do update set \
         snapshot_id = excluded.snapshot_id, updated_at = excluded.updated_at",
    )
    .bind(account_id)
    .bind(snapshot_id)
    .bind(completed_at)
    .execute(&mut **transaction)
    .await
    .map_err(SnapshotError::Query)?;
    Ok(())
}

async fn persist_users(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    users: &[x_normalize::records::NormalizedUser],
) -> Result<HashMap<String, Uuid>, SnapshotError> {
    let mut ids = HashMap::new();
    for user in users {
        let id: Uuid = sqlx::query_scalar(
            "insert into x_archive.users (provider_id, username, display_name, parser_version) \
             values ($1, $2, $3, $4) on conflict (provider_id) do update set \
             username = excluded.username, display_name = excluded.display_name, \
             parser_version = excluded.parser_version, updated_at = now() returning id",
        )
        .bind(&user.provider_id)
        .bind(&user.username)
        .bind(&user.display_name)
        .bind(user.parser_version)
        .fetch_one(&mut **transaction)
        .await
        .map_err(SnapshotError::Query)?;
        ids.insert(user.provider_id.clone(), id);
    }
    Ok(ids)
}

async fn persist_posts(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    posts: &[x_normalize::records::NormalizedPost],
    users: &HashMap<String, Uuid>,
) -> Result<HashMap<String, Uuid>, SnapshotError> {
    let mut ids = HashMap::new();
    for post in posts {
        let author_id = users
            .get(&post.author_provider_id)
            .copied()
            .ok_or(SnapshotError::PersistedAuthorMissing)?;
        let id: Uuid = sqlx::query_scalar(
            "insert into x_archive.posts (provider_id, author_user_id, text, long_text, language, \
             published_at, edited_at, conversation_provider_id, like_count, retweet_count, \
             reply_count, quote_count, bookmark_count, impression_count, parser_version, availability) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16) \
             on conflict (provider_id) do update set author_user_id = excluded.author_user_id, \
             text = excluded.text, long_text = excluded.long_text, language = excluded.language, \
             published_at = excluded.published_at, edited_at = excluded.edited_at, \
             conversation_provider_id = excluded.conversation_provider_id, like_count = excluded.like_count, \
             retweet_count = excluded.retweet_count, reply_count = excluded.reply_count, \
             quote_count = excluded.quote_count, bookmark_count = excluded.bookmark_count, \
             impression_count = excluded.impression_count, parser_version = excluded.parser_version, \
             availability = excluded.availability, updated_at = now() returning id",
        )
        .bind(&post.provider_id)
        .bind(author_id)
        .bind(&post.text)
        .bind(&post.long_text)
        .bind(&post.language)
        .bind(post.published_at)
        .bind(post.edited_at)
        .bind(&post.conversation_provider_id)
        .bind(post.like_count)
        .bind(post.retweet_count)
        .bind(post.reply_count)
        .bind(post.quote_count)
        .bind(post.bookmark_count)
        .bind(post.impression_count)
        .bind(post.parser_version)
        .bind(availability_name(post.availability))
        .fetch_one(&mut **transaction)
        .await
        .map_err(SnapshotError::Query)?;
        ids.insert(post.provider_id.clone(), id);
    }
    Ok(ids)
}

async fn persist_relations(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    relations: &[x_normalize::records::NormalizedRelation],
    posts: &HashMap<String, Uuid>,
) -> Result<(), SnapshotError> {
    for relation in relations {
        let post_id = posts
            .get(&relation.post_provider_id)
            .copied()
            .ok_or(SnapshotError::PersistedAuthorMissing)?;
        sqlx::query(
            "insert into x_archive.post_relations (post_id, related_post_provider_id, relation, parser_version) \
             values ($1, $2, $3, $4) on conflict (post_id, related_post_provider_id, relation) \
             do update set parser_version = excluded.parser_version",
        )
        .bind(post_id)
        .bind(&relation.related_post_provider_id)
        .bind(relation_name(relation.relation))
        .bind(relation.parser_version)
        .execute(&mut **transaction)
        .await
        .map_err(SnapshotError::Query)?;
    }
    Ok(())
}

async fn persist_media(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    media: &[x_normalize::records::NormalizedMedia],
    posts: &HashMap<String, Uuid>,
) -> Result<(), SnapshotError> {
    for item in media {
        let post_id = posts
            .get(&item.post_provider_id)
            .copied()
            .ok_or(SnapshotError::PersistedAuthorMissing)?;
        sqlx::query(
            "insert into x_archive.media (post_id, provider_id, kind, metadata, blob_ref, parser_version) \
             values ($1, $2, $3, $4::jsonb, $5, $6) on conflict (post_id, provider_id) do update \
             set kind = excluded.kind, metadata = excluded.metadata, blob_ref = excluded.blob_ref, \
             parser_version = excluded.parser_version",
        )
        .bind(post_id)
        .bind(&item.provider_id)
        .bind(item.kind.map(|kind| match kind {
            x_normalize::records::MediaKind::Photo => "photo",
            x_normalize::records::MediaKind::Video => "video",
            x_normalize::records::MediaKind::AnimatedGif => "animated_gif",
        }))
        .bind(item.metadata.to_string())
        .bind(&item.blob_ref)
        .bind(item.parser_version)
        .execute(&mut **transaction)
        .await
        .map_err(SnapshotError::Query)?;
    }
    Ok(())
}

fn availability_name(availability: Availability) -> &'static str {
    match availability {
        Availability::Active => "active",
        Availability::Deleted => "deleted",
        Availability::Protected => "protected",
        Availability::AuthorSuspended => "author_suspended",
        Availability::Unavailable => "unavailable",
        Availability::Unknown => "unknown",
    }
}

fn relation_name(relation: Relation) -> &'static str {
    match relation {
        Relation::Reply => "reply",
        Relation::Quote => "quote",
        Relation::Repost => "repost",
    }
}

/// Returns an observation timestamp through the service's injected clock.
fn observed_at(clock: &dyn Clock) -> DateTime<Utc> {
    clock.now()
}
