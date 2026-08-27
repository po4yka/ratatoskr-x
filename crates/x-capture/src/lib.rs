//! Explicit browser-capture command validation for the X bounded context.

use std::future::Future;

use async_nats::jetstream;
use futures_util::StreamExt as _;
use ratatoskr_event_envelope::CommandEnvelope;
use ratatoskr_social_contracts::{
    AcquisitionMethod, SavedAuthority, SocialCaptureProvider, SocialCaptureRequested,
};
use x_persistence::database::Database;

/// The only `JetStream` subject the X browser-capture consumer may receive.
pub const COMMAND_SUBJECT: &str = "cmd.x.capture.requested.v1";

/// Validates one Platform command before the X persistence boundary.
///
/// The implementation is added through the red-green test cycle.
///
/// # Errors
///
/// Returns [`CaptureCommandError`] when the payload, provider, or closed provenance is invalid.
pub fn validate_browser_capture(command: &CommandEnvelope) -> Result<(), CaptureCommandError> {
    let capture = command
        .payload_as::<SocialCaptureRequested>()
        .map_err(|_| CaptureCommandError::InvalidPayload)?;
    if capture.provider != SocialCaptureProvider::X {
        return Err(CaptureCommandError::WrongProvider);
    }
    if capture.acquisition != AcquisitionMethod::BrowserExtension
        || capture.saved_authority != SavedAuthority::ExplicitUserCapture
    {
        return Err(CaptureCommandError::InvalidProvenance);
    }
    Ok(())
}

/// Whether a command delivery produced a new explicit capture or was already durable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// The command created the X explicit-capture record.
    Applied,
    /// A prior delivery already created the same record.
    Duplicate,
}

/// Persists an X browser capture with the command delivery as its durable deduplication key.
///
/// The transaction-backed implementation follows the red-green test cycle.
///
/// # Errors
///
/// Returns [`DeliveryError`] when validation, inbox/capture persistence, serialisation, or
/// timestamp conversion fails.
pub async fn persist_browser_capture(
    database: &Database,
    command: &CommandEnvelope,
) -> Result<Delivery, DeliveryError> {
    validate_browser_capture(command)?;
    let capture = command
        .payload_as::<SocialCaptureRequested>()
        .map_err(|_| CaptureCommandError::InvalidPayload)?;
    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(DeliveryError::Persistence)?;
    let claimed: Option<(uuid::Uuid,)> = sqlx::query_as(
        "insert into x_archive.inbox_events (source, event_type, event_id, payload, consumed_at) \
         values ('platform', $1, $2, $3, now()) \
         on conflict (event_id) do nothing returning id",
    )
    .bind(command.command_type.to_wire())
    .bind(command.command_id.to_string())
    .bind(serde_json::to_value(command).map_err(DeliveryError::Serialize)?)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(DeliveryError::Persistence)?;
    if claimed.is_none() {
        transaction
            .rollback()
            .await
            .map_err(DeliveryError::Persistence)?;
        return Ok(Delivery::Duplicate);
    }
    sqlx::query(
        "insert into x_archive.explicit_captures \
         (capture_id, command_id, operation_id, original_permalink, captured_at, acquisition, saved_authority) \
         values ($1, $2, $3, $4, $5, 'browser_extension', 'explicit_user_capture')",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(command.command_id.to_string())
    .bind(capture.operation_id.to_string())
    .bind(capture.original_permalink.as_str())
    .bind(capture.captured_at.to_wire().parse::<chrono::DateTime<chrono::Utc>>().map_err(|_| DeliveryError::InvalidTimestamp)?)
    .execute(&mut *transaction)
    .await
    .map_err(DeliveryError::Persistence)?;
    transaction
        .commit()
        .await
        .map_err(DeliveryError::Persistence)?;
    Ok(Delivery::Applied)
}

/// Why a browser social-capture command cannot enter the X bounded context.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CaptureCommandError {
    /// The command payload does not use the published social capture grammar.
    #[error("the command payload is not a social browser capture")]
    InvalidPayload,
    /// The command's provenance is not the explicit browser-capture lane.
    #[error("the social capture provenance is not an explicit browser capture")]
    InvalidProvenance,
    /// The command does not belong to X.
    #[error("the social capture command is not owned by X")]
    WrongProvider,
}

