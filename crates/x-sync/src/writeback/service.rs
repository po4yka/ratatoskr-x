//! Bookmark write-back admission, durable execution, and audit coordination.

mod completion;
mod support;

use std::sync::Arc;

use chrono::{DateTime, TimeDelta, Utc};
use x_budget::gate::{BudgetError, BudgetGate, Clock};
use x_persistence::database::Database;

use super::types::{
    BookmarkAction, BookmarkAdmissionRefusal, BookmarkDryRunOutcome, BookmarkDryRunResult,
    BookmarkMutationProvider, BookmarkProviderError, BookmarkWriteConsent, BookmarkWriteRequest,
    BookmarkWriteResult, BookmarkWriteStatus, BookmarkWritebackError, ConsentSurfaceId,
};
use support::{
    LocalAdmission, OperationClaim, admission_refusal_error, audit_ordinal, dry_run_outcome_token,
    request_fingerprint, sha256_hex, stored_write_status, validate_provider_post_id,
};

/// Coordinates local admission and one official bookmark provider call.
#[derive(Clone)]
pub struct BookmarkWritebackService {
    database: Database,
    budget: BudgetGate,
    clock: Arc<dyn Clock>,
    consent_lifetime: TimeDelta,
}

impl std::fmt::Debug for BookmarkWritebackService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BookmarkWritebackService")
            .field("consent_lifetime", &self.consent_lifetime)
            .finish_non_exhaustive()
    }
}

impl BookmarkWritebackService {
    /// Wires the service to its owned database, isolated write budget, and clock.
    ///
    /// # Errors
    /// When the consent lifetime is not a positive representable number of seconds.
    pub fn new(
        database: Database,
        budget: BudgetGate,
        clock: Arc<dyn Clock>,
        consent_lifetime_seconds: i64,
    ) -> Result<Self, BookmarkWritebackError> {
        let Some(consent_lifetime) = TimeDelta::try_seconds(consent_lifetime_seconds) else {
            return Err(BookmarkWritebackError::InvalidApprovalInstant);
        };
        if consent_lifetime <= TimeDelta::zero() {
            return Err(BookmarkWritebackError::InvalidApprovalInstant);
        }
        Ok(Self {
            database,
            budget,
            clock,
            consent_lifetime,
        })
    }

