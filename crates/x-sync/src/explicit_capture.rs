//! Explicit X capture command boundary.

use chrono::{DateTime, Utc};
use ratatoskr_event_envelope::{CommandEnvelope, CommandPayload};
use ratatoskr_social_contracts::{
    AcquisitionMethod, SavedAuthority, SocialCaptureProvider, SocialCaptureRequested,
};
use sqlx::types::Uuid;
use x_persistence::database::Database;

/// Applies an explicit browser capture after the post is normalized by the X adapter.
#[derive(Clone, Debug)]
pub struct ExplicitCaptureService {
    database: Database,
}

impl ExplicitCaptureService {
    /// Builds the explicit-capture command consumer.
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    /// Applies one command delivery to an already normalized X post.
    ///
    /// # Errors
    ///
    /// Returns a typed refusal for an unsupported or foreign command, and a
    /// persistence error when atomic application cannot commit.
    pub async fn apply(
        &self,
        account_id: Uuid,
        command: CommandEnvelope,
    ) -> Result<(), ExplicitCaptureError> {
        let request = command.payload_as::<SocialCaptureRequested>()?;
        if !matches!(request.provider, SocialCaptureProvider::X)
            || !matches!(request.acquisition, AcquisitionMethod::BrowserExtension)
            || !matches!(request.saved_authority, SavedAuthority::ExplicitUserCapture)
        {
            return Err(ExplicitCaptureError::Refused);
        }
        let owner: Uuid =
            sqlx::query_scalar("select internal_user_id from x_archive.accounts where id = $1")
                .bind(account_id)
                .fetch_optional(self.database.pool())
                .await?
                .ok_or(ExplicitCaptureError::AccountUnavailable)?;
        if command
            .tenant_id
            .as_ref()
            .map(ToString::to_string)
            .as_deref()
            != Some(&format!("user:{owner}"))
        {
            return Err(ExplicitCaptureError::TenantMismatch);
        }
        let permalink = request.original_permalink.to_string();
        let post_provider_id = status_id(&permalink).ok_or(ExplicitCaptureError::Refused)?;
        let captured_at: DateTime<Utc> = request
            .captured_at
            .to_string()
            .parse()
            .map_err(|_| ExplicitCaptureError::Refused)?;
        let mut transaction = self.database.pool().begin().await?;
        let inserted: Option<Uuid> = sqlx::query_scalar(
            "insert into x_archive.inbox_events (source, event_type, event_id, payload, consumed_at) \
             values ('ratatoskr-platform', $1, $2, $3::jsonb, null) \
             on conflict (event_id) do nothing returning id",
        )
        .bind(SocialCaptureRequested::COMMAND_TYPE)
        .bind(command.command_id.to_string())
        .bind(serde_json::to_string(&command).map_err(ExplicitCaptureError::Serialize)?)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(inbox_id) = inserted else {
            transaction.commit().await?;
            return Ok(());
        };
        let post_id: Uuid =
            sqlx::query_scalar("select id from x_archive.posts where provider_id = $1")
                .bind(post_provider_id)
                .fetch_optional(&mut *transaction)
                .await?
                .ok_or(ExplicitCaptureError::PostUnavailable)?;
        crate::social_sources::publish_explicit_source(
            &mut transaction,
            account_id,
            post_id,
            captured_at,
        )
        .await?;
        sqlx::query("update x_archive.inbox_events set consumed_at = now() where id = $1")
            .bind(inbox_id)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }
}

/// Explicit command refusal or persistence failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExplicitCaptureError {
    /// The command envelope or typed payload is malformed.
    #[error(transparent)]
    Command(#[from] ratatoskr_event_envelope::CommandError),
    /// The command has unsupported provider or provenance facts.
    #[error("the explicit capture command is not an X browser capture")]
    Refused,
    /// The selected account is absent.
    #[error("the selected X account is unavailable")]
    AccountUnavailable,
    /// The envelope tenant is not the account owner.
    #[error("the explicit capture tenant does not own the selected account")]
    TenantMismatch,
    /// No normalized X post matches the captured permalink.
    #[error("the explicit capture post is not normalized")]
    PostUnavailable,
    /// The persisted command cannot be serialized as JSON evidence.
    #[error("the explicit capture command cannot be serialized")]
    Serialize(#[source] serde_json::Error),
    /// The transaction query failed.
    #[error("the explicit capture persistence query failed")]
    Query(#[from] sqlx::Error),
    /// Source event publication failed.
    #[error(transparent)]
    Source(#[from] crate::SnapshotError),
}

fn status_id(permalink: &str) -> Option<&str> {
    let (_, rest) = permalink.split_once("://")?;
    let (host, path) = rest.split_once('/')?;
    if !matches!(host, "x.com" | "www.x.com") {
        return None;
    }
    let (_, id) = path.split_once("status/")?;
    let id = id.split(['/', '?', '#']).next()?;
    (!id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())).then_some(id)
}
