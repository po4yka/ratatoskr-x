//! The public capture worker (XR-021 CONTRACTS.md S10 CD2, CD5 and CD6).
//!
//! An explicit browser capture is accepted by `x-capture` as a queued `explicit_captures` row that
//! names its owner. This worker claims due rows, asks the injected [`PublicPostResolver`] for the
//! post (a public, app-only call that returns only what any reader may see), and commits exactly
//! one outcome per capture in one transaction: the owner's `explicit_sources` row, the owner-scoped
//! social event when the content changed, and the single terminal operation report.
//!
//! The worker never reads rows another tenant's sync produced. Its tables are `explicit_captures`
//! and `explicit_sources`, both keyed by the owner.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use ratatoskr_identifiers::{CommandId, OperationId, SocialSourceId, UserId};
use ratatoskr_operation_contracts::OperationReported;
use ratatoskr_social_contracts::{
    SocialContractError, SourceUnavailability, preserved_report, report_envelope,
    unavailable_report,
};
use sqlx::types::Uuid;
use x_budget::gate::Clock;
use x_persistence::database::Database;
use x_persistence::outbox::{OutboxError, enqueue_event};

use crate::SnapshotError;
use crate::envelopes::{PRODUCER, QueueError, entity};
use crate::public_post::{PublicPost, PublicPostFailure, PublicPostResolver};
use crate::social_sources::{EXPLICIT_PROVENANCE, SourceFact, SourceFacts, queue_source_event};

/// How long a claimed capture stays invisible to other workers while it is being resolved.
const LEASE: Duration = Duration::seconds(120);
/// The first retry delay; each further transient failure multiplies it by four.
const FIRST_RETRY_SECONDS: i64 = 30;
/// No retry waits longer than this.
const MAX_RETRY_SECONDS: i64 = 30 * 60;

/// Retry and batching limits of the worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapturePolicy {
    /// Resolution attempts before a transient failure becomes terminal.
    pub max_attempts: u32,
    /// Captures claimed per pass.
    pub batch_size: u32,
}

impl CapturePolicy {
    /// A policy with the given limits.
    #[must_use]
    pub const fn new(max_attempts: u32, batch_size: u32) -> Self {
        Self {
            max_attempts,
            batch_size,
        }
    }
}

/// What one worker pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaptureRunSummary {
    /// Captures this pass claimed.
    pub claimed: u32,
    /// Captures resolved and reported as preserved.
    pub preserved: u32,
    /// Captures that ended with a terminal unavailable report.
    pub unavailable: u32,
    /// Captures left queued for a later retry.
    pub deferred: u32,
}

