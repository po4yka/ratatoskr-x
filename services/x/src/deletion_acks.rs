//! Consumption of Knowledge's deletion acknowledgements (XR-021 CONTRACTS.md R5-11).
//!
//! The `ratatoskr_events` stream drops its oldest messages, so a `social.source.removed.v1` fact
//! that Knowledge has not consumed in time would be lost. The relay therefore keeps publishing it
//! until Knowledge answers with `knowledge.deletion.acknowledged.v1`, and this consumer records
//! the answer. The durable `ratatoskr_x_deletion_acks` is provisioned by Edge on the events
//! stream; this consumer only verifies it. Every acknowledgement of every producer arrives here,
//! so one that names no removal of this service is acknowledged and ignored.
//!
//! A message that is not a valid acknowledgement from Knowledge cannot be attributed and
//! redelivery cannot fix it, so it is terminated. A database failure is redelivered.

use std::future::Future;
use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::AckKind;
use async_nats::jetstream::consumer::pull::MessagesErrorKind;
use futures_util::StreamExt as _;
use ratatoskr_event_envelope::EventEnvelope;
use ratatoskr_operation_contracts::{
    DeletionAckFamily, DeletionAckOutcome, KnowledgeDeletionAcknowledged,
};
use x_persistence::database::Database;
use x_persistence::outbox::{self, AckOutcome};

/// The Edge-provisioned durable of this consumer (S04 as amended by R5-11).
pub const DURABLE: &str = "ratatoskr_x_deletion_acks";

/// The only subject the durable is filtered to.
pub const FILTER_SUBJECT: &str = "evt.knowledge.deletion.acknowledged.v1";

/// The acknowledgement wait Edge provisions for the durable.
const ACK_WAIT: Duration = Duration::from_secs(30);

/// How long a transiently failed acknowledgement waits before redelivery.
const NAK_DELAY: Duration = Duration::from_secs(2);

/// Pause after a recoverable stream error, so a broken connection cannot spin the loop.
const RETRY_PAUSE: Duration = Duration::from_secs(1);

/// The only producer an acknowledgement may come from.
const KNOWLEDGE: &str = "ratatoskr-knowledge";

/// Totals from one consumer session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AcksSummary {
    /// Acknowledgements that ended the resend of a removal with outcome `applied`.
    pub applied: u64,
    /// Acknowledgements that ended the resend of a removal with outcome `refused`.
    pub refused: u64,
    /// Valid acknowledgements that matched no open removal of this service.
    pub ignored: u64,
    /// Messages that could not be attributed to Knowledge and were terminated.
    pub terminated: u64,
    /// Messages left for redelivery after a database failure.
    pub retried: u64,
}

/// Why the consumer could not start or had to stop.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AcksError {
    /// The broker refused the stream or durable lookup, or the durable does not match its spec.
    #[error("the deletion-acknowledgement consumer cannot use JetStream: {0}")]
    Bus(String),
    /// The message stream ended without a shutdown request.
    #[error("the deletion-acknowledgement consumer stopped without a shutdown request")]
    Stopped,
}

async fn acks_consumer(
    context: &jetstream::Context,
    stream_name: &str,
    durable_name: &str,
) -> Result<jetstream::consumer::Consumer<jetstream::consumer::pull::Config>, AcksError> {
    let consumer = context
        .get_consumer_from_stream(durable_name, stream_name)
        .await
        .map_err(|error| AcksError::Bus(error.to_string()))?;
    let config = &consumer.cached_info().config;
    if config.durable_name.as_deref() != Some(durable_name)
        || config.filter_subject != FILTER_SUBJECT
        || config.ack_policy != jetstream::consumer::AckPolicy::Explicit
        || config.ack_wait != ACK_WAIT
        || config.deliver_subject.is_some()
    {
        return Err(AcksError::Bus(
            "the Edge-provisioned deletion-acknowledgement durable is mismatched".to_owned(),
        ));
    }
    Ok(consumer)
}

/// Opens the Edge-provisioned durable and verifies its filter, ack policy and ack wait.
///
/// # Errors
/// [`AcksError::Bus`] when the stream or durable is absent or differs from its spec. This client
/// never creates a consumer: that could widen a compromised identity's filter.
pub async fn ensure_acks_consumer(
    context: &jetstream::Context,
    stream_name: &str,
    durable_name: &str,
) -> Result<(), AcksError> {
    acks_consumer(context, stream_name, durable_name).await?;
    Ok(())
}

/// What one valid message did.
enum Handled {
    Applied,
    Refused,
    Ignored,
}

/// Why one message was not applied.
enum Rejected {
    /// Not a valid acknowledgement from Knowledge.
    Unattributable,
    /// The database failed; the message is redelivered.
    Database,
}

