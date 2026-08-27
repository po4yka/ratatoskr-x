//! Bounded upstream compliance revalidation for retained X sources.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use ratatoskr_event_envelope::EventPayload;
use ratatoskr_social_contracts::SocialSourceRemoved;
use serde_json::json;
use sqlx::types::Uuid;

use x_budget::gate::{BudgetError, BudgetGate, Clock};
use x_persistence::database::Database;

/// An authoritative provider observation for one preserved post.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComplianceAvailability {
    /// The connected user remains authorized to access the post.
    Available,
    /// The provider reports that the post was deleted.
    Deleted,
    /// The provider reports that the post is protected from this connection.
    Protected,
    /// The provider reports that the author is suspended.
    AuthorSuspended,
    /// The provider authoritatively reports another unavailable state.
    Unavailable,
}

/// Non-sensitive evidence returned with an authoritative observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComplianceObservation {
    /// The classified upstream authorization state.
    pub availability: ComplianceAvailability,
    /// Provider request identity suitable for diagnostics, when supplied.
    pub provider_request_id: Option<String>,
}

/// An indeterminate provider failure with no takedown authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ComplianceSourceError {
    /// The provider budget or endpoint rate limit refused the check.
    #[error("the provider rate-limited compliance revalidation")]
    RateLimited,
    /// The current connection no longer carries the required read scope.
    #[error("the provider authorization cannot revalidate this source")]
    AuthorizationLost,
    /// The provider failed transiently without authoritative source state.
    #[error("the provider temporarily failed compliance revalidation")]
    ProviderFailure,
    /// The returned evidence could not be classified safely.
    #[error("the provider returned invalid compliance evidence")]
    InvalidEvidence,
}

/// Official-provider seam for one post authorization check.
pub trait ComplianceRevalidationSource: Send + Sync {
    /// Revalidates one opaque provider post identity.
    fn revalidate<'a>(
        &'a self,
        provider_post_id: &'a str,
    ) -> Pin<
        Box<dyn Future<Output = Result<ComplianceObservation, ComplianceSourceError>> + Send + 'a>,
    >;
}

/// Summary of one bounded due-source invocation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ComplianceRunSummary {
    /// Provider calls that produced ledger evidence.
    pub checked: u32,
    /// Sources newly removed by authoritative observations.
    pub takedowns: u32,
    /// Whether the existing provider-request budget stopped the invocation.
    pub budget_exhausted: bool,
}

#[derive(Debug)]
struct RevalidationCandidate {
    source: Uuid,
    post: Uuid,
    provider_post: String,
    owner: Uuid,
}

#[derive(Debug)]
struct RevalidationEvidence {
    availability: Option<ComplianceAvailability>,
    provider_request_id: Option<String>,
    outcome: &'static str,
    failure_class: Option<&'static str>,
}

impl From<Result<ComplianceObservation, ComplianceSourceError>> for RevalidationEvidence {
    fn from(result: Result<ComplianceObservation, ComplianceSourceError>) -> Self {
        match result {
            Ok(observation) => Self {
                availability: Some(observation.availability),
                provider_request_id: observation.provider_request_id,
                outcome: availability_outcome(observation.availability),
                failure_class: None,
            },
            Err(error) => Self {
                availability: None,
                provider_request_id: None,
                outcome: "indeterminate",
                failure_class: Some(failure_token(error)),
            },
        }
    }
}

/// Compliance worker over the owned schema and existing request budget.
#[derive(Clone)]
pub struct ComplianceRevalidationService {
    database: Database,
    budget: BudgetGate,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for ComplianceRevalidationService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ComplianceRevalidationService")
            .finish_non_exhaustive()
    }
}

impl ComplianceRevalidationService {
    /// Builds a due-driven worker over existing database, budget, and clock boundaries.
    #[must_use]
    pub fn new(database: Database, budget: BudgetGate, clock: Arc<dyn Clock>) -> Self {
        Self {
            database,
            budget,
            clock,
        }
    }

