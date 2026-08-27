//! Closed state-machine and persistence helpers shared by write-back service modules.

use chrono::{DateTime, Utc};
use x_budget::gate::BudgetError;

use super::super::types::{
    BookmarkAction, BookmarkAdmissionRefusal, BookmarkDryRunOutcome, BookmarkProviderError,
    BookmarkWriteRequest, BookmarkWriteResult, BookmarkWriteStatus, BookmarkWritebackError,
};

pub(crate) async fn project_confirmed_bookmark(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    operation_id: uuid::Uuid,
    account_id: uuid::Uuid,
    provider_post_id: &str,
    action: BookmarkAction,
    observed_at: DateTime<Utc>,
) -> Result<bool, BookmarkWritebackError> {
    let post_id: Option<uuid::Uuid> =
        sqlx::query_scalar("select id from x_archive.posts where provider_id = $1")
            .bind(provider_post_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(BookmarkWritebackError::Query)?;
    let Some(post_id) = post_id else {
        return Ok(false);
    };

    match action {
        BookmarkAction::Add => {
            sqlx::query(
                "insert into x_archive.bookmarks \
                 (account_id, post_id, first_observed_saved_at, last_observed_saved_at, \
                  observed_removed_at, observed_removed_snapshot_id, last_write_operation_id, \
                  last_write_observed_at, observed_removed_write_operation_id) \
                 values ($1, $2, $3, $3, null, null, $4, $3, null) \
                 on conflict (account_id, post_id) do update set \
                   last_observed_saved_at = excluded.last_observed_saved_at, \
                   observed_removed_at = null, observed_removed_snapshot_id = null, \
                   last_write_operation_id = excluded.last_write_operation_id, \
                   last_write_observed_at = excluded.last_write_observed_at, \
                   observed_removed_write_operation_id = null",
            )
            .bind(account_id)
            .bind(post_id)
            .bind(observed_at)
            .bind(operation_id)
            .execute(&mut **transaction)
            .await
            .map_err(BookmarkWritebackError::Query)?;
            Ok(true)
        }
        BookmarkAction::Remove => {
            let updated = sqlx::query(
                "update x_archive.bookmarks set observed_removed_at = $3, \
                 observed_removed_snapshot_id = null, observed_removed_write_operation_id = $4 \
                 where account_id = $1 and post_id = $2",
            )
            .bind(account_id)
            .bind(post_id)
            .bind(observed_at)
            .bind(operation_id)
            .execute(&mut **transaction)
            .await
            .map_err(BookmarkWritebackError::Query)?;
            Ok(updated.rows_affected() == 1)
        }
    }
}

pub(crate) enum OperationClaim {
    Execute(uuid::Uuid),
    Replay(BookmarkWriteResult),
    InProgress(uuid::Uuid),
}

pub(crate) enum LocalAdmission {
    Admitted {
        budget_reset_at: DateTime<Utc>,
    },
    AlreadySatisfied {
        observed_at: DateTime<Utc>,
    },
    Refused {
        class: BookmarkAdmissionRefusal,
        reset_at: Option<DateTime<Utc>>,
    },
}

pub(crate) fn stored_write_status(status: &str) -> Option<BookmarkWriteStatus> {
    match status {
        "succeeded" | "reconciled_succeeded" => Some(BookmarkWriteStatus::Succeeded),
        "projection_pending" => Some(BookmarkWriteStatus::ProjectionPending),
        "uncertain" => Some(BookmarkWriteStatus::Uncertain),
        "reconciled_not_current" => Some(BookmarkWriteStatus::ReconciledNotCurrent),
        _ => None,
    }
}

pub(crate) fn provider_failure_evidence(
    error: &BookmarkProviderError,
) -> (&'static str, Option<String>, Option<DateTime<Utc>>) {
    match error {
        BookmarkProviderError::Unavailable => ("provider_unavailable", None, None),
        BookmarkProviderError::CredentialUnavailable => ("credential_unavailable", None, None),
        BookmarkProviderError::CredentialScopeRequired => ("credential_scope_required", None, None),
        BookmarkProviderError::CredentialInvalid => ("credential_invalid", None, None),
        BookmarkProviderError::Transient => ("transient_before_contact", None, None),
        BookmarkProviderError::AuthorizationLost { evidence } => {
            ("authorization_lost", evidence.request_id.clone(), None)
        }
        BookmarkProviderError::RateLimited {
            reset_epoch_seconds,
            evidence,
        } => (
            "rate_limited",
            evidence.request_id.clone(),
            reset_epoch_seconds.and_then(|seconds| DateTime::from_timestamp(seconds, 0)),
        ),
        BookmarkProviderError::DefiniteRefusal { evidence, .. } => {
            ("definite_refusal", evidence.request_id.clone(), None)
        }
        BookmarkProviderError::Uncertain { evidence, .. } => {
            ("uncertain", evidence.request_id.clone(), None)
        }
    }
}

pub(crate) const fn audit_ordinal(event_class: &str) -> i32 {
    match event_class.as_bytes() {
        b"request_received" => 1,
        b"gate_admitted" | b"gate_refused" | b"budget_refused" => 2,
        b"provider_attempted" => 3,
        b"provider_classified" => 4,
        b"projection_reconciled" | b"outcome_uncertain" => 5,
        b"operation_completed" => 6,
        b"idempotent_replay" | b"idempotency_conflict" | b"dry_run_completed" => 7,
        b"snapshot_reconciled" => 8,
        _ => 9,
    }
}

pub(crate) fn admission_refusal_error(
    refusal: BookmarkAdmissionRefusal,
    reset_at: Option<DateTime<Utc>>,
) -> BookmarkWritebackError {
    match refusal {
        BookmarkAdmissionRefusal::OwnershipMismatch => BookmarkWritebackError::OwnershipMismatch,
        BookmarkAdmissionRefusal::ConnectionInactive => BookmarkWritebackError::ConnectionInactive,
        BookmarkAdmissionRefusal::WriteAuthorizationRequired => {
            BookmarkWritebackError::WriteAuthorizationRequired
        }
        BookmarkAdmissionRefusal::WriteScopeRequired => BookmarkWritebackError::WriteScopeRequired,
        BookmarkAdmissionRefusal::ConsentRequired => BookmarkWritebackError::ConsentRequired,
        BookmarkAdmissionRefusal::BudgetExhausted => reset_at
            .map_or(BookmarkWritebackError::InvalidBudgetEvidence, |reset_at| {
                BookmarkWritebackError::Budget(BudgetError::Exhausted { reset_at })
            }),
    }
}

pub(crate) const fn dry_run_outcome_token(outcome: BookmarkDryRunOutcome) -> &'static str {
    match outcome {
        BookmarkDryRunOutcome::WouldSubmit(BookmarkAction::Add) => "would_add",
        BookmarkDryRunOutcome::WouldSubmit(BookmarkAction::Remove) => "would_remove",
        BookmarkDryRunOutcome::WouldAlreadySatisfy(BookmarkAction::Add) => "already_added",
        BookmarkDryRunOutcome::WouldAlreadySatisfy(BookmarkAction::Remove) => "already_removed",
        BookmarkDryRunOutcome::WouldRefuse(BookmarkAdmissionRefusal::OwnershipMismatch) => {
            "refused_ownership"
        }
        BookmarkDryRunOutcome::WouldRefuse(BookmarkAdmissionRefusal::ConnectionInactive) => {
            "refused_connection"
        }
        BookmarkDryRunOutcome::WouldRefuse(
            BookmarkAdmissionRefusal::WriteAuthorizationRequired,
        ) => "refused_authorization",
        BookmarkDryRunOutcome::WouldRefuse(BookmarkAdmissionRefusal::WriteScopeRequired) => {
            "refused_scope"
        }
        BookmarkDryRunOutcome::WouldRefuse(BookmarkAdmissionRefusal::ConsentRequired) => {
            "refused_consent"
        }
        BookmarkDryRunOutcome::WouldRefuse(BookmarkAdmissionRefusal::BudgetExhausted) => {
            "refused_budget"
        }
    }
}

