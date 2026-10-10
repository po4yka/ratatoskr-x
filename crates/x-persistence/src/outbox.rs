//! The envelope outbox: the one place a producer row is written (XR-021 CONTRACTS.md S02).
//!
//! Every row stores the COMPLETE canonical envelope, and the row id is the envelope's own event
//! or command id. The relay publishes the stored JSON unchanged and uses the id as the broker
//! deduplication id, so the two can never disagree. Callers mint the envelope with the contract id
//! types and pass their open transaction, so the fact and its row commit together.

use ratatoskr_event_envelope::{CommandEnvelope, EventEnvelope};
use sqlx::PgConnection;

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
