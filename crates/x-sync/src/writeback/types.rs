//! Public bookmark write-back contracts and closed failure vocabulary.

use std::future::Future;
use std::pin::Pin;

use chrono::{DateTime, Utc};
use x_budget::gate::BudgetError;

/// The complete and deliberately closed bookmark mutation surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BookmarkAction {
    /// Save one provider post to the connected account's bookmarks.
    Add,
    /// Remove one provider post from the connected account's bookmarks.
    Remove,
}

impl BookmarkAction {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Remove => "remove",
        }
    }
}

/// A validated bounded identifier for the trusted consent-recording surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentSurfaceId(String);

impl ConsentSurfaceId {
    /// Validates and owns one surface identifier.
    ///
    /// # Errors
    /// When the value is empty, over 64 bytes, or outside the closed lowercase vocabulary.
    pub fn parse(value: &str) -> Result<Self, BookmarkWritebackError> {
        let mut bytes = value.bytes();
        let first_valid = bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
        let rest_valid = bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        });
        if value.len() <= 64 && first_valid && rest_valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(BookmarkWritebackError::InvalidSurface)
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Immutable evidence returned after recording one explicit approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkWriteConsent {
    /// Generated single-action consent identity.
    pub id: uuid::Uuid,
    /// The authenticated owner who approved it.
    pub internal_user_id: uuid::Uuid,
    /// The connected account whose bookmark may change.
    pub account_id: uuid::Uuid,
    /// The exact approved action.
    pub action: BookmarkAction,
    /// The exact provider post identity.
    pub provider_post_id: String,
    /// Trusted approval observation supplied to the service.
    pub approved_at: DateTime<Utc>,
    /// Derived hard expiry.
    pub expires_at: DateTime<Utc>,
    /// Validated initiating surface.
    pub surface: ConsentSurfaceId,
}

/// One live bookmark-add request from an authenticated Ratatoskr owner.
#[derive(Debug, Clone, Copy)]
pub struct BookmarkWriteRequest<'a> {
    /// The authenticated internal owner requesting the mutation.
    pub internal_user_id: uuid::Uuid,
    /// The connected X account being mutated.
    pub account_id: uuid::Uuid,
    /// The explicit consent capability presented for this action.
    pub consent_id: uuid::Uuid,
    /// The provider post identity to bookmark.
    pub provider_post_id: &'a str,
    /// Caller-supplied retry identity; its raw value must never be persisted.
    pub idempotency_key: &'a str,
}

/// The terminal classification returned by a completed bookmark mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BookmarkWriteStatus {
    /// The provider mutation completed successfully.
    Succeeded,
    /// Durable local evidence already proved the requested bookmark state.
    AlreadySatisfied,
    /// The request may have reached the provider and awaits authoritative reconciliation.
    Uncertain,
    /// The provider confirmed success but the normalized target is not yet available locally.
    ProjectionPending,
    /// A complete snapshot proved the requested state is not current.
    ReconciledNotCurrent,
}

/// One stable operation identity and its stored terminal result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BookmarkWriteResult {
    /// The durable operation identity exact retries must share.
    pub operation_id: uuid::Uuid,
    /// The terminal result classification.
    pub status: BookmarkWriteStatus,
}

/// A closed local admission refusal reported by an advisory dry run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BookmarkAdmissionRefusal {
    /// The account is not owned by the authenticated Ratatoskr user.
    OwnershipMismatch,
    /// The connected account is not active.
    ConnectionInactive,
    /// No independently active local write authorization exists.
    WriteAuthorizationRequired,
    /// The credential or local authorization lacks a required scope.
    WriteScopeRequired,
    /// No matching unconsumed consent exists.
    ConsentRequired,
    /// The isolated bookmark-write budget cannot currently admit one request.
    BudgetExhausted,
}

/// The local outcome a dry-run bookmark action currently predicts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BookmarkDryRunOutcome {
    /// The local gates predict that the provider request would be submitted.
    WouldSubmit(BookmarkAction),
    /// Durable local evidence already proves the requested state.
    WouldAlreadySatisfy(BookmarkAction),
    /// One local admission gate would refuse the request.
    WouldRefuse(BookmarkAdmissionRefusal),
}

/// Advisory dry-run evidence without provider, consent, projection, or budget effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BookmarkDryRunResult {
    /// The typed local decision at evaluation time.
    pub outcome: BookmarkDryRunOutcome,
    /// The instant at which local admission was evaluated.
    pub evaluated_at: DateTime<Utc>,
    /// The durable bookmark observation that informed the decision, when one exists.
    pub observed_at: Option<DateTime<Utc>>,
    /// The inspected bookmark-write reset instant, when budget informed the decision.
    pub budget_reset_at: Option<DateTime<Utc>>,
    /// Always true: provider state may change after this local preview.
    pub advisory: bool,
}

