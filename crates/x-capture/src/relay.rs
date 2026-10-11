//! The outbox relay: complete envelopes from `x_archive.outbox_events` to `JetStream`
//! (XR-021 CONTRACTS.md S02 and S03).
//!
//! Every row already stores the canonical envelope and its id, so the relay maps the stored
//! `event_type` to a subject with a CLOSED match, publishes the stored document unchanged with the
//! row id as `Nats-Msg-Id`, and marks `published_at` only after the broker acknowledged the
//! message. A row it cannot map is a programming error: the relay stops instead of skipping it.
//!
//! A relay that keeps running but cannot publish is not healthy (CONTRACTS.md R2-03): [`RelayHealth`]
//! turns unhealthy once a publishable row has been refused repeatedly, and the service feeds it
//! into readiness. A body larger than the broker accepts can never be delivered, so it is marked
//! undeliverable at once and the pass continues (R2-04); it never counts against health.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_nats::jetstream;
use x_persistence::database::Database;

/// Rows one pass publishes at most.
const BATCH_SIZE: i64 = 32;
/// Pause between passes of [`OutboxRelay::run`].
const INTERVAL: Duration = Duration::from_secs(1);
/// How long the broker may take to acknowledge one message.
const ACK_TIMEOUT: Duration = Duration::from_secs(5);
/// Room left under the server's `max_payload` for the headers and framing of one message.
const PAYLOAD_HEADROOM: usize = 1024;
/// Refusals of one row after which the relay is unhealthy.
const UNHEALTHY_ATTEMPTS: i32 = 3;
/// Seconds a row may keep failing before the relay is unhealthy.
const UNHEALTHY_AGE_SECONDS: i32 = 300;
/// The class of a body the broker would refuse for its size, on every attempt.
pub const PAYLOAD_TOO_LARGE: &str = "payload_too_large";

/// The `JetStream` subject of an outbox `event_type`, or `None` for a type the relay may not
/// publish. The list is closed on purpose: it is the same list the schema's `CHECK` constraint
/// admits and the same list the X identity may publish in the deployed ACL.
#[must_use]
pub fn subject_for(event_type: &str) -> Option<String> {
    match event_type {
        "platform.operation.reported.v1"
        | "social.source.captured.v1"
        | "social.source.updated.v1"
        | "social.source.removed.v1" => Some(format!("evt.{event_type}")),
        "content.capture.requested.v1" => Some(format!("cmd.{event_type}")),
        _ => None,
    }
}

/// Why one publish was not acknowledged. Carries a safe class, never message content.
///
/// A [`PAYLOAD_TOO_LARGE`] failure is permanent; every other class is transient.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "the broker did not acknowledge the publish ({class}); check the NATS server log for a \
     Publish Violation"
)]
pub struct PublishFailure {
    /// A fixed, content-free class recorded in `last_error`.
    pub class: &'static str,
}

impl PublishFailure {
    /// Whether retrying can never succeed.
    #[must_use]
    pub fn is_permanent(&self) -> bool {
        self.class == PAYLOAD_TOO_LARGE
    }
}

/// The broker side of the relay, injected so failure handling is testable without a broken broker.
pub trait OutboxPublisher: Send + Sync {
    /// Publishes `payload` to `subject` and resolves once the broker acknowledged it.
    /// `message_id` is the broker deduplication id.
    fn publish<'a>(
        &'a self,
        subject: &'a str,
        message_id: &'a str,
        payload: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<(), PublishFailure>> + Send + 'a>>;
}

/// Publishes through a `JetStream` context and waits for the `PubAck`.
#[derive(Clone)]
pub struct JetStreamPublisher {
    context: jetstream::Context,
}

impl std::fmt::Debug for JetStreamPublisher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JetStreamPublisher")
            .finish_non_exhaustive()
    }
}

impl JetStreamPublisher {
    /// Wraps a `JetStream` context.
    #[must_use]
    pub fn new(context: jetstream::Context) -> Self {
        Self { context }
    }

