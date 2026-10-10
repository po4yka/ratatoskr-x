//! Explicit browser-capture command validation for the X bounded context.

use std::future::Future;

use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::AckKind;
use async_nats::jetstream::consumer::pull::MessagesErrorKind;
use chrono::{DateTime, Utc};
use futures_util::StreamExt as _;
use ratatoskr_event_envelope::CommandEnvelope;
use ratatoskr_identifiers::{CommandId, EntityRef, EventId, OperationId, UserId};
use ratatoskr_operation_contracts::OperationReported;
use ratatoskr_social_contracts::{
    AcquisitionMethod, SavedAuthority, SocialCaptureProvider, SocialCaptureRequested,
    SocialContractError, SourceUnavailability, queued_report, report_envelope, unavailable_report,
};
use x_persistence::database::Database;
use x_persistence::outbox::{OutboxError, enqueue_event};

pub mod relay;

/// The producer name every X operation report carries.
const REPORT_PRODUCER: &str = "ratatoskr-x";

/// Pause after a recoverable stream error, so a broken connection cannot spin the loop.
const RETRY_PAUSE: Duration = Duration::from_secs(1);
/// How long the broker waits before redelivering a command whose persistence failed.
const REDELIVERY_DELAY: Duration = Duration::from_secs(5);

/// The only `JetStream` subject the X browser-capture consumer may receive.
pub const COMMAND_SUBJECT: &str = "cmd.x.capture.requested.v1";

/// Validates one Platform command before the X persistence boundary.
///
/// A command without a tenant names no owner, so it is a poison command: the explicit lane is
/// owner-scoped and has nobody to attribute the capture to.
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
    if command.tenant_id.is_none() {
        return Err(CaptureCommandError::MissingTenant);
    }
    Ok(())
}

/// The hosts whose status permalinks Platform accepts for X.
const STATUS_HOSTS: [&str; 6] = [
    "x.com",
    "www.x.com",
    "mobile.x.com",
    "twitter.com",
    "www.twitter.com",
    "mobile.twitter.com",
];

/// The status id a permalink names, when it is a status permalink on one of the six hosts
/// Platform accepts (`x.com`, `twitter.com` and their `www.` and `mobile.` forms).
///
/// The id is returned only when it is 1 to 19 decimal digits, the range the schema admits.
#[must_use]
pub fn status_id_from_permalink(permalink: &str) -> Option<&str> {
    let (scheme, rest) = permalink.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("https") && !scheme.eq_ignore_ascii_case("http") {
        return None;
    }
    let (host, path) = rest.split_once('/')?;
    if !STATUS_HOSTS
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(host))
    {
        return None;
    }
    let path = path.split(['?', '#']).next()?;
    let mut segments = path.split('/');
    segments.find(|segment| matches!(*segment, "status" | "statuses"))?;
    let id = segments.next()?;
    (matches!(id.len(), 1..=19) && id.bytes().all(|byte| byte.is_ascii_digit())).then_some(id)
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
/// In one transaction this claims the inbox row, stores the owner-scoped capture and queues the
/// operation report: `queued` for a capture the resolution worker will pick up, or a terminal
/// `failed` report for a permalink that names no status (so the operation never hangs).
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
    let owner = command
        .tenant_id
        .ok_or(CaptureCommandError::MissingTenant)?
        .user_id();
    let captured_at: DateTime<Utc> = capture
        .captured_at
        .to_wire()
        .parse()
        .map_err(|_| DeliveryError::InvalidTimestamp)?;
    let mut transaction = database
        .pool()
        .begin()
        .await
        .map_err(DeliveryError::Persistence)?;
    if !claim_inbox(&mut transaction, command).await? {
        transaction
            .rollback()
            .await
            .map_err(DeliveryError::Persistence)?;
        return Ok(Delivery::Duplicate);
    }

    let capture_id = uuid::Uuid::now_v7();
    let provider_post_id = status_id_from_permalink(capture.original_permalink.as_str());
    let report = match provider_post_id {
        Some(_) => queued_report(capture.operation_id)?,
        None => unavailable_report(capture.operation_id, SourceUnavailability::Inaccessible)?,
    };
    sqlx::query(
        "insert into x_archive.explicit_captures \
         (capture_id, command_id, operation_id, owner, provider_post_id, original_permalink, \
          captured_at, acquisition, saved_authority, status, reported_at) \
         values ($1, $2, $3, $4, $5, $6, $7, 'browser_extension', 'explicit_user_capture', $8, \
                 case when $5::text is null then now() end)",
    )
    .bind(capture_id)
    .bind(command.command_id.to_string())
    .bind(capture.operation_id.0)
    .bind(owner.0)
    .bind(provider_post_id)
    .bind(capture.original_permalink.as_str())
    .bind(captured_at)
    .bind(if provider_post_id.is_some() {
        "accepted"
    } else {
        "unavailable"
    })
    .execute(&mut *transaction)
    .await
    .map_err(DeliveryError::Persistence)?;
    queue_report(
        &mut transaction,
        ReportFacts {
            owner,
            operation: capture.operation_id,
            capture_id,
            command_id: command.command_id,
        },
        &report,
    )
    .await?;
    transaction
        .commit()
        .await
        .map_err(DeliveryError::Persistence)?;
    Ok(Delivery::Applied)
}

