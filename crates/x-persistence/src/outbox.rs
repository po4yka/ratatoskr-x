//! The envelope outbox: the one place a producer row is written (XR-021 CONTRACTS.md S02).
//!
//! Every row stores the COMPLETE canonical envelope, and the row id is the envelope's own event
//! or command id. The relay publishes the stored JSON unchanged and uses the id as the broker
//! deduplication id, so the two can never disagree. Callers mint the envelope with the contract id
//! types and pass their open transaction, so the fact and its row commit together.

use std::time::Duration;

use ratatoskr_event_envelope::{CommandEnvelope, EventEnvelope};
use sqlx::types::Uuid;
use sqlx::{PgConnection, PgPool};

/// The one event type Knowledge acknowledges (XR-021 CONTRACTS.md R5-11): a removal fact is kept and
/// published again until the acknowledgement arrives.
pub const REMOVAL_EVENT_TYPE: &str = "social.source.removed.v1";

/// Why an envelope could not be queued.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OutboxError {
    /// The envelope could not be encoded as JSON.
    #[error("the envelope could not be encoded")]
    Encode(#[source] serde_json::Error),
    /// The outbox insert failed, including a refusal by the closed `event_type` list.
    #[error("the outbox row could not be stored")]
    Query(#[source] sqlx::Error),
}

/// Queues one complete event envelope on the caller's transaction.
///
/// # Errors
/// [`OutboxError::Encode`] when the envelope cannot be encoded, [`OutboxError::Query`] when the
/// insert fails (an unlisted `event_type` or a repeated event id included).
pub async fn enqueue_event(
    connection: &mut PgConnection,
    envelope: &EventEnvelope,
) -> Result<(), OutboxError> {
    let payload = serde_json::to_string(envelope).map_err(OutboxError::Encode)?;
    insert(
        connection,
        &Row {
            id: envelope.event_id.0,
            aggregate: envelope.aggregate_id.to_wire(),
            event_type: envelope.event_type.to_wire(),
            payload,
            correlation_id: envelope.correlation_id.to_wire(),
            causation_id: envelope.causation_id.as_ref().map(ToString::to_string),
        },
    )
    .await
}

/// Queues one complete command envelope on the caller's transaction.
///
/// # Errors
/// As [`enqueue_event`].
pub async fn enqueue_command(
    connection: &mut PgConnection,
    envelope: &CommandEnvelope,
) -> Result<(), OutboxError> {
    let payload = serde_json::to_string(envelope).map_err(OutboxError::Encode)?;
    insert(
        connection,
        &Row {
            id: envelope.command_id.0,
            aggregate: envelope.aggregate_id.to_wire(),
            event_type: envelope.command_type.to_wire(),
            payload,
            correlation_id: envelope.correlation_id.to_wire(),
            causation_id: envelope.causation_id.as_ref().map(ToString::to_string),
        },
    )
    .await
}

struct Row {
    id: sqlx::types::Uuid,
    aggregate: String,
    event_type: String,
    payload: String,
    correlation_id: String,
    causation_id: Option<String>,
}

async fn insert(connection: &mut PgConnection, row: &Row) -> Result<(), OutboxError> {
    sqlx::query(
        "insert into x_archive.outbox_events \
           (id, aggregate, event_type, payload, correlation_id, causation_id) \
         values ($1, $2, $3, $4::jsonb, $5, $6)",
    )
    .bind(row.id)
    .bind(&row.aggregate)
    .bind(&row.event_type)
    .bind(&row.payload)
    .bind(&row.correlation_id)
    .bind(row.causation_id.as_deref())
    .execute(connection)
    .await
    .map_err(OutboxError::Query)?;
    Ok(())
}

/// How long a published removal waits for Knowledge before it is published again, and the longest
/// the wait between two copies grows to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResendPolicy {
    /// The delay before the first republish; each later delay doubles.
    pub after: Duration,
    /// The longest delay between two republishes.
    pub cap: Duration,
}

impl ResendPolicy {
    /// A policy from the configured delay and cap.
    #[must_use]
    pub fn new(after: Duration, cap: Duration) -> Self {
        Self { after, cap }
    }
}

/// What Knowledge did with a removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckOutcome {
    /// The removal was applied, or repeated one that had been.
    Applied,
    /// Knowledge refused it; the data stays there and an operator must look.
    Refused,
}

