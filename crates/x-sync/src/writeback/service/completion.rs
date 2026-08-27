//! Terminal provider and projection transitions.

use chrono::{DateTime, Utc};

use super::super::types::{
    BookmarkAction, BookmarkProviderError, BookmarkProviderEvidence, BookmarkWriteStatus,
    BookmarkWritebackError,
};
use super::BookmarkWritebackService;
use super::support::{project_confirmed_bookmark, provider_failure_evidence};

impl BookmarkWritebackService {
    pub(crate) async fn complete_operation(
        &self,
        operation_id: uuid::Uuid,
        status: &str,
        outcome: &str,
        observed_at: Option<DateTime<Utc>>,
    ) -> Result<(), BookmarkWritebackError> {
        sqlx::query(
            "update x_archive.bookmark_write_operations \
             set status = $2, outcome = $3, projection_observed_at = $4, updated_at = $5 \
             where id = $1 and status = 'received'",
        )
        .bind(operation_id)
        .bind(status)
        .bind(outcome)
        .bind(observed_at)
        .bind(self.clock.now())
        .execute(self.database.pool())
        .await
        .map_err(BookmarkWritebackError::Query)?;
        if outcome == "already_satisfied" {
            self.append_operation_audit_details(
                operation_id,
                "gate_admitted",
                serde_json::json!({ "result": outcome }),
            )
            .await?;
            self.append_operation_audit_details(
                operation_id,
                "projection_reconciled",
                serde_json::json!({ "projection": outcome }),
            )
            .await?;
            self.append_operation_audit_details(
                operation_id,
                "operation_completed",
                serde_json::json!({ "result": outcome }),
            )
            .await?;
        }
        Ok(())
    }

    pub(crate) async fn complete_confirmed(
        &self,
        operation_id: uuid::Uuid,
        account_id: uuid::Uuid,
        provider_post_id: &str,
        action: BookmarkAction,
        evidence: BookmarkProviderEvidence,
    ) -> Result<BookmarkWriteStatus, BookmarkWritebackError> {
        let observed_at = self.clock.now();
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(BookmarkWritebackError::Query)?;
        let projection_updated = project_confirmed_bookmark(
            &mut transaction,
            operation_id,
            account_id,
            provider_post_id,
            action,
            observed_at,
        )
        .await?;
        let status = if projection_updated {
            "succeeded"
        } else {
            "projection_pending"
        };
        let request_id = evidence.request_id;
        sqlx::query(
            "update x_archive.bookmark_write_operations set status = $2, outcome = 'confirmed', \
             provider_request_id = $3, projection_observed_at = $4, updated_at = $5 \
             where id = $1 and status = 'provider_in_flight'",
        )
        .bind(operation_id)
        .bind(status)
        .bind(&request_id)
        .bind(projection_updated.then_some(observed_at))
        .bind(observed_at)
        .execute(&mut *transaction)
        .await
        .map_err(BookmarkWritebackError::Query)?;
        sqlx::query(
            "insert into x_archive.bookmark_write_audit_events \
             (id, account_id, operation_id, consent_id, internal_user_id, action, provider_post_id, \
              surface, event_class, occurred_at, correlation_id, idempotency_digest, \
              provider_request_id, details) \
             select encode(set_byte(uuid_send(gen_random_uuid()), 0, stage.ordinal), 'hex')::uuid, \
                    operation.account_id, operation.id, operation.consent_id, \
                    operation.internal_user_id, operation.action, operation.provider_post_id, \
                    consent.surface, stage.event_class, $2, operation.id::text, \
                    operation.idempotency_digest, $3, \
                    jsonb_build_object('result', 'confirmed', \
                      'projection', case when $4 then 'updated' else 'pending' end) \
             from x_archive.bookmark_write_operations operation \
             join x_archive.bookmark_write_consents consent on consent.id = operation.consent_id \
             cross join (values ('provider_classified', 4), ('projection_reconciled', 5), \
                                ('operation_completed', 6)) as stage(event_class, ordinal) \
             where operation.id = $1 order by stage.ordinal",
        )
        .bind(operation_id)
        .bind(observed_at)
        .bind(&request_id)
        .bind(projection_updated)
        .execute(&mut *transaction)
        .await
        .map_err(BookmarkWritebackError::Query)?;
        transaction
            .commit()
            .await
            .map_err(BookmarkWritebackError::Query)?;
        Ok(if projection_updated {
            BookmarkWriteStatus::Succeeded
        } else {
            BookmarkWriteStatus::ProjectionPending
        })
    }

    pub(crate) async fn complete_uncertain(
        &self,
        operation_id: uuid::Uuid,
        evidence: BookmarkProviderEvidence,
    ) -> Result<(), BookmarkWritebackError> {
        sqlx::query(
            "update x_archive.bookmark_write_operations \
             set status = 'uncertain', outcome = 'uncertain', provider_request_id = $2, \
                 updated_at = $3 where id = $1 and status = 'provider_in_flight'",
        )
        .bind(operation_id)
        .bind(evidence.request_id)
        .bind(self.clock.now())
        .execute(self.database.pool())
        .await
        .map_err(BookmarkWritebackError::Query)?;
        for event_class in [
            "provider_classified",
            "outcome_uncertain",
            "operation_completed",
        ] {
            self.append_operation_audit_details(
                operation_id,
                event_class,
                serde_json::json!({ "result": "uncertain" }),
            )
            .await?;
        }
        Ok(())
    }

    pub(crate) async fn complete_provider_failure(
        &self,
        operation_id: uuid::Uuid,
        error: &BookmarkProviderError,
    ) -> Result<(), BookmarkWritebackError> {
        let (outcome, request_id, reset_at) = provider_failure_evidence(error);
        sqlx::query(
            "update x_archive.bookmark_write_operations \
             set status = 'failed', outcome = $2, provider_request_id = $3, \
                 rate_limit_reset_at = $4, updated_at = $5 \
             where id = $1 and status = 'provider_in_flight'",
        )
        .bind(operation_id)
        .bind(outcome)
        .bind(request_id)
        .bind(reset_at)
        .bind(self.clock.now())
        .execute(self.database.pool())
        .await
        .map_err(BookmarkWritebackError::Query)?;
        self.append_operation_audit_details(
            operation_id,
            "provider_classified",
            serde_json::json!({ "result": outcome }),
        )
        .await?;
        self.append_operation_audit_details(
            operation_id,
            "operation_completed",
            serde_json::json!({ "result": outcome }),
        )
        .await
    }
}
