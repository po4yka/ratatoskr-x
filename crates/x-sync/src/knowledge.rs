//! Knowledge completion linkage for retained social-source revisions.

use chrono::{DateTime, Utc};
use ratatoskr_event_envelope::{EventEnvelope, EventPayload as _};
use ratatoskr_social_contracts::SocialSourceAnalysisCompleted;
use sqlx::types::Uuid;

use x_persistence::database::Database;

/// Replay admission for one Knowledge completion fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnowledgeAnalysisAdmission {
    /// The completion linked to the source's current digest.
    LinkedCurrent,
    /// The completion linked to a retained older digest.
    LinkedHistorical,
    /// The completion event or source digest was already linked.
    Duplicate,
}

/// Knowledge completion consumption failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum KnowledgeIntegrationError {
    /// The inbound fact is malformed, foreign, unknown, or no longer linkable.
    #[error("the Knowledge completion is invalid for this X source revision")]
    InvalidCompletion,
    /// The owned inbox or linkage state could not be persisted.
    #[error("the Knowledge completion could not be persisted")]
    Query(#[source] sqlx::Error),
}

/// Consumes Knowledge completion facts into X-owned revision linkage.
#[derive(Debug, Clone)]
pub struct KnowledgeAnalysisService {
    database: Database,
}

impl KnowledgeAnalysisService {
    /// Builds the consumer over the X-owned persistence boundary.
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    /// Consumes one serialized Knowledge completion envelope.
    ///
    /// # Errors
    ///
    /// Returns [`KnowledgeIntegrationError`] when the fact is invalid or cannot be persisted.
    pub async fn consume_completion(
        &self,
        serialized_event: &str,
    ) -> Result<KnowledgeAnalysisAdmission, KnowledgeIntegrationError> {
        let envelope: EventEnvelope = serde_json::from_str(serialized_event)
            .map_err(|_| KnowledgeIntegrationError::InvalidCompletion)?;
        if envelope.producer.to_string() != "ratatoskr-knowledge" {
            return Err(KnowledgeIntegrationError::InvalidCompletion);
        }
        let completion: SocialSourceAnalysisCompleted = envelope
            .payload_as()
            .map_err(|_| KnowledgeIntegrationError::InvalidCompletion)?;
        let owner = completion.owner.to_string();
        if envelope.tenant_id.as_ref().map(ToString::to_string) != Some(owner.clone()) {
            return Err(KnowledgeIntegrationError::InvalidCompletion);
        }
        let digest = serde_json::to_value(&completion.content_digest)
            .map_err(|_| KnowledgeIntegrationError::InvalidCompletion)?;
        let completed_at: DateTime<Utc> = completion
            .completed_at
            .to_string()
            .parse()
            .map_err(|_| KnowledgeIntegrationError::InvalidCompletion)?;

        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(KnowledgeIntegrationError::Query)?;
        let source: Option<(Uuid, serde_json::Value, Option<DateTime<Utc>>)> = sqlx::query_as(
            "select account.internal_user_id, source.current_content_digest, source.removed_at \
             from x_archive.social_sources source \
             join x_archive.accounts account on account.id = source.account_id \
             join x_archive.social_source_revisions revision \
               on revision.social_source_id = source.social_source_id \
              and revision.content_digest = $2::jsonb \
             where source.social_source_id = $1 for update of source",
        )
        .bind(completion.social_source_id.0)
        .bind(digest.to_string())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(KnowledgeIntegrationError::Query)?;
        let Some((owner_id, current_digest, removed_at)) = source else {
            return Err(KnowledgeIntegrationError::InvalidCompletion);
        };
        if owner != format!("user:{owner_id}") || removed_at.is_some() {
            return Err(KnowledgeIntegrationError::InvalidCompletion);
        }

        let event_id = envelope.event_id.to_string();
        let claimed: Option<Uuid> = sqlx::query_scalar(
            "insert into x_archive.inbox_events \
               (source, event_type, event_id, payload, consumed_at) \
             values ('ratatoskr-knowledge', $1, $2, $3::jsonb, now()) \
             on conflict (event_id) do nothing returning id",
        )
        .bind(SocialSourceAnalysisCompleted::EVENT_TYPE)
        .bind(&event_id)
        .bind(serialized_event)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(KnowledgeIntegrationError::Query)?;
        if claimed.is_none() {
            transaction
                .commit()
                .await
                .map_err(KnowledgeIntegrationError::Query)?;
            return Ok(KnowledgeAnalysisAdmission::Duplicate);
        }

        let linked: Option<Uuid> = sqlx::query_scalar(
            "insert into x_archive.knowledge_analysis_links \
               (inbox_event_id, social_source_id, content_digest, completed_at) \
             values ($1, $2, $3::jsonb, $4) \
             on conflict (social_source_id, content_digest) do nothing returning id",
        )
        .bind(event_id)
        .bind(completion.social_source_id.0)
        .bind(digest.to_string())
        .bind(completed_at)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(KnowledgeIntegrationError::Query)?;
        transaction
            .commit()
            .await
            .map_err(KnowledgeIntegrationError::Query)?;

        match (linked.is_some(), current_digest == digest) {
            (true, true) => Ok(KnowledgeAnalysisAdmission::LinkedCurrent),
            (true, false) => Ok(KnowledgeAnalysisAdmission::LinkedHistorical),
            (false, _) => Ok(KnowledgeAnalysisAdmission::Duplicate),
        }
    }
}