/// Why a worker pass could not finish.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PublicCaptureError {
    /// A database statement failed.
    #[error("a public capture database statement failed")]
    Query(#[source] sqlx::Error),
    /// The social snapshot of the resolved post could not be built.
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),
    /// A social event or report envelope could not be built.
    #[error(transparent)]
    Queue(#[from] QueueError),
    /// An operation report could not be built.
    #[error(transparent)]
    Report(#[from] SocialContractError),
    /// A report could not be queued.
    #[error(transparent)]
    Outbox(#[from] OutboxError),
}

/// A capture claimed for resolution.
#[derive(Debug)]
struct ClaimedCapture {
    capture_id: Uuid,
    command_id: String,
    operation_id: Uuid,
    owner: Uuid,
    provider_post_id: String,
    attempts: i32,
    captured_at: DateTime<Utc>,
}

type ClaimedRow = (Uuid, String, Uuid, Uuid, String, i32, DateTime<Utc>);

impl From<ClaimedRow> for ClaimedCapture {
    fn from(row: ClaimedRow) -> Self {
        let (capture_id, command_id, operation_id, owner, provider_post_id, attempts, captured_at) =
            row;
        Self {
            capture_id,
            command_id,
            operation_id,
            owner,
            provider_post_id,
            attempts,
            captured_at,
        }
    }
}

/// Resolves queued explicit captures and reports each exactly once.
#[derive(Clone)]
pub struct PublicCaptureWorker {
    database: Database,
    resolver: Arc<dyn PublicPostResolver>,
    clock: Arc<dyn Clock>,
    policy: CapturePolicy,
}

impl std::fmt::Debug for PublicCaptureWorker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PublicCaptureWorker")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl PublicCaptureWorker {
    /// Builds the worker over its database, resolver, clock and policy.
    #[must_use]
    pub fn new(
        database: Database,
        resolver: Arc<dyn PublicPostResolver>,
        clock: Arc<dyn Clock>,
        policy: CapturePolicy,
    ) -> Self {
        Self {
            database,
            resolver,
            clock,
            policy,
        }
    }

    /// Claims the captures that are due and commits one outcome for each.
    ///
    /// # Errors
    /// [`PublicCaptureError`] when a statement or an envelope fails. A capture whose outcome did
    /// not commit stays queued and becomes due again when its lease ends. A resolved post the
    /// social contract cannot represent is not an error of the pass: that capture ends
    /// unavailable.
    pub async fn run_due_once(&self) -> Result<CaptureRunSummary, PublicCaptureError> {
        let claimed = self.claim_due().await?;
        let mut summary = CaptureRunSummary {
            claimed: u32::try_from(claimed.len()).unwrap_or(u32::MAX),
            ..CaptureRunSummary::default()
        };
        for capture in claimed {
            match self.resolver.resolve(&capture.provider_post_id).await {
                Ok(post) if post.provider_id == capture.provider_post_id => {
                    match self.preserve(&capture, &post).await {
                        Ok(()) => summary.preserved += 1,
                        // A post the shared social contract cannot represent (empty text, a
                        // control character, a handle outside the grammar) is data a user can
                        // influence, so it ends this one capture. Nothing was written: the
                        // snapshot is built before the first insert and the transaction rolled
                        // back when it was dropped.
                        Err(PublicCaptureError::Snapshot(SnapshotError::Contract(_))) => {
                            tracing::warn!(
                                capture_id = %capture.capture_id,
                                "the resolved post does not satisfy the social contract"
                            );
                            self.terminate(&capture, PublicPostFailure::Inaccessible)
                                .await?;
                            summary.unavailable += 1;
                        }
                        Err(error) => return Err(error),
                    }
                }
                // A different post than the one asked for is never published.
                Ok(_) => {
                    self.terminate(&capture, PublicPostFailure::Inaccessible)
                        .await?;
                    summary.unavailable += 1;
                }
                Err(PublicPostFailure::Transient) if self.has_attempts_left(&capture) => {
                    self.defer(&capture).await?;
                    summary.deferred += 1;
                }
                Err(failure) => {
                    self.terminate(&capture, failure).await?;
                    summary.unavailable += 1;
                }
            }
        }
        Ok(summary)
    }

    fn has_attempts_left(&self, capture: &ClaimedCapture) -> bool {
        u32::try_from(capture.attempts.saturating_add(1))
            .is_ok_and(|made| made < self.policy.max_attempts)
    }

    async fn claim_due(&self) -> Result<Vec<ClaimedCapture>, PublicCaptureError> {
        let now = self.clock.now();
        let rows: Vec<ClaimedRow> = sqlx::query_as(
            "with due as ( \
               select capture_id from x_archive.explicit_captures \
                where status = 'accepted' and provider_post_id is not null \
                  and next_attempt_at <= $1 \
                order by next_attempt_at, capture_id \
                limit $2 for update skip locked) \
             update x_archive.explicit_captures capture set next_attempt_at = $3 \
               from due where capture.capture_id = due.capture_id \
             returning capture.capture_id, capture.command_id, capture.operation_id, \
                       capture.owner, capture.provider_post_id, capture.attempts, \
                       capture.captured_at",
        )
        .bind(now)
        .bind(i64::from(self.policy.batch_size))
        .bind(now + LEASE)
        .fetch_all(self.database.pool())
        .await
        .map_err(PublicCaptureError::Query)?;
        Ok(rows.into_iter().map(ClaimedCapture::from).collect())
    }

    /// Leaves the capture queued with the next backoff delay.
    async fn defer(&self, capture: &ClaimedCapture) -> Result<(), PublicCaptureError> {
        let delay = retry_delay(capture.attempts.saturating_add(1));
        sqlx::query(
            "update x_archive.explicit_captures \
                set attempts = attempts + 1, next_attempt_at = $2 \
              where capture_id = $1 and status = 'accepted' and reported_at is null",
        )
        .bind(capture.capture_id)
        .bind(self.clock.now() + delay)
        .execute(self.database.pool())
        .await
        .map_err(PublicCaptureError::Query)?;
        Ok(())
    }

    /// Ends the capture as unavailable and queues its one terminal report.
    async fn terminate(
        &self,
        capture: &ClaimedCapture,
        failure: PublicPostFailure,
    ) -> Result<(), PublicCaptureError> {
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(PublicCaptureError::Query)?;
        let guarded = sqlx::query(
            "update x_archive.explicit_captures \
                set status = 'unavailable', attempts = attempts + 1, reported_at = $2 \
              where capture_id = $1 and status = 'accepted' and reported_at is null",
        )
        .bind(capture.capture_id)
        .bind(self.clock.now())
        .execute(&mut *transaction)
        .await
        .map_err(PublicCaptureError::Query)?
        .rows_affected()
            == 1;
        if guarded {
            let reason = match failure {
                PublicPostFailure::Deleted => SourceUnavailability::Deleted,
                PublicPostFailure::Inaccessible => SourceUnavailability::Inaccessible,
                PublicPostFailure::Transient => SourceUnavailability::Transient,
            };
            let report = unavailable_report(OperationId(capture.operation_id), reason)?;
            queue_report(&mut transaction, capture, &report).await?;
        }
        transaction
            .commit()
            .await
            .map_err(PublicCaptureError::Query)?;
        Ok(())
    }

    /// Publishes the owner's source and reports the capture preserved, in one transaction.
    async fn preserve(
        &self,
        capture: &ClaimedCapture,
        post: &PublicPost,
    ) -> Result<(), PublicCaptureError> {
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(PublicCaptureError::Query)?;
        sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!(
                "explicit_source:{}:{}",
                capture.owner, capture.provider_post_id
            ))
            .execute(&mut *transaction)
            .await
            .map_err(PublicCaptureError::Query)?;
        let source_id = upsert_owner_source(&mut transaction, capture, post).await?;
        let guarded = sqlx::query(
            "update x_archive.explicit_captures \
                set status = 'resolved', attempts = attempts + 1, social_source_id = $2, \
                    reported_at = $3 \
              where capture_id = $1 and status = 'accepted' and reported_at is null",
        )
        .bind(capture.capture_id)
        .bind(source_id)
        .bind(self.clock.now())
        .execute(&mut *transaction)
        .await
        .map_err(PublicCaptureError::Query)?
        .rows_affected()
            == 1;
        if guarded {
            let report =
                preserved_report(OperationId(capture.operation_id), SocialSourceId(source_id))?;
            queue_report(&mut transaction, capture, &report).await?;
            transaction
                .commit()
                .await
                .map_err(PublicCaptureError::Query)?;
        } else {
            // Another worker already reported this capture; nothing of ours may commit.
            transaction
                .rollback()
                .await
                .map_err(PublicCaptureError::Query)?;
        }
        Ok(())
    }
}