    /// Revalidates at most `max_items` sources whose last ledger entry predates `due_before`.
    ///
    /// # Errors
    ///
    /// Returns [`ComplianceRevalidationError`] when due selection, budget persistence, or ledger
    /// persistence fails.
    pub async fn run_due<S>(
        &self,
        account_id: Uuid,
        source: &S,
        due_before: DateTime<Utc>,
        max_items: u32,
    ) -> Result<ComplianceRunSummary, ComplianceRevalidationError>
    where
        S: ComplianceRevalidationSource,
    {
        let candidates: Vec<(Uuid, Uuid, String, Uuid)> = sqlx::query_as(
            "select source.social_source_id, source.post_id, post.provider_id, \
                    account.internal_user_id \
             from x_archive.social_sources source \
             join x_archive.posts post on post.id = source.post_id \
             join x_archive.accounts account on account.id = source.account_id \
             left join lateral ( \
               select max(ledger.checked_at) as last_checked_at \
               from x_archive.compliance_revalidation_ledger ledger \
               where ledger.account_id = source.account_id \
                 and ledger.social_source_id = source.social_source_id \
             ) revalidation on true \
             where source.account_id = $1 and source.removed_at is null \
               and (revalidation.last_checked_at is null \
                 or revalidation.last_checked_at < $2) \
             order by revalidation.last_checked_at asc nulls first, source.social_source_id \
             limit $3",
        )
        .bind(account_id)
        .bind(due_before)
        .bind(i64::from(max_items))
        .fetch_all(self.database.pool())
        .await
        .map_err(ComplianceRevalidationError::Query)?;
        let mut summary = ComplianceRunSummary::default();

        for (social_source_id, post_id, provider_post_id, owner_id) in candidates {
            match self.budget.reserve(account_id, 1).await {
                Ok(_) => {}
                Err(BudgetError::Exhausted { .. }) => {
                    summary.budget_exhausted = true;
                    break;
                }
                Err(error) => return Err(ComplianceRevalidationError::Budget(error)),
            }
            let candidate = RevalidationCandidate {
                source: social_source_id,
                post: post_id,
                provider_post: provider_post_id,
                owner: owner_id,
            };
            let result = source.revalidate(&candidate.provider_post).await;
            let takedown = self
                .record_result(account_id, &candidate, result.into(), self.clock.now())
                .await?;
            summary.checked += 1;
            summary.takedowns += u32::from(takedown);
        }

        Ok(summary)
    }

    async fn record_result(
        &self,
        account_id: Uuid,
        candidate: &RevalidationCandidate,
        evidence: RevalidationEvidence,
        checked_at: DateTime<Utc>,
    ) -> Result<bool, ComplianceRevalidationError> {
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(ComplianceRevalidationError::Query)?;
        let revalidation_id: Uuid = sqlx::query_scalar(
            "insert into x_archive.compliance_revalidation_ledger \
               (account_id, post_id, social_source_id, provider_request_id, outcome, \
                failure_class, checked_at) \
             values ($1, $2, $3, $4, $5, $6, $7) returning id",
        )
        .bind(account_id)
        .bind(candidate.post)
        .bind(candidate.source)
        .bind(evidence.provider_request_id)
        .bind(evidence.outcome)
        .bind(evidence.failure_class)
        .bind(checked_at)
        .fetch_one(&mut *transaction)
        .await
        .map_err(ComplianceRevalidationError::Query)?;

        let mut takedown = false;
        if let Some(availability) = evidence.availability {
            sqlx::query(
                "update x_archive.posts set availability = $2, updated_at = now() where id = $1",
            )
            .bind(candidate.post)
            .bind(post_availability(availability))
            .execute(&mut *transaction)
            .await
            .map_err(ComplianceRevalidationError::Query)?;
            if availability != ComplianceAvailability::Available {
                takedown = remove_source(
                    &mut transaction,
                    account_id,
                    candidate,
                    availability,
                    revalidation_id,
                    checked_at,
                )
                .await?;
            }
        }

        transaction
            .commit()
            .await
            .map_err(ComplianceRevalidationError::Query)?;
        Ok(takedown)
    }
}