/// Claims the command delivery in the inbox; `false` means an earlier delivery already did.
async fn claim_inbox(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    command: &CommandEnvelope,
) -> Result<bool, DeliveryError> {
    let claimed: Option<(uuid::Uuid,)> = sqlx::query_as(
        "insert into x_archive.inbox_events (source, event_type, event_id, payload, consumed_at) \
         values ('platform', $1, $2, $3, now()) \
         on conflict (event_id) do nothing returning id",
    )
    .bind(command.command_type.to_wire())
    .bind(command.command_id.to_string())
    .bind(serde_json::to_value(command).map_err(DeliveryError::Serialize)?)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(DeliveryError::Persistence)?;
    Ok(claimed.is_some())
}

/// What identifies the operation report of one capture.
struct ReportFacts {
    owner: UserId,
    operation: OperationId,
    capture_id: uuid::Uuid,
    command_id: CommandId,
}

/// Queues the complete `platform.operation.reported.v1` envelope for `report`.
async fn queue_report(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    facts: ReportFacts,
    report: &OperationReported,
) -> Result<(), DeliveryError> {
    let aggregate = EntityRef::parse(&format!("capture:{}", facts.capture_id))
        .map_err(|_| DeliveryError::InvalidIdentifier)?;
    let envelope = report_envelope(
        REPORT_PRODUCER,
        facts.owner,
        facts.operation,
        aggregate,
        facts.command_id,
        EventId::new_v7(),
        report,
    )?;
    enqueue_event(transaction, &envelope).await?;
    Ok(())
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
    /// The command names no tenant, so no owner can be attributed.
    #[error("the social capture command names no tenant")]
    MissingTenant,
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
    /// A locally minted identifier failed its contract grammar.
    #[error("the X browser capture identifier is invalid")]
    InvalidIdentifier,
    /// The operation report could not be built.
    #[error("the X browser capture report cannot be built")]
    Report(#[from] SocialContractError),
    /// The operation report could not be queued.
    #[error("the X browser capture report cannot be queued")]
    Outbox(#[from] OutboxError),
}

impl DeliveryError {
    /// Whether redelivery can never change the outcome: the command itself is unacceptable.
    fn is_permanent(&self) -> bool {
        matches!(
            self,
            Self::Command(_) | Self::InvalidTimestamp | Self::Serialize(_)
        )
    }
}

/// Totals from one live `JetStream` consumer session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConsumerReport {
    /// Commands accepted by the durable X inbox.
    pub applied: u64,
    /// Redeliveries absorbed by the durable X inbox.
    pub duplicates: u64,
    /// Poison commands terminated after rejection.
    pub rejected: u64,
    /// Commands handed back to `JetStream` for a delayed redelivery after a transient failure.
    pub retryable_failures: u64,
}

/// Runs the X-owned durable pull consumer until `shutdown` resolves.
///
/// The Platform-owned command stream must already exist. This consumer never creates or mutates a
/// stream, so its broker authority is limited to one durable consumer and acknowledgements.
///
/// # Errors
///
/// Returns an error when `JetStream` cannot open the declared command stream or durable consumer,
/// when the durable is deleted, or when the message stream ends without a shutdown request.
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
            () = &mut shutdown => return Ok(report),
            next = messages.next() => next,
        };
        // The stream ending without a shutdown request means the consumer is gone: the caller
        // must see that as a fault instead of a quiet success.
        let Some(message) = message else {
            return Err(ConsumerError::Stopped);
        };
        let message = match message {
            Ok(message) => message,
            Err(error) => {
                if matches!(
                    error.kind(),
                    MessagesErrorKind::ConsumerDeleted | MessagesErrorKind::PushBasedConsumer
                ) {
                    return Err(ConsumerError::Bus(error.to_string()));
                }
                tracing::warn!(%error, "the social capture stream reported a recoverable error");
                tokio::select! {
                    biased;
                    () = &mut shutdown => return Ok(report),
                    () = tokio::time::sleep(RETRY_PAUSE) => {}
                }
                continue;
            }
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
            Err(error) if error.is_permanent() => {
                report.rejected += 1;
                tracing::warn!(%error, "terminating a poison social capture command");
                settle(&message, AckKind::Term).await;
            }
            Err(error) => {
                report.retryable_failures += 1;
                tracing::error!(%error, "leaving social capture command for redelivery");
                settle(&message, AckKind::Nak(Some(REDELIVERY_DELAY))).await;
            }
        }
    }
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
    settle(message, AckKind::Ack).await;
}

/// Tells the broker what became of `message`: stored, never acceptable (`Term`) or worth another
/// attempt after a pause (`Nak`).
async fn settle(message: &jetstream::Message, kind: AckKind) {
    if let Err(error) = message.ack_with(kind).await {
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
    /// The message stream ended without a shutdown request.
    #[error("the X browser-capture consumer stopped without a shutdown request")]
    Stopped,
}