/// The delay before attempt `failures + 1`: 30 s, 2 min, 8 min, then 30 min.
fn retry_delay(failures: i32) -> Duration {
    let exponent = u32::try_from(failures.saturating_sub(1))
        .unwrap_or(0)
        .min(8);
    let seconds = FIRST_RETRY_SECONDS.saturating_mul(4_i64.pow(exponent));
    Duration::seconds(seconds.min(MAX_RETRY_SECONDS))
}

type OwnerSourceRow = (Uuid, String);

/// Inserts or revises the owner's source for the resolved post and queues the social event when
/// its content changed. Returns the source identity.
async fn upsert_owner_source(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    capture: &ClaimedCapture,
    post: &PublicPost,
) -> Result<Uuid, PublicCaptureError> {
    let facts = SourceFacts {
        provider_id: &post.provider_id,
        text: &post.text,
        long_text: post.long_text.as_deref(),
        published_at: post.published_at,
        availability: "active",
        author_provider_id: &post.author.id,
        author_username: post.author.username.as_deref(),
        author_display_name: post.author.name.as_deref(),
        expanded_urls: &post.expanded_urls,
    };
    let digest = facts.digest(EXPLICIT_PROVENANCE)?;
    let existing: Option<OwnerSourceRow> = sqlx::query_as(
        "select social_source_id, current_content_digest::text \
           from x_archive.explicit_sources where owner = $1 and provider_post_id = $2",
    )
    .bind(capture.owner)
    .bind(&capture.provider_post_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(PublicCaptureError::Query)?;
    let (source_id, fact) = match existing {
        Some((source_id, stored)) => {
            let unchanged = serde_json::from_str::<serde_json::Value>(&stored)
                .is_ok_and(|stored| stored == digest);
            if unchanged {
                return Ok(source_id);
            }
            (source_id, SourceFact::Updated)
        }
        None => (Uuid::now_v7(), SourceFact::Captured),
    };
    let snapshot = facts.snapshot(
        source_id,
        capture.owner,
        &digest,
        capture.captured_at,
        EXPLICIT_PROVENANCE,
    )?;
    sqlx::query(
        "insert into x_archive.explicit_sources \
           (social_source_id, owner, provider_post_id, author_provider_id, author_username, \
            author_display_name, text, long_text, published_at, expanded_urls, \
            current_content_digest, captured_at) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::jsonb, $11::jsonb, $12) \
         on conflict (owner, provider_post_id) do update set \
            author_provider_id = excluded.author_provider_id, \
            author_username = excluded.author_username, \
            author_display_name = excluded.author_display_name, text = excluded.text, \
            long_text = excluded.long_text, published_at = excluded.published_at, \
            expanded_urls = excluded.expanded_urls, \
            current_content_digest = excluded.current_content_digest, \
            captured_at = excluded.captured_at, updated_at = now()",
    )
    .bind(source_id)
    .bind(capture.owner)
    .bind(&capture.provider_post_id)
    .bind(&post.author.id)
    .bind(post.author.username.as_deref())
    .bind(post.author.name.as_deref())
    .bind(&post.text)
    .bind(post.long_text.as_deref())
    .bind(post.published_at)
    .bind(serde_json::json!(post.expanded_urls).to_string())
    .bind(digest.to_string())
    .bind(capture.captured_at)
    .execute(&mut **transaction)
    .await
    .map_err(PublicCaptureError::Query)?;
    queue_source_event(
        transaction,
        capture.owner,
        fact,
        snapshot,
        capture.captured_at,
    )
    .await?;
    Ok(source_id)
}

/// Queues the complete `platform.operation.reported.v1` envelope of one capture.
async fn queue_report(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    capture: &ClaimedCapture,
    report: &OperationReported,
) -> Result<(), PublicCaptureError> {
    let command_id = CommandId::parse(&capture.command_id).map_err(QueueError::from)?;
    let envelope = report_envelope(
        PRODUCER,
        UserId(capture.owner),
        OperationId(capture.operation_id),
        entity("capture", capture.capture_id)?,
        command_id,
        ratatoskr_identifiers::EventId::new_v7(),
        report,
    )?;
    enqueue_event(transaction, &envelope).await?;
    Ok(())
}