pub(crate) fn request_fingerprint(
    request: BookmarkWriteRequest<'_>,
    action: BookmarkAction,
    execution_mode: &str,
) -> String {
    let owner = request.internal_user_id.to_string();
    let account = request.account_id.to_string();
    let consent = request.consent_id.to_string();
    fingerprint_parts(&[
        owner.as_bytes(),
        account.as_bytes(),
        action.as_str().as_bytes(),
        request.provider_post_id.as_bytes(),
        consent.as_bytes(),
        execution_mode.as_bytes(),
    ])
}

pub(crate) fn sha256_hex(value: &[u8]) -> String {
    use sha2::Digest as _;
    hex_digest(sha2::Sha256::digest(value).as_slice())
}

pub(crate) fn validate_provider_post_id(
    provider_post_id: &str,
) -> Result<(), BookmarkWritebackError> {
    if (1..=19).contains(&provider_post_id.len())
        && provider_post_id.bytes().all(|byte| byte.is_ascii_digit())
    {
        Ok(())
    } else {
        Err(BookmarkWritebackError::InvalidTarget)
    }
}

fn fingerprint_parts(parts: &[&[u8]]) -> String {
    use sha2::Digest as _;
    let mut digest = sha2::Sha256::new();
    digest.update(b"ratatoskr-x-bookmark-write\0");
    for part in parts {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part);
    }
    hex_digest(digest.finalize().as_slice())
}

fn hex_digest(value: &[u8]) -> String {
    let mut hex = String::with_capacity(value.len() * 2);
    for byte in value {
        let _ = std::fmt::Write::write_fmt(&mut hex, format_args!("{byte:02x}"));
    }
    hex
}