async fn remove_source(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    candidate: &RevalidationCandidate,
    availability: ComplianceAvailability,
    revalidation_id: Uuid,
    checked_at: DateTime<Utc>,
) -> Result<bool, ComplianceRevalidationError> {
    let removed = sqlx::query(
        "update x_archive.social_sources \
         set removed_at = $3, removal_reason = 'retention_policy', updated_at = now() \
         where account_id = $1 and social_source_id = $2 and removed_at is null",
    )
    .bind(account_id)
    .bind(candidate.source)
    .bind(checked_at)
    .execute(&mut **transaction)
    .await
    .map_err(ComplianceRevalidationError::Query)?
    .rows_affected()
        == 1;
    if !removed {
        return Ok(false);
    }

    sqlx::query(
        "insert into x_archive.tombstones \
           (account_id, social_source_id, post_provider_id, reason, revalidation_id, recorded_at) \
         values ($1, $2, $3, $4, $5, $6)",
    )
    .bind(account_id)
    .bind(candidate.source)
    .bind(&candidate.provider_post)
    .bind(availability_outcome(availability))
    .bind(revalidation_id)
    .bind(checked_at)
    .execute(&mut **transaction)
    .await
    .map_err(ComplianceRevalidationError::Query)?;
    let payload: SocialSourceRemoved = serde_json::from_value(json!({
        "social_source_id": candidate.source,
        "owner": format!("user:{}", candidate.owner),
        "reason": "retention_policy",
        "removed_at": checked_at.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
    }))
    .map_err(ComplianceRevalidationError::Contract)?;
    let payload = serde_json::to_value(payload).map_err(ComplianceRevalidationError::Contract)?;
    sqlx::query(
        "insert into x_archive.outbox_events \
           (aggregate, event_type, payload, correlation_id, causation_id) \
         values ($1, $2, $3, $4, $5)",
    )
    .bind(format!("social_source:{}", candidate.source))
    .bind(SocialSourceRemoved::EVENT_TYPE)
    .bind(payload)
    .bind(format!("social_source:{}", candidate.source))
    .bind(format!("compliance_revalidation:{revalidation_id}"))
    .execute(&mut **transaction)
    .await
    .map_err(ComplianceRevalidationError::Query)?;
    Ok(true)
}

const fn post_availability(availability: ComplianceAvailability) -> &'static str {
    match availability {
        ComplianceAvailability::Available => "active",
        ComplianceAvailability::Deleted => "deleted",
        ComplianceAvailability::Protected => "protected",
        ComplianceAvailability::AuthorSuspended => "author_suspended",
        ComplianceAvailability::Unavailable => "unavailable",
    }
}

const fn availability_outcome(availability: ComplianceAvailability) -> &'static str {
    match availability {
        ComplianceAvailability::Available => "available",
        ComplianceAvailability::Deleted => "deleted",
        ComplianceAvailability::Protected => "protected",
        ComplianceAvailability::AuthorSuspended => "author_suspended",
        ComplianceAvailability::Unavailable => "unavailable",
    }
}

const fn failure_token(error: ComplianceSourceError) -> &'static str {
    match error {
        ComplianceSourceError::RateLimited => "rate_limited",
        ComplianceSourceError::AuthorizationLost => "authorization_lost",
        ComplianceSourceError::ProviderFailure => "provider_failure",
        ComplianceSourceError::InvalidEvidence => "invalid_evidence",
    }
}

/// Compliance orchestration failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ComplianceRevalidationError {
    /// The existing durable provider budget failed.
    #[error(transparent)]
    Budget(#[from] BudgetError),
    /// X-owned due or evidence state could not be persisted.
    #[error("the compliance revalidation query failed")]
    Query(#[source] sqlx::Error),
    /// The shared removal contract could not represent takedown evidence.
    #[error("the compliance takedown payload is invalid")]
    Contract(#[source] serde_json::Error),
}