    /// Records one immutable, short-lived approval without contacting X.
    ///
    /// # Errors
    /// When ownership, target, approval time, or persistence validation fails.
    pub async fn record_consent(
        &self,
        internal_user_id: uuid::Uuid,
        account_id: uuid::Uuid,
        action: BookmarkAction,
        provider_post_id: &str,
        approved_at: DateTime<Utc>,
        surface: ConsentSurfaceId,
    ) -> Result<BookmarkWriteConsent, BookmarkWritebackError> {
        validate_provider_post_id(provider_post_id)?;
        let state = self.account_state(internal_user_id, account_id).await?;
        if state != "connected" {
            return Err(BookmarkWritebackError::ConnectionInactive);
        }
        let now = self.clock.now();
        let Some(expires_at) = approved_at.checked_add_signed(self.consent_lifetime) else {
            return Err(BookmarkWritebackError::InvalidApprovalInstant);
        };
        if approved_at > now || expires_at <= now {
            return Err(BookmarkWritebackError::InvalidApprovalInstant);
        }
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(BookmarkWritebackError::Query)?;
        let id: uuid::Uuid = sqlx::query_scalar(
            "insert into x_archive.bookmark_write_consents \
             (account_id, internal_user_id, action, provider_post_id, approved_at, expires_at, surface) \
             values ($1, $2, $3, $4, $5, $6, $7) returning id",
        )
        .bind(account_id)
        .bind(internal_user_id)
        .bind(action.as_str())
        .bind(provider_post_id)
        .bind(approved_at)
        .bind(expires_at)
        .bind(surface.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(BookmarkWritebackError::Query)?;
        sqlx::query(
            "insert into x_archive.bookmark_write_audit_events \
             (id, account_id, consent_id, internal_user_id, action, provider_post_id, surface, \
              event_class, occurred_at, details) \
             values (encode(set_byte(uuid_send(gen_random_uuid()), 0, 0), 'hex')::uuid, \
                     $1, $2, $3, $4, $5, $6, 'consent_recorded', $7, \
                     jsonb_build_object('approved_at', $8::text, 'expires_at', $9::text))",
        )
        .bind(account_id)
        .bind(id)
        .bind(internal_user_id)
        .bind(action.as_str())
        .bind(provider_post_id)
        .bind(surface.as_str())
        .bind(now)
        .bind(approved_at.to_rfc3339())
        .bind(expires_at.to_rfc3339())
        .execute(&mut *transaction)
        .await
        .map_err(BookmarkWritebackError::Query)?;
        transaction
            .commit()
            .await
            .map_err(BookmarkWritebackError::Query)?;
        Ok(BookmarkWriteConsent {
            id,
            internal_user_id,
            account_id,
            action,
            provider_post_id: provider_post_id.to_owned(),
            approved_at,
            expires_at,
            surface,
        })
    }

    /// Executes one live bookmark add.
    ///
    /// # Errors
    /// When admission, budget reservation, or the provider mutation fails.
    pub async fn add_bookmark<P>(
        &self,
        request: BookmarkWriteRequest<'_>,
        provider: &P,
    ) -> Result<BookmarkWriteResult, BookmarkWritebackError>
    where
        P: BookmarkMutationProvider,
    {
        self.execute(request, BookmarkAction::Add, provider).await
    }

    /// Executes one live bookmark removal.
    ///
    /// # Errors
    /// When admission, budget reservation, or the provider mutation fails.
    pub async fn remove_bookmark<P>(
        &self,
        request: BookmarkWriteRequest<'_>,
        provider: &P,
    ) -> Result<BookmarkWriteResult, BookmarkWritebackError>
    where
        P: BookmarkMutationProvider,
    {
        self.execute(request, BookmarkAction::Remove, provider)
            .await
    }

    /// Previews one bookmark action without contacting the provider.
    ///
    /// # Errors
    /// When local admission evidence cannot be evaluated.
    pub async fn dry_run(
        &self,
        request: BookmarkWriteRequest<'_>,
        action: BookmarkAction,
    ) -> Result<BookmarkDryRunResult, BookmarkWritebackError> {
        validate_provider_post_id(request.provider_post_id)?;
        if request.idempotency_key.is_empty() || request.idempotency_key.len() > 256 {
            return Err(BookmarkWritebackError::InvalidIdempotencyKey);
        }
        let operation_id = match self.claim_operation(request, action, "dry_run").await? {
            OperationClaim::Execute(operation_id) => operation_id,
            OperationClaim::Replay(_) | OperationClaim::InProgress(_) => {
                return Err(BookmarkWritebackError::OperationInProgress);
            }
        };
        let evaluated_at = self.clock.now();
        let (outcome, observed_at, budget_reset_at) =
            match self.evaluate_local(request, action, evaluated_at).await? {
                LocalAdmission::Refused { class, reset_at } => {
                    (BookmarkDryRunOutcome::WouldRefuse(class), None, reset_at)
                }
                LocalAdmission::AlreadySatisfied { observed_at } => (
                    BookmarkDryRunOutcome::WouldAlreadySatisfy(action),
                    Some(observed_at),
                    None,
                ),
                LocalAdmission::Admitted { budget_reset_at } => (
                    BookmarkDryRunOutcome::WouldSubmit(action),
                    None,
                    Some(budget_reset_at),
                ),
            };
        let result = BookmarkDryRunResult {
            outcome,
            evaluated_at,
            observed_at,
            budget_reset_at,
            advisory: true,
        };
        self.complete_dry_run(operation_id, result).await?;
        Ok(result)
    }

    async fn execute<P>(
        &self,
        request: BookmarkWriteRequest<'_>,
        action: BookmarkAction,
        provider: &P,
    ) -> Result<BookmarkWriteResult, BookmarkWritebackError>
    where
        P: BookmarkMutationProvider,
    {
        validate_provider_post_id(request.provider_post_id)?;
        if request.idempotency_key.is_empty() || request.idempotency_key.len() > 256 {
            return Err(BookmarkWritebackError::InvalidIdempotencyKey);
        }
        let operation_id = match self.claim_operation(request, action, "live").await? {
            OperationClaim::Execute(operation_id) => operation_id,
            OperationClaim::Replay(result) => return Ok(result),
            OperationClaim::InProgress(operation_id) => {
                return self.wait_for_operation(operation_id).await;
            }
        };
        let now = self.clock.now();
        match self.evaluate_local(request, action, now).await? {
            LocalAdmission::Admitted { .. } => {}
            LocalAdmission::AlreadySatisfied { observed_at } => {
                self.complete_operation(
                    operation_id,
                    "succeeded",
                    "already_satisfied",
                    Some(observed_at),
                )
                .await?;
                return Ok(BookmarkWriteResult {
                    operation_id,
                    status: BookmarkWriteStatus::AlreadySatisfied,
                });
            }
            LocalAdmission::Refused { class, reset_at } => {
                self.refuse_operation(operation_id, class, reset_at).await?;
                return Err(admission_refusal_error(class, reset_at));
            }
        }
        let reservation = match self.budget.reserve(request.account_id, 1).await {
            Ok(reservation) => reservation,
            Err(error @ BudgetError::Exhausted { reset_at }) => {
                self.refuse_operation(
                    operation_id,
                    BookmarkAdmissionRefusal::BudgetExhausted,
                    Some(reset_at),
                )
                .await?;
                return Err(BookmarkWritebackError::Budget(error));
            }
            Err(error) => return Err(BookmarkWritebackError::Budget(error)),
        };
        self.consume_consent(operation_id, request, action, now, &reservation)
            .await?;
        let provider_outcome = match action {
            BookmarkAction::Add => {
                provider
                    .add_bookmark(request.account_id, request.provider_post_id)
                    .await
            }
            BookmarkAction::Remove => {
                provider
                    .remove_bookmark(request.account_id, request.provider_post_id)
                    .await
            }
        };
        let provider_success = match provider_outcome {
            Ok(success) => success,
            Err(BookmarkProviderError::Uncertain { evidence, .. }) => {
                self.complete_uncertain(operation_id, evidence).await?;
                return Ok(BookmarkWriteResult {
                    operation_id,
                    status: BookmarkWriteStatus::Uncertain,
                });
            }
            Err(error) => {
                self.complete_provider_failure(operation_id, &error).await?;
                return Err(BookmarkWritebackError::Provider(error));
            }
        };
        let status = self
            .complete_confirmed(
                operation_id,
                request.account_id,
                request.provider_post_id,
                action,
                provider_success.evidence,
            )
            .await?;
        Ok(BookmarkWriteResult {
            operation_id,
            status,
        })
    }

    async fn claim_operation(
        &self,
        request: BookmarkWriteRequest<'_>,
        action: BookmarkAction,
        execution_mode: &str,
    ) -> Result<OperationClaim, BookmarkWritebackError> {
        let idempotency_digest = sha256_hex(request.idempotency_key.as_bytes());
        let request_fingerprint = request_fingerprint(request, action, execution_mode);
        let operation_id: Option<uuid::Uuid> = sqlx::query_scalar(
            "insert into x_archive.bookmark_write_operations \
             (account_id, internal_user_id, consent_id, action, provider_post_id, \
              execution_mode, idempotency_digest, request_fingerprint, status) \
             values ($1, $2, (select id from x_archive.bookmark_write_consents where id = $3), \
                     $4, $5, $6, $7, $8, 'received') \
             on conflict (account_id, idempotency_digest) do nothing returning id",
        )
        .bind(request.account_id)
        .bind(request.internal_user_id)
        .bind(request.consent_id)
        .bind(action.as_str())
        .bind(request.provider_post_id)
        .bind(execution_mode)
        .bind(&idempotency_digest)
        .bind(&request_fingerprint)
        .fetch_optional(self.database.pool())
        .await
        .map_err(BookmarkWritebackError::Query)?;
        if let Some(operation_id) = operation_id {
            self.append_operation_audit(operation_id, "request_received")
                .await?;
            return Ok(OperationClaim::Execute(operation_id));
        }
        let existing: (uuid::Uuid, String, String) = sqlx::query_as(
            "select id, request_fingerprint, status \
             from x_archive.bookmark_write_operations \
             where account_id = $1 and idempotency_digest = $2",
        )
        .bind(request.account_id)
        .bind(idempotency_digest)
        .fetch_one(self.database.pool())
        .await
        .map_err(BookmarkWritebackError::Query)?;
        if existing.1 != request_fingerprint {
            self.append_conflict_audit(existing.0).await?;
            return Err(BookmarkWritebackError::IdempotencyConflict);
        }
        if let Some(status) = stored_write_status(&existing.2) {
            self.append_replay_audit(existing.0).await?;
            Ok(OperationClaim::Replay(BookmarkWriteResult {
                operation_id: existing.0,
                status,
            }))
        } else {
            Ok(OperationClaim::InProgress(existing.0))
        }
    }

    async fn evaluate_local(
        &self,
        request: BookmarkWriteRequest<'_>,
        action: BookmarkAction,
        evaluated_at: DateTime<Utc>,
    ) -> Result<LocalAdmission, BookmarkWritebackError> {
        let state = match self
            .account_state(request.internal_user_id, request.account_id)
            .await
        {
            Ok(state) => state,
            Err(BookmarkWritebackError::OwnershipMismatch) => {
                return Ok(LocalAdmission::Refused {
                    class: BookmarkAdmissionRefusal::OwnershipMismatch,
                    reset_at: None,
                });
            }
            Err(error) => return Err(error),
        };
        if state != "connected" {
            return Ok(LocalAdmission::Refused {
                class: BookmarkAdmissionRefusal::ConnectionInactive,
                reset_at: None,
            });
        }
        match self.require_write_authority(request.account_id).await {
            Ok(()) => {}
            Err(BookmarkWritebackError::WriteAuthorizationRequired) => {
                return Ok(LocalAdmission::Refused {
                    class: BookmarkAdmissionRefusal::WriteAuthorizationRequired,
                    reset_at: None,
                });
            }
            Err(BookmarkWritebackError::WriteScopeRequired) => {
                return Ok(LocalAdmission::Refused {
                    class: BookmarkAdmissionRefusal::WriteScopeRequired,
                    reset_at: None,
                });
            }
            Err(error) => return Err(error),
        }
        let consent_exists: bool = sqlx::query_scalar(
            "select exists(select 1 from x_archive.bookmark_write_consents \
             where id = $1 and internal_user_id = $2 and account_id = $3 \
               and action = $4 and provider_post_id = $5 \
               and consumed_at is null and expires_at > $6)",
        )
        .bind(request.consent_id)
        .bind(request.internal_user_id)
        .bind(request.account_id)
        .bind(action.as_str())
        .bind(request.provider_post_id)
        .bind(evaluated_at)
        .fetch_one(self.database.pool())
        .await
        .map_err(BookmarkWritebackError::Query)?;
        if !consent_exists {
            return Ok(LocalAdmission::Refused {
                class: BookmarkAdmissionRefusal::ConsentRequired,
                reset_at: None,
            });
        }
        let observation: Option<(DateTime<Utc>, Option<DateTime<Utc>>)> = sqlx::query_as(
            "select bookmark.last_observed_saved_at, bookmark.observed_removed_at \
             from x_archive.bookmarks bookmark \
             join x_archive.posts post on post.id = bookmark.post_id \
             where bookmark.account_id = $1 and post.provider_id = $2",
        )
        .bind(request.account_id)
        .bind(request.provider_post_id)
        .fetch_optional(self.database.pool())
        .await
        .map_err(BookmarkWritebackError::Query)?;
        if let Some((saved_at, removed_at)) = observation {
            match (action, removed_at) {
                (BookmarkAction::Add, None) => {
                    return Ok(LocalAdmission::AlreadySatisfied {
                        observed_at: saved_at,
                    });
                }
                (BookmarkAction::Remove, Some(observed_at)) => {
                    return Ok(LocalAdmission::AlreadySatisfied { observed_at });
                }
                _ => {}
            }
        }
        let inspection = self.budget.inspect(request.account_id, 1).await?;
        if inspection.eligible {
            Ok(LocalAdmission::Admitted {
                budget_reset_at: inspection.reset_at,
            })
        } else {
            Ok(LocalAdmission::Refused {
                class: BookmarkAdmissionRefusal::BudgetExhausted,
                reset_at: Some(inspection.reset_at),
            })
        }
    }

    async fn complete_dry_run(
        &self,
        operation_id: uuid::Uuid,
        result: BookmarkDryRunResult,
    ) -> Result<(), BookmarkWritebackError> {
        sqlx::query(
            "update x_archive.bookmark_write_operations \
             set status = 'dry_run', outcome = $2, rate_limit_reset_at = $3, \
                 projection_observed_at = $4, updated_at = $5 \
             where id = $1 and status = 'received'",
        )
        .bind(operation_id)
        .bind(dry_run_outcome_token(result.outcome))
        .bind(result.budget_reset_at)
        .bind(result.observed_at)
        .bind(result.evaluated_at)
        .execute(self.database.pool())
        .await
        .map_err(BookmarkWritebackError::Query)?;
        let (gate_event, gate_details) = match result.outcome {
            BookmarkDryRunOutcome::WouldRefuse(BookmarkAdmissionRefusal::BudgetExhausted) => (
                "budget_refused",
                serde_json::json!({
                    "mode": "dry_run",
                    "reset_at": result.budget_reset_at.map(|value| value.to_rfc3339()),
                }),
            ),
            BookmarkDryRunOutcome::WouldRefuse(refusal) => (
                "gate_refused",
                serde_json::json!({
                    "mode": "dry_run",
                    "refusal": dry_run_outcome_token(BookmarkDryRunOutcome::WouldRefuse(refusal)),
                }),
            ),
            _ => ("gate_admitted", serde_json::json!({ "mode": "dry_run" })),
        };
        self.append_operation_audit_details(operation_id, gate_event, gate_details)
            .await?;
        self.append_operation_audit(operation_id, "dry_run_completed")
            .await
    }

    async fn refuse_operation(
        &self,
        operation_id: uuid::Uuid,
        class: BookmarkAdmissionRefusal,
        reset_at: Option<DateTime<Utc>>,
    ) -> Result<(), BookmarkWritebackError> {
        let outcome = match class {
            BookmarkAdmissionRefusal::OwnershipMismatch => "ownership_mismatch",
            BookmarkAdmissionRefusal::ConnectionInactive => "connection_inactive",
            BookmarkAdmissionRefusal::WriteAuthorizationRequired => "write_authorization_required",
            BookmarkAdmissionRefusal::WriteScopeRequired => "write_scope_required",
            BookmarkAdmissionRefusal::ConsentRequired => "consent_required",
            BookmarkAdmissionRefusal::BudgetExhausted => "budget_exhausted",
        };
        sqlx::query(
            "update x_archive.bookmark_write_operations \
             set status = 'refused', outcome = $2, rate_limit_reset_at = $3, updated_at = $4 \
             where id = $1 and status = 'received'",
        )
        .bind(operation_id)
        .bind(outcome)
        .bind(reset_at)
        .bind(self.clock.now())
        .execute(self.database.pool())
        .await
        .map_err(BookmarkWritebackError::Query)?;
        let event_class = if class == BookmarkAdmissionRefusal::BudgetExhausted {
            "budget_refused"
        } else {
            "gate_refused"
        };
        let details = reset_at.map_or_else(
            || serde_json::json!({ "refusal": outcome }),
            |reset_at| {
                serde_json::json!({
                    "refusal": outcome,
                    "reset_at": reset_at.to_rfc3339(),
                })
            },
        );
        self.append_operation_audit_details(operation_id, event_class, details)
            .await
    }

    async fn wait_for_operation(
        &self,
        operation_id: uuid::Uuid,
    ) -> Result<BookmarkWriteResult, BookmarkWritebackError> {
        const MAX_POLLS: usize = 500;
        const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);
        for _ in 0..MAX_POLLS {
            let status: Option<String> = sqlx::query_scalar(
                "select status from x_archive.bookmark_write_operations where id = $1",
            )
            .bind(operation_id)
            .fetch_optional(self.database.pool())
            .await
            .map_err(BookmarkWritebackError::Query)?;
            if let Some(status) = status.as_deref().and_then(stored_write_status) {
                self.append_replay_audit(operation_id).await?;
                return Ok(BookmarkWriteResult {
                    operation_id,
                    status,
                });
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        Err(BookmarkWritebackError::OperationInProgress)
    }

    async fn append_conflict_audit(
        &self,
        operation_id: uuid::Uuid,
    ) -> Result<(), BookmarkWritebackError> {
        self.append_operation_audit(operation_id, "idempotency_conflict")
            .await
    }

    async fn append_replay_audit(
        &self,
        operation_id: uuid::Uuid,
    ) -> Result<(), BookmarkWritebackError> {
        self.append_operation_audit(operation_id, "idempotent_replay")
            .await
    }

    async fn append_operation_audit(
        &self,
        operation_id: uuid::Uuid,
        event_class: &str,
    ) -> Result<(), BookmarkWritebackError> {
        self.append_operation_audit_details(operation_id, event_class, serde_json::json!({}))
            .await
    }

    async fn append_operation_audit_details(
        &self,
        operation_id: uuid::Uuid,
        event_class: &str,
        details: serde_json::Value,
    ) -> Result<(), BookmarkWritebackError> {
        sqlx::query(
            "insert into x_archive.bookmark_write_audit_events \
             (id, account_id, operation_id, consent_id, internal_user_id, action, provider_post_id, \
              surface, event_class, occurred_at, correlation_id, idempotency_digest, \
              provider_request_id, details) \
             select encode(set_byte(uuid_send(gen_random_uuid()), 0, $5), 'hex')::uuid, \
                    operation.account_id, operation.id, operation.consent_id, \
                    operation.internal_user_id, operation.action, operation.provider_post_id, \
                    coalesce(consent.surface, 'unknown'), $2, $3, operation.id::text, \
                    operation.idempotency_digest, \
                    operation.provider_request_id, $4 \
             from x_archive.bookmark_write_operations operation \
             left join x_archive.bookmark_write_consents consent \
               on consent.id = operation.consent_id \
             where operation.id = $1",
        )
        .bind(operation_id)
        .bind(event_class)
        .bind(self.clock.now())
        .bind(details)
        .bind(audit_ordinal(event_class))
        .execute(self.database.pool())
        .await
        .map_err(BookmarkWritebackError::Query)?;
        Ok(())
    }

    async fn append_admission_audit(
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        operation_id: uuid::Uuid,
        occurred_at: DateTime<Utc>,
        cost: u32,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "insert into x_archive.bookmark_write_audit_events \
             (id, account_id, operation_id, consent_id, internal_user_id, action, provider_post_id, \
              surface, event_class, occurred_at, correlation_id, idempotency_digest, details) \
             select encode(set_byte(uuid_send(gen_random_uuid()), 0, stage.ordinal), 'hex')::uuid, \
                    operation.account_id, operation.id, operation.consent_id, \
                    operation.internal_user_id, operation.action, operation.provider_post_id, \
                    consent.surface, stage.event_class, $2, operation.id::text, \
                    operation.idempotency_digest, \
                    case when stage.event_class = 'provider_attempted' \
                         then jsonb_build_object('budget_class', 'bookmark_write', 'cost', $3::int) \
                         else '{}'::jsonb end \
             from x_archive.bookmark_write_operations operation \
             join x_archive.bookmark_write_consents consent on consent.id = operation.consent_id \
             cross join (values ('gate_admitted', 2), ('provider_attempted', 3)) \
                        as stage(event_class, ordinal) \
             where operation.id = $1 order by stage.ordinal",
        )
        .bind(operation_id)
        .bind(occurred_at)
        .bind(i32::try_from(cost).unwrap_or(i32::MAX))
        .execute(&mut **transaction)
        .await?;
        Ok(())
    }

    async fn consume_consent(
        &self,
        operation_id: uuid::Uuid,
        request: BookmarkWriteRequest<'_>,
        action: BookmarkAction,
        now: DateTime<Utc>,
        reservation: &x_budget::gate::Reservation,
    ) -> Result<(), BookmarkWritebackError> {
        let mut transaction = match self.database.pool().begin().await {
            Ok(transaction) => transaction,
            Err(error) => {
                self.refund_before_provider(request.account_id, reservation)
                    .await?;
                return Err(BookmarkWritebackError::Query(error));
            }
        };
        let locked = match sqlx::query_scalar::<_, uuid::Uuid>(
            "select id from x_archive.bookmark_write_consents \
             where id = $1 and internal_user_id = $2 and account_id = $3 \
               and action = $4 and provider_post_id = $5 \
               and consumed_at is null and expires_at > $6 for update",
        )
        .bind(request.consent_id)
        .bind(request.internal_user_id)
        .bind(request.account_id)
        .bind(action.as_str())
        .bind(request.provider_post_id)
        .bind(now)
        .fetch_optional(&mut *transaction)
        .await
        {
            Ok(locked) => locked,
            Err(error) => {
                drop(transaction);
                self.refund_before_provider(request.account_id, reservation)
                    .await?;
                return Err(BookmarkWritebackError::Query(error));
            }
        };
        if locked.is_none() {
            drop(transaction);
            self.refund_before_provider(request.account_id, reservation)
                .await?;
            return Err(BookmarkWritebackError::ConsentRequired);
        }
        if let Err(error) = sqlx::query(
            "update x_archive.bookmark_write_consents set consumed_at = $2 where id = $1",
        )
        .bind(request.consent_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        {
            drop(transaction);
            self.refund_before_provider(request.account_id, reservation)
                .await?;
            return Err(BookmarkWritebackError::Query(error));
        }
        if let Err(error) = sqlx::query(
            "update x_archive.bookmark_write_operations \
             set status = 'provider_in_flight', updated_at = $2 \
             where id = $1 and status = 'received'",
        )
        .bind(operation_id)
        .bind(now)
        .execute(&mut *transaction)
        .await
        {
            drop(transaction);
            self.refund_before_provider(request.account_id, reservation)
                .await?;
            return Err(BookmarkWritebackError::Query(error));
        }
        if let Err(error) =
            Self::append_admission_audit(&mut transaction, operation_id, now, reservation.cost)
                .await
        {
            drop(transaction);
            self.refund_before_provider(request.account_id, reservation)
                .await?;
            return Err(BookmarkWritebackError::Query(error));
        }
        if let Err(error) = transaction.commit().await {
            self.refund_before_provider(request.account_id, reservation)
                .await?;
            return Err(BookmarkWritebackError::Query(error));
        }
        Ok(())
    }

    async fn refund_before_provider(
        &self,
        account_id: uuid::Uuid,
        reservation: &x_budget::gate::Reservation,
    ) -> Result<(), BookmarkWritebackError> {
        self.budget
            .refund(account_id, reservation.window_start, reservation.cost)
            .await
            .map_err(BookmarkWritebackError::Budget)
    }

    async fn account_state(
        &self,
        internal_user_id: uuid::Uuid,
        account_id: uuid::Uuid,
    ) -> Result<String, BookmarkWritebackError> {
        sqlx::query_scalar(
            "select state from x_archive.accounts where id = $1 and internal_user_id = $2",
        )
        .bind(account_id)
        .bind(internal_user_id)
        .fetch_optional(self.database.pool())
        .await
        .map_err(BookmarkWritebackError::Query)?
        .ok_or(BookmarkWritebackError::OwnershipMismatch)
    }

    async fn require_write_authority(
        &self,
        account_id: uuid::Uuid,
    ) -> Result<(), BookmarkWritebackError> {
        let credential_scopes: Option<Vec<String>> = sqlx::query_scalar(
            "select granted_scopes from x_archive.credentials \
             where account_id = $1 and status = 'active' \
             order by created_at desc, id desc limit 1",
        )
        .bind(account_id)
        .fetch_optional(self.database.pool())
        .await
        .map_err(BookmarkWritebackError::Query)?;
        let Some(credential_scopes) = credential_scopes else {
            return Err(BookmarkWritebackError::WriteAuthorizationRequired);
        };
        let authorization: Option<(String, Vec<String>)> = sqlx::query_as(
            "select status, granted_scopes from x_archive.bookmark_write_authorizations \
             where account_id = $1",
        )
        .bind(account_id)
        .fetch_optional(self.database.pool())
        .await
        .map_err(BookmarkWritebackError::Query)?;
        let Some((status, authorization_scopes)) = authorization else {
            return Err(BookmarkWritebackError::WriteAuthorizationRequired);
        };
        if status != "active" {
            return Err(BookmarkWritebackError::WriteAuthorizationRequired);
        }
        let required = [
            "users.read",
            "tweet.read",
            "bookmark.read",
            "offline.access",
            "bookmark.write",
        ];
        if required.iter().any(|required_scope| {
            !credential_scopes
                .iter()
                .any(|scope| scope == required_scope)
                || !authorization_scopes
                    .iter()
                    .any(|scope| scope == required_scope)
        }) {
            return Err(BookmarkWritebackError::WriteScopeRequired);
        }
        Ok(())
    }
}