/// Decodes and validates one message: the envelope must be Knowledge's
/// `knowledge.deletion.acknowledged.v1` for the tenant its payload names.
fn decode(payload: &[u8]) -> Option<KnowledgeDeletionAcknowledged> {
    let envelope = EventEnvelope::from_json(payload).ok()?;
    if envelope.producer.to_string() != KNOWLEDGE {
        return None;
    }
    let acknowledgement = envelope
        .payload_as::<KnowledgeDeletionAcknowledged>()
        .ok()?;
    (envelope.tenant_id.as_ref() == Some(&acknowledgement.owner)).then_some(acknowledgement)
}

async fn apply(database: &Database, payload: &[u8]) -> Result<Handled, Rejected> {
    let acknowledgement = decode(payload).ok_or(Rejected::Unattributable)?;
    // Social removals are the only facts this service publishes; another family names a row of a
    // sibling producer.
    if acknowledgement.family != DeletionAckFamily::Social {
        return Ok(Handled::Ignored);
    }
    let outcome = match acknowledgement.outcome {
        DeletionAckOutcome::Applied => AckOutcome::Applied,
        DeletionAckOutcome::Refused => AckOutcome::Refused,
        _ => return Err(Rejected::Unattributable),
    };
    let changed = outbox::acknowledge(
        database.pool(),
        acknowledgement.acknowledged_event_id.0,
        &acknowledgement.owner.to_string(),
        outcome,
    )
    .await
    .map_err(|error| {
        tracing::error!(%error, "recording a deletion acknowledgement failed");
        Rejected::Database
    })?;
    Ok(match (changed, outcome) {
        (false, _) => Handled::Ignored,
        (true, AckOutcome::Applied) => Handled::Applied,
        (true, AckOutcome::Refused) => {
            // Ids and the refusal class only: the data stays in Knowledge and an operator must look.
            tracing::error!(
                acknowledged_event_id = %acknowledgement.acknowledged_event_id.0,
                refusal = ?acknowledgement.refusal,
                "Knowledge refused a removal; the resend stopped and the data remains there"
            );
            metrics::counter!("x_tombstone_refused_total").increment(1);
            Handled::Refused
        }
    })
}

/// Runs the pull loop until `shutdown` resolves.
///
/// # Errors
/// [`AcksError`] when the durable cannot be opened, was deleted, or the message stream ended
/// without a shutdown request.
pub async fn consume_deletion_acks(
    context: &jetstream::Context,
    database: &Database,
    stream_name: &str,
    durable_name: &str,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<AcksSummary, AcksError> {
    let consumer = acks_consumer(context, stream_name, durable_name).await?;
    let mut messages = consumer
        .messages()
        .await
        .map_err(|error| AcksError::Bus(error.to_string()))?;
    // Registers the series so an operator sees zero before the first refusal.
    metrics::counter!("x_tombstone_refused_total").increment(0);
    let mut summary = AcksSummary::default();
    tokio::pin!(shutdown);

    loop {
        let next = tokio::select! {
            biased;
            () = &mut shutdown => return Ok(summary),
            next = messages.next() => next,
        };
        let message = match next {
            None => return Err(AcksError::Stopped),
            Some(Ok(message)) => message,
            Some(Err(error)) => {
                if matches!(
                    error.kind(),
                    MessagesErrorKind::ConsumerDeleted | MessagesErrorKind::PushBasedConsumer
                ) {
                    return Err(AcksError::Bus(error.to_string()));
                }
                tracing::warn!(%error, "the deletion-acknowledgement stream reported a recoverable error");
                tokio::select! {
                    biased;
                    () = &mut shutdown => return Ok(summary),
                    () = tokio::time::sleep(RETRY_PAUSE) => {}
                }
                continue;
            }
        };
        let kind = match apply(database, &message.payload).await {
            Ok(Handled::Applied) => {
                summary.applied += 1;
                AckKind::Ack
            }
            Ok(Handled::Refused) => {
                summary.refused += 1;
                AckKind::Ack
            }
            Ok(Handled::Ignored) => {
                summary.ignored += 1;
                AckKind::Ack
            }
            Err(Rejected::Unattributable) => {
                summary.terminated += 1;
                tracing::warn!(
                    "terminating a message that is not a valid Knowledge acknowledgement"
                );
                AckKind::Term
            }
            Err(Rejected::Database) => {
                summary.retried += 1;
                AckKind::Nak(Some(NAK_DELAY))
            }
        };
        if let Err(error) = message.ack_with(kind).await {
            tracing::warn!(%error, "the deletion-acknowledgement acknowledgement failed");
        }
    }
}
