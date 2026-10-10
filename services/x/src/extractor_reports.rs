//! Consumption of operation reports for linked-article captures (XR-021 CONTRACTS.md S04 and S10).
//!
//! X asks the extractor to capture an article through `content.capture.requested.v1` and learns the
//! outcome from the extractor's `platform.operation.reported.v1`. The durable
//! `ratatoskr_x_extractor_reports` is provisioned by Edge on the events stream; this consumer only
//! verifies it. Every other producer's report on the same subject is acknowledged and ignored, so
//! the consumer never holds up the shared subject.

use std::future::Future;
use std::time::Duration;

use async_nats::jetstream;
use async_nats::jetstream::AckKind;
use async_nats::jetstream::consumer::pull::MessagesErrorKind;
use futures_util::StreamExt as _;
use x_persistence::database::Database;
use x_sync::{ArticleCaptureError, ArticleCaptureService};

/// The Edge-provisioned durable of this consumer (S04).
pub const DURABLE: &str = "ratatoskr_x_extractor_reports";

/// The only subject the durable is filtered to (S04).
pub const FILTER_SUBJECT: &str = "evt.platform.operation.reported.v1";

/// The acknowledgement wait Edge provisions for the durable (S04).
const ACK_WAIT: Duration = Duration::from_secs(30);

/// How long a transiently failed report waits before redelivery.
const NAK_DELAY: Duration = Duration::from_secs(2);

/// Pause after a recoverable stream error, so a broken connection cannot spin the loop.
const RETRY_PAUSE: Duration = Duration::from_secs(1);

/// Totals from one consumer session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReportsSummary {
    /// Reports that completed a linked-article capture.
    pub applied: u64,
    /// Reports that were not ours, not valid, or for an unknown operation (acknowledged).
    pub ignored: u64,
    /// Reports left for redelivery after a database failure.
    pub retried: u64,
}

/// Why the consumer could not start or had to stop.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReportsError {
    /// The broker refused the stream or durable lookup, or the durable does not match its spec.
    #[error("the extractor-report consumer cannot use JetStream: {0}")]
    Bus(String),
    /// The message stream ended without a shutdown request.
    #[error("the extractor-report consumer stopped without a shutdown request")]
    Stopped,
}

async fn reports_consumer(
    context: &jetstream::Context,
    stream_name: &str,
    durable_name: &str,
) -> Result<jetstream::consumer::Consumer<jetstream::consumer::pull::Config>, ReportsError> {
    let consumer = context
        .get_consumer_from_stream(durable_name, stream_name)
        .await
        .map_err(|error| ReportsError::Bus(error.to_string()))?;
    let config = &consumer.cached_info().config;
    if config.durable_name.as_deref() != Some(durable_name)
        || config.filter_subject != FILTER_SUBJECT
        || config.ack_policy != jetstream::consumer::AckPolicy::Explicit
        || config.ack_wait != ACK_WAIT
        || config.deliver_subject.is_some()
    {
        return Err(ReportsError::Bus(
            "the Edge-provisioned extractor-report durable is mismatched".to_owned(),
        ));
    }
    Ok(consumer)
}

/// Opens the Edge-provisioned durable and verifies its filter, ack policy and ack wait.
///
/// # Errors
/// [`ReportsError::Bus`] when the stream or durable is absent or differs from its spec. This
/// client never creates a consumer: that could widen a compromised identity's filter.
pub async fn ensure_reports_consumer(
    context: &jetstream::Context,
    stream_name: &str,
    durable_name: &str,
) -> Result<(), ReportsError> {
    reports_consumer(context, stream_name, durable_name).await?;
    Ok(())
}

/// Runs the pull loop until `shutdown` resolves.
///
/// # Errors
/// [`ReportsError`] when the durable cannot be opened, was deleted, or the message stream ended
/// without a shutdown request.
pub async fn consume_extractor_reports(
    context: &jetstream::Context,
    database: &Database,
    stream_name: &str,
    durable_name: &str,
    shutdown: impl Future<Output = ()> + Send,
) -> Result<ReportsSummary, ReportsError> {
    let consumer = reports_consumer(context, stream_name, durable_name).await?;
    let mut messages = consumer
        .messages()
        .await
        .map_err(|error| ReportsError::Bus(error.to_string()))?;
    let service = ArticleCaptureService::new(database.clone());
    let mut summary = ReportsSummary::default();
    tokio::pin!(shutdown);

    loop {
        let next = tokio::select! {
            biased;
            () = &mut shutdown => return Ok(summary),
            next = messages.next() => next,
        };
        let message = match next {
            None => return Err(ReportsError::Stopped),
            Some(Ok(message)) => message,
            Some(Err(error)) => {
                if matches!(
                    error.kind(),
                    MessagesErrorKind::ConsumerDeleted | MessagesErrorKind::PushBasedConsumer
                ) {
                    return Err(ReportsError::Bus(error.to_string()));
                }
                tracing::warn!(%error, "the extractor-report stream reported a recoverable error");
                tokio::select! {
                    biased;
                    () = &mut shutdown => return Ok(summary),
                    () = tokio::time::sleep(RETRY_PAUSE) => {}
                }
                continue;
            }
        };
        let outcome = match std::str::from_utf8(&message.payload) {
            Ok(document) => service.consume_extractor_operation_report(document).await,
            Err(_) => Err(ArticleCaptureError::InvalidOutcome),
        };
        match outcome {
            Ok(()) => {
                summary.applied += 1;
                acknowledge(&message, AckKind::Ack).await;
            }
            Err(ArticleCaptureError::InvalidOutcome | ArticleCaptureError::SourceUnavailable) => {
                summary.ignored += 1;
                acknowledge(&message, AckKind::Ack).await;
            }
            Err(error) => {
                summary.retried += 1;
                tracing::error!(%error, "leaving an extractor report for redelivery");
                acknowledge(&message, AckKind::Nak(Some(NAK_DELAY))).await;
            }
        }
    }
}

async fn acknowledge(message: &jetstream::Message, kind: AckKind) {
    if let Err(error) = message.ack_with(kind).await {
        tracing::warn!(%error, "the extractor-report acknowledgement failed");
    }
}