/// Why a command could not be durably accepted by the X owner.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DeliveryError {
    /// The command is not a valid X browser capture.
    #[error(transparent)]
    Command(#[from] CaptureCommandError),
    /// The X-owned transaction could not complete.
    #[error("the X browser capture could not be persisted")]
    Persistence(#[source] sqlx::Error),
    /// The serialised command cannot be retained in the inbox.
    #[error("the X browser capture command cannot be serialised")]
    Serialize(#[source] serde_json::Error),
    /// A contract timestamp failed conversion to the database representation.
    #[error("the X browser capture timestamp is invalid")]
    InvalidTimestamp,
}

/// Totals from one live `JetStream` consumer session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConsumerReport {
    /// Commands accepted by the durable X inbox.
    pub applied: u64,
    /// Redeliveries absorbed by the durable X inbox.
    pub duplicates: u64,
    /// Poison commands acknowledged after rejection.
    pub rejected: u64,
    /// Valid commands left unacknowledged for `JetStream` redelivery after a transient failure.
    pub retryable_failures: u64,
}

/// Runs the X-owned durable pull consumer until `shutdown` resolves.
///
/// The Platform-owned command stream must already exist. This consumer never creates or mutates a
/// stream, so its broker authority is limited to one durable consumer and acknowledgements.
///
/// # Errors
///
/// Returns an error when `JetStream` cannot open the declared command stream or durable consumer.
pub async fn consume_browser_commands(
    context: &jetstream::Context,
    database: &Database,
    stream_name: &str,
    durable_name: &str,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<ConsumerReport, ConsumerError> {
    let consumer = browser_consumer(context, stream_name, durable_name).await?;
    let mut messages = consumer
        .messages()
        .await
        .map_err(|error| ConsumerError::Bus(error.to_string()))?;
    let mut report = ConsumerReport::default();
    tokio::pin!(shutdown);

    loop {
        let message = tokio::select! {
            biased;
            () = &mut shutdown => break,
            next = messages.next() => next,
        };
        let Some(message) = message else { break };
        let Ok(message) = message else {
            report.rejected += 1;
            continue;
        };
        let disposition = match CommandEnvelope::from_json(&message.payload) {
            Ok(command) => persist_browser_capture(database, &command).await,
            Err(_) => Err(DeliveryError::Command(CaptureCommandError::InvalidPayload)),
        };
        match disposition {
            Ok(Delivery::Applied) => {
                report.applied += 1;
                acknowledge(&message).await;
            }
            Ok(Delivery::Duplicate) => {
                report.duplicates += 1;
                acknowledge(&message).await;
            }
            Err(DeliveryError::Command(error)) => {
                report.rejected += 1;
                tracing::warn!(%error, "rejecting a poison social capture command");
                acknowledge(&message).await;
            }
            Err(error) => {
                report.retryable_failures += 1;
                tracing::error!(%error, "leaving social capture command for redelivery");
            }
        }
    }
    Ok(report)
}

/// Opens the Platform-preprovisioned X durable pull consumer before the service reports readiness.
///
/// # Errors
///
/// Returns an error when the Platform-owned command stream or its fixed durable is absent or
/// mismatched. This client deliberately has no authority to create a consumer: that could widen a
/// compromised identity's command filter.
pub async fn ensure_browser_consumer(
    context: &jetstream::Context,
    stream_name: &str,
    durable_name: &str,
) -> Result<(), ConsumerError> {
    browser_consumer(context, stream_name, durable_name).await?;
    Ok(())
}

async fn browser_consumer(
    context: &jetstream::Context,
    stream_name: &str,
    durable_name: &str,
) -> Result<
    async_nats::jetstream::consumer::Consumer<jetstream::consumer::pull::Config>,
    ConsumerError,
> {
    let consumer = context
        .get_consumer_from_stream(durable_name, stream_name)
        .await
        .map_err(|error| ConsumerError::Bus(error.to_string()))?;
    let config = &consumer.cached_info().config;
    if config.durable_name.as_deref() != Some(durable_name)
        || config.filter_subject != COMMAND_SUBJECT
        || config.ack_policy != jetstream::consumer::AckPolicy::Explicit
        || config.deliver_subject.is_some()
    {
        return Err(ConsumerError::Bus(
            "the Platform-preprovisioned X browser-capture consumer is mismatched".to_owned(),
        ));
    }
    Ok(consumer)
}

async fn acknowledge(message: &jetstream::Message) {
    if let Err(error) = message.ack().await {
        tracing::warn!(%error, "the social capture command acknowledgement failed");
    }
}

/// Why the live X browser-capture consumer could not start.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConsumerError {
    /// `JetStream` refused the stream or durable-consumer operation.
    #[error("the X browser-capture consumer cannot use JetStream: {0}")]
    Bus(String),
}
