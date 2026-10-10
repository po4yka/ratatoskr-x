//! Complete canonical envelopes for the X outbox (XR-021 CONTRACTS.md S02 and S10).
//!
//! Every producer in this crate builds its envelope here and queues it through
//! [`x_persistence::outbox`], so the producer name, the tenant form, the timestamp spelling and the
//! row-id-equals-envelope-id rule have exactly one implementation.

use chrono::{DateTime, SecondsFormat, Utc};
use ratatoskr_event_envelope::{
    CommandEnvelope, CommandPayload, EnvelopeSchemaVersion, EventEnvelope, EventPayload,
    ProducerName,
};
use ratatoskr_identifiers::{
    CommandId, EntityRef, EventId, Extensions, IdentifierError, TenantRef, UserId, WireTimestamp,
};
use sqlx::PgConnection;
use sqlx::types::Uuid;
use x_persistence::outbox::{OutboxError, enqueue_command, enqueue_event};

/// The producer name every X envelope carries.
pub(crate) const PRODUCER: &str = "ratatoskr-x";

/// Why an envelope could not be built or queued.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum QueueError {
    /// An identifier or timestamp does not satisfy its contract grammar.
    #[error("an envelope identifier is invalid")]
    Identifier(#[source] IdentifierError),
    /// The payload could not be encoded into its envelope.
    #[error("an envelope payload could not be encoded: {0}")]
    Payload(String),
    /// The outbox refused the row.
    #[error(transparent)]
    Outbox(#[from] OutboxError),
}

impl From<IdentifierError> for QueueError {
    fn from(error: IdentifierError) -> Self {
        Self::Identifier(error)
    }
}

/// The facts every envelope of one outbox row shares.
#[derive(Debug, Clone)]
pub(crate) struct EnvelopeFacts {
    /// The entity the fact or command is about.
    pub(crate) aggregate: EntityRef,
    /// The unit of work the row belongs to.
    pub(crate) correlation: EntityRef,
    /// The record that directly caused the row, when there is one.
    pub(crate) causation: Option<EntityRef>,
    /// The tenant that owns the data.
    pub(crate) owner: Uuid,
    /// The instant the fact became true.
    pub(crate) occurred_at: DateTime<Utc>,
}

/// `<kind>:<id>` as an [`EntityRef`].
pub(crate) fn entity(kind: &str, id: impl std::fmt::Display) -> Result<EntityRef, QueueError> {
    Ok(EntityRef::parse(&format!("{kind}:{id}"))?)
}

/// The tenant reference of an owner.
pub(crate) fn tenant(owner: Uuid) -> TenantRef {
    TenantRef::of_user(UserId(owner))
}

/// An instant in the canonical contract spelling: RFC 3339 UTC, a literal `Z`, the fraction
/// trimmed of trailing zeros and absent when zero.
pub(crate) fn wire_timestamp(value: DateTime<Utc>) -> String {
    let nanos = value.to_rfc3339_opts(SecondsFormat::Nanos, true);
    let body = nanos.strip_suffix('Z').unwrap_or(&nanos);
    let body = body.trim_end_matches('0').trim_end_matches('.');
    format!("{body}Z")
}

fn instant(value: DateTime<Utc>) -> Result<WireTimestamp, QueueError> {
    Ok(WireTimestamp::parse(&wire_timestamp(value))?)
}

/// Builds the complete event envelope for `payload` with a freshly minted `UUIDv7` event id.
pub(crate) fn event_envelope<P: EventPayload>(
    facts: EnvelopeFacts,
    payload: &P,
) -> Result<EventEnvelope, QueueError> {
    let mut envelope = EventEnvelope {
        event_id: EventId::new_v7(),
        event_type: P::event_type(),
        occurred_at: instant(facts.occurred_at)?,
        producer: ProducerName::parse(PRODUCER)?,
        aggregate_id: facts.aggregate,
        correlation_id: facts.correlation,
        causation_id: facts.causation,
        tenant_id: Some(tenant(facts.owner)),
        schema_version: EnvelopeSchemaVersion::CURRENT,
        payload: serde_json::Map::new(),
        extensions: Extensions::new(),
    };
    envelope
        .set_payload(payload)
        .map_err(|error| QueueError::Payload(error.to_string()))?;
    Ok(envelope)
}

/// Builds and queues one complete event envelope on the caller's transaction.
pub(crate) async fn queue_event<P: EventPayload>(
    connection: &mut PgConnection,
    facts: EnvelopeFacts,
    payload: &P,
) -> Result<EventEnvelope, QueueError> {
    let envelope = event_envelope(facts, payload)?;
    enqueue_event(connection, &envelope).await?;
    Ok(envelope)
}

/// Builds and queues one complete command envelope on the caller's transaction.
pub(crate) async fn queue_command<P: CommandPayload>(
    connection: &mut PgConnection,
    facts: EnvelopeFacts,
    payload: &P,
) -> Result<CommandEnvelope, QueueError> {
    let mut envelope = CommandEnvelope {
        command_id: CommandId::new_v7(),
        command_type: P::command_type(),
        issued_at: instant(facts.occurred_at)?,
        producer: ProducerName::parse(PRODUCER)?,
        aggregate_id: facts.aggregate,
        correlation_id: facts.correlation,
        causation_id: facts.causation,
        tenant_id: Some(tenant(facts.owner)),
        schema_version: EnvelopeSchemaVersion::CURRENT,
        payload: serde_json::Map::new(),
        extensions: Extensions::new(),
    };
    envelope
        .set_payload(payload)
        .map_err(|error| QueueError::Payload(error.to_string()))?;
    enqueue_command(connection, &envelope).await?;
    Ok(envelope)
}