/// The bounded official-provider bookmark mutation seam.
pub trait BookmarkMutationProvider: Send + Sync {
    /// Adds the target post to the connected account's bookmarks.
    fn add_bookmark<'a>(
        &'a self,
        account_id: uuid::Uuid,
        provider_post_id: &'a str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<BookmarkProviderSuccess, BookmarkProviderError>> + Send + 'a,
        >,
    >;

    /// Removes the target post from the connected account's bookmarks.
    fn remove_bookmark<'a>(
        &'a self,
        account_id: uuid::Uuid,
        provider_post_id: &'a str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<BookmarkProviderSuccess, BookmarkProviderError>> + Send + 'a,
        >,
    >;
}

/// Bounded, non-sensitive provider response evidence safe for operation/audit storage.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BookmarkProviderEvidence {
    /// Provider request identity after syntax and length validation.
    pub request_id: Option<String>,
}

/// A provider response that confirmed the requested bookmark state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkProviderSuccess {
    /// Non-sensitive response correlation evidence.
    pub evidence: BookmarkProviderEvidence,
}

/// A non-sensitive bookmark-provider failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BookmarkProviderError {
    /// The provider did not complete the requested mutation.
    #[error("the bookmark provider did not complete the mutation")]
    Unavailable,
    /// Owned account or credential persistence could not be read safely.
    #[error("the bookmark provider credential state is unavailable")]
    CredentialUnavailable,
    /// The persisted scope evidence is incomplete for bookmark mutation.
    #[error("the bookmark provider credential lacks required scope evidence")]
    CredentialScopeRequired,
    /// The account-bound credential envelope could not be opened or decoded.
    #[error("the bookmark provider credential envelope is invalid")]
    CredentialInvalid,
    /// A connect failure proved that no request reached the provider.
    #[error("the bookmark provider request failed transiently before contact")]
    Transient,
    /// Provider authentication or authorization is no longer usable.
    #[error("the bookmark provider authorization was lost")]
    AuthorizationLost {
        /// Non-sensitive response correlation evidence.
        evidence: BookmarkProviderEvidence,
    },
    /// The provider refused the current rate window.
    #[error("the bookmark provider rate limit was exhausted")]
    RateLimited {
        /// Provider epoch-seconds reset metadata, when valid.
        reset_epoch_seconds: Option<i64>,
        /// Non-sensitive response correlation evidence.
        evidence: BookmarkProviderEvidence,
    },
    /// A completed 4xx response definitively refused the request.
    #[error("the bookmark provider returned a definite refusal")]
    DefiniteRefusal {
        /// HTTP response status without response content.
        status: u16,
        /// Non-sensitive response correlation evidence.
        evidence: BookmarkProviderEvidence,
    },
    /// A request may have reached the provider, but its result is not known.
    #[error("the bookmark provider mutation result is uncertain")]
    Uncertain {
        /// HTTP response status when a response was received.
        status: Option<u16>,
        /// Non-sensitive response correlation evidence.
        evidence: BookmarkProviderEvidence,
    },
}

/// Why a bookmark write-back request could not complete.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BookmarkWritebackError {
    /// The account is not owned by the authenticated Ratatoskr user.
    #[error("the connected account is not owned by the authenticated user")]
    OwnershipMismatch,
    /// The account's read connection is not active.
    #[error("the connected account is not active")]
    ConnectionInactive,
    /// No independently active local write authorization exists.
    #[error("bookmark-write authorization is required")]
    WriteAuthorizationRequired,
    /// The active credential or local authorization lacks a required scope.
    #[error("the active credential lacks bookmark-write prerequisites")]
    WriteScopeRequired,
    /// No live one-action consent admits this owner/account/action/target tuple.
    #[error("the bookmark mutation has no matching live consent")]
    ConsentRequired,
    /// The caller reused an account-scoped idempotency key for different content.
    #[error("the idempotency key is already bound to a different request")]
    IdempotencyConflict,
    /// An exact request is already being executed by another caller or process.
    #[error("the idempotent bookmark operation is still in progress")]
    OperationInProgress,
    /// The caller supplied an empty or unreasonably large idempotency key.
    #[error("the idempotency key is invalid")]
    InvalidIdempotencyKey,
    /// Provider post identities are decimal strings of at most 19 digits.
    #[error("the provider post identity is invalid")]
    InvalidTarget,
    /// The consent surface is outside the bounded lowercase vocabulary.
    #[error("the consent surface identifier is invalid")]
    InvalidSurface,
    /// The supplied approval instant is future-dated or outside the configured lifetime.
    #[error("the consent approval instant is not currently admissible")]
    InvalidApprovalInstant,
    /// The isolated bookmark-write budget could not admit the provider request.
    #[error(transparent)]
    Budget(#[from] BudgetError),
    /// A budget refusal lost the reset evidence needed for a truthful result.
    #[error("bookmark-write budget refusal lacked reset evidence")]
    InvalidBudgetEvidence,
    /// The provider did not complete the mutation.
    #[error(transparent)]
    Provider(#[from] BookmarkProviderError),
    /// Owned persistence could not evaluate or record admission.
    #[error("bookmark write-back persistence failed")]
    Query(#[source] sqlx::Error),
}