    async fn send(
        &self,
        subject: &str,
        message_id: &str,
        payload: Vec<u8>,
    ) -> Result<(), PublishFailure> {
        let limit = self.context.client().server_info().max_payload;
        if limit > 0 && payload.len() > limit.saturating_sub(PAYLOAD_HEADROOM) {
            return Err(PublishFailure {
                class: PAYLOAD_TOO_LARGE,
            });
        }
        let mut headers = async_nats::HeaderMap::new();
        headers.insert(async_nats::header::NATS_MESSAGE_ID, message_id);
        let acknowledgement = self
            .context
            .publish_with_headers(subject.to_owned(), headers, payload.into())
            .await
            .map_err(|_| PublishFailure {
                class: "publish_failed",
            })?;
        // A denied publish is only visible in the server log; to the client it is a missing ack.
        match tokio::time::timeout(ACK_TIMEOUT, acknowledgement).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(_)) => Err(PublishFailure {
                class: "ack_rejected",
            }),
            Err(_) => Err(PublishFailure {
                class: "ack_timeout",
            }),
        }
    }
}

impl OutboxPublisher for JetStreamPublisher {
    fn publish<'a>(
        &'a self,
        subject: &'a str,
        message_id: &'a str,
        payload: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<(), PublishFailure>> + Send + 'a>> {
        Box::pin(self.send(subject, message_id, payload))
    }
}

/// Whether the relay can publish what it holds.
///
/// The flag is unhealthy while a publishable unpublished row has been refused at least
/// three times, or has been refused at least once and is older than 300 seconds. It is healthy
/// again once no such row is left: the row published, or became undeliverable. The relay keeps
/// retrying either way; the flag only tells the service what to report.
#[derive(Debug, Clone)]
pub struct RelayHealth(Arc<AtomicBool>);

impl RelayHealth {
    /// A healthy flag.
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }

    /// Whether the relay is healthy.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// Stores the verdict and returns the previous one.
    fn replace(&self, healthy: bool) -> bool {
        self.0.swap(healthy, Ordering::AcqRel)
    }
}

impl Default for RelayHealth {
    fn default() -> Self {
        Self::new()
    }
}

/// What one relay pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RelayReport {
    /// Rows published and marked.
    pub published: u32,
    /// Rows whose publish failed (a pass stops at the first one).
    pub failed: u32,
    /// Rows marked undeliverable.
    pub undeliverable: u32,
}

/// Why the relay stopped.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RelayError {
    /// A stored row has an `event_type` with no subject. The schema forbids it, so this is a
    /// programming error and the relay never skips it.
    #[error("the outbox holds an event type the relay cannot map to a subject: {0}")]
    UnmappableEventType(String),
    /// A database statement failed.
    #[error("the outbox relay database statement failed")]
    Query(#[source] sqlx::Error),
}

/// Publishes unpublished outbox rows in creation order.
#[derive(Clone)]
pub struct OutboxRelay {
    database: Database,
    publisher: Arc<dyn OutboxPublisher>,
    health: RelayHealth,
}

impl std::fmt::Debug for OutboxRelay {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OutboxRelay")
            .finish_non_exhaustive()
    }
}

impl OutboxRelay {
    /// Builds a relay over the outbox and a publisher.
    #[must_use]
    pub fn new(database: Database, publisher: Arc<dyn OutboxPublisher>) -> Self {
        Self {
            database,
            publisher,
            health: RelayHealth::new(),
        }
    }

    /// The health flag this relay keeps.
    #[must_use]
    pub fn health(&self) -> RelayHealth {
        self.health.clone()
    }

    /// Runs one pass: publishes the due rows oldest first and stops at the first failed publish.
    ///
    /// A failed row records `attempt_count`, a safe `last_error` class and a `next_attempt_at`
    /// backoff (20 s, 40 s, 80 s, 160 s, then 5 min), so the next pass skips it and a failing head
    /// row never starves later rows. A body the broker can never accept is marked undeliverable
    /// instead and the pass goes on with the next row. The pass ends by refreshing
    /// [`RelayHealth`].
    ///
    /// # Errors
    /// [`RelayError::UnmappableEventType`] for a row with no subject; [`RelayError::Query`] when
    /// the outbox cannot be read or updated.
    pub async fn run_once(&self) -> Result<RelayReport, RelayError> {
        let rows: Vec<(sqlx::types::Uuid, String, String)> = sqlx::query_as(
            "select id, event_type, payload::text from x_archive.outbox_events \
              where published_at is null and undeliverable_at is null \
                and next_attempt_at <= now() \
              order by created_at, id limit $1",
        )
        .bind(BATCH_SIZE)
        .fetch_all(self.database.pool())
        .await
        .map_err(RelayError::Query)?;
        let mut report = RelayReport::default();
        for (id, event_type, payload) in rows {
            let Some(subject) = subject_for(&event_type) else {
                return Err(RelayError::UnmappableEventType(event_type));
            };
            match self
                .publisher
                .publish(&subject, &id.to_string(), payload.into_bytes())
                .await
            {
                Ok(()) => {
                    self.mark_published(id).await?;
                    report.published += 1;
                }
                Err(failure) if failure.is_permanent() => {
                    tracing::warn!(
                        %failure,
                        row_id = %id,
                        %event_type,
                        "the outbox relay marked a row undeliverable"
                    );
                    metrics::counter!(
                        "x_outbox_undeliverable_total",
                        "class" => failure.class
                    )
                    .increment(1);
                    self.record_undeliverable(id, failure).await?;
                    report.undeliverable += 1;
                }
                Err(failure) => {
                    tracing::warn!(
                        %failure,
                        row_id = %id,
                        %event_type,
                        "the outbox relay could not publish a row; it is backed off"
                    );
                    self.record_failure(id, failure).await?;
                    report.failed += 1;
                    break;
                }
            }
        }
        self.refresh_health().await?;
        Ok(report)
    }