impl AckOutcome {
    /// The value stored in `ack_outcome`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Refused => "refused",
        }
    }
}

/// Marks a row published after the broker acknowledged it. A removal is also armed for its first
/// republish: `next_resend_at` becomes the publish time plus `policy.after`.
///
/// # Errors
/// [`OutboxError::Query`] when the update fails.
pub async fn mark_published(
    pool: &PgPool,
    id: Uuid,
    policy: ResendPolicy,
) -> Result<(), OutboxError> {
    sqlx::query(
        "update x_archive.outbox_events \
            set published_at = now(), last_error = null, \
                next_resend_at = case when event_type = $2 \
                                      then now() + make_interval(secs => $3) end \
          where id = $1 and published_at is null",
    )
    .bind(id)
    .bind(REMOVAL_EVENT_TYPE)
    .bind(policy.after.as_secs_f64())
    .execute(pool)
    .await
    .map_err(OutboxError::Query)?;
    Ok(())
}

/// Records one acknowledged republish of an unacknowledged removal: `resend_count` grows by one
/// and `next_resend_at` becomes now plus the least of `policy.after * 2^resend_count` and
/// `policy.cap`.
///
/// The exponent is clamped at 30, far beyond the point where the cap wins, so an operator who sets
/// a tiny delay and lets a removal wait for years cannot overflow the arithmetic.
///
/// # Errors
/// [`OutboxError::Query`] when the update fails.
pub async fn record_resend(
    pool: &PgPool,
    id: Uuid,
    policy: ResendPolicy,
) -> Result<(), OutboxError> {
    sqlx::query(
        "update x_archive.outbox_events \
            set resend_count = resend_count + 1, last_error = null, attempt_count = 0, \
                next_resend_at = now() + make_interval(secs => \
                    least($3::float8 * power(2::float8, least(resend_count, 30)::float8), $4::float8)) \
          where id = $1 and event_type = $2 and published_at is not null \
            and acknowledged_at is null",
    )
    .bind(id)
    .bind(REMOVAL_EVENT_TYPE)
    .bind(policy.after.as_secs_f64())
    .bind(policy.cap.as_secs_f64())
    .execute(pool)
    .await
    .map_err(OutboxError::Query)?;
    Ok(())
}

/// Records Knowledge's answer on the removal row `id` of the tenant `tenant` (the wire form of
/// the acknowledgement's `owner`). Returns whether a row changed: an unknown id, another tenant's
/// row, a row that is not a removal and an already acknowledged row change nothing.
///
/// # Errors
/// [`OutboxError::Query`] when the update fails.
pub async fn acknowledge(
    pool: &PgPool,
    id: Uuid,
    tenant: &str,
    outcome: AckOutcome,
) -> Result<bool, OutboxError> {
    let changed = sqlx::query(
        "update x_archive.outbox_events \
            set acknowledged_at = now(), ack_outcome = $4 \
          where id = $1 and event_type = $2 and payload ->> 'tenant_id' = $3 \
            and acknowledged_at is null",
    )
    .bind(id)
    .bind(REMOVAL_EVENT_TYPE)
    .bind(tenant)
    .bind(outcome.as_str())
    .execute(pool)
    .await
    .map_err(OutboxError::Query)?;
    Ok(changed.rows_affected() == 1)
}

/// The removals Knowledge has not acknowledged yet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Unacknowledged {
    /// How many there are.
    pub count: i64,
    /// Seconds since the oldest was queued, or zero when there is none.
    pub oldest_age_seconds: f64,
}

/// Counts the unacknowledged removals and ages the oldest.
///
/// # Errors
/// [`OutboxError::Query`] when the read fails.
pub async fn unacknowledged_removals(pool: &PgPool) -> Result<Unacknowledged, OutboxError> {
    let (count, oldest_age_seconds): (i64, f64) = sqlx::query_as(
        "select count(*), coalesce(extract(epoch from now() - min(created_at)), 0)::float8 \
           from x_archive.outbox_events \
          where event_type = $1 and acknowledged_at is null",
    )
    .bind(REMOVAL_EVENT_TYPE)
    .fetch_one(pool)
    .await
    .map_err(OutboxError::Query)?;
    Ok(Unacknowledged {
        count,
        oldest_age_seconds,
    })
}