    /// Runs passes until `shutdown` resolves, calling `on_health` with the verdict after each
    /// pass.
    ///
    /// # Errors
    /// Any [`RelayError`]: the relay stopping for any reason other than shutdown is a fault the
    /// service must surface.
    pub async fn run(
        &self,
        shutdown: impl Future<Output = ()> + Send,
        mut on_health: impl FnMut(bool) + Send,
    ) -> Result<(), RelayError> {
        tokio::pin!(shutdown);
        loop {
            self.run_once().await?;
            on_health(self.health.is_healthy());
            tokio::select! {
                biased;
                () = &mut shutdown => return Ok(()),
                () = tokio::time::sleep(INTERVAL) => {}
            }
        }
    }

    /// Decides the health verdict from the rows the relay is still holding.
    async fn refresh_health(&self) -> Result<(), RelayError> {
        let stuck: Option<(Option<String>,)> = sqlx::query_as(
            "select last_error from x_archive.outbox_events \
              where published_at is null and undeliverable_at is null and attempt_count >= 1 \
                and (attempt_count >= $1 \
                     or created_at < now() - make_interval(secs => $2)) \
              order by created_at, id limit 1",
        )
        .bind(UNHEALTHY_ATTEMPTS)
        .bind(UNHEALTHY_AGE_SECONDS)
        .fetch_optional(self.database.pool())
        .await
        .map_err(RelayError::Query)?;
        let was_healthy = self.health.replace(stuck.is_none());
        if let (true, Some((class,))) = (was_healthy, stuck) {
            let class = class.unwrap_or_else(|| "unknown".to_owned());
            tracing::error!(
                %class,
                "the outbox relay cannot publish; check the NATS server log for a Publish Violation"
            );
            metrics::counter!("x_outbox_relay_unhealthy_total", "class" => class).increment(1);
        }
        Ok(())
    }

    async fn mark_published(&self, id: sqlx::types::Uuid) -> Result<(), RelayError> {
        sqlx::query(
            "update x_archive.outbox_events set published_at = now(), last_error = null \
              where id = $1 and published_at is null",
        )
        .bind(id)
        .execute(self.database.pool())
        .await
        .map_err(RelayError::Query)?;
        Ok(())
    }

    async fn record_undeliverable(
        &self,
        id: sqlx::types::Uuid,
        failure: PublishFailure,
    ) -> Result<(), RelayError> {
        sqlx::query(
            "update x_archive.outbox_events \
                set attempt_count = attempt_count + 1, last_error = $2, undeliverable_at = now() \
              where id = $1 and published_at is null",
        )
        .bind(id)
        .bind(failure.class)
        .execute(self.database.pool())
        .await
        .map_err(RelayError::Query)?;
        Ok(())
    }

    async fn record_failure(
        &self,
        id: sqlx::types::Uuid,
        failure: PublishFailure,
    ) -> Result<(), RelayError> {
        sqlx::query(
            "update x_archive.outbox_events \
                set attempt_count = attempt_count + 1, last_error = $2, \
                    next_attempt_at = now() + least(300, 10 * (1 << least(attempt_count + 1, 5))) \
                                              * interval '1 second' \
              where id = $1 and published_at is null",
        )
        .bind(id)
        .bind(failure.class)
        .execute(self.database.pool())
        .await
        .map_err(RelayError::Query)?;
        Ok(())
    }
}
