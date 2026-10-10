//! Transactional projection of normalized X posts into shared `SocialSource` events.
//!
//! The bookmark path (account-scoped rows that an authoritative snapshot observed) and the
//! explicit-capture path (owner-scoped rows that the public resolution returned) share the pure
//! pieces here: the semantic digest, the snapshot and the complete event envelope that is queued
//! on the caller's transaction.

use chrono::{DateTime, Utc};
use ratatoskr_identifiers::Extensions;
use ratatoskr_social_contracts::{SocialSourceCaptured, SocialSourceSnapshot, SocialSourceUpdated};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::types::{Json, Uuid};

use crate::SnapshotError;
use crate::envelopes::{EnvelopeFacts, entity, queue_event, wire_timestamp};

#[derive(Clone, Copy)]
pub(crate) struct SourceProvenance {
    acquisition: &'static str,
    saved_authority: &'static str,
}

const BOOKMARK_PROVENANCE: SourceProvenance = SourceProvenance {
    acquisition: "official_api",
    saved_authority: "authoritative_platform_state",
};

pub(crate) const EXPLICIT_PROVENANCE: SourceProvenance = SourceProvenance {
    acquisition: "browser_extension",
    saved_authority: "explicit_user_capture",
};

/// The plain facts a source revision is built from, whichever table they were read from.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SourceFacts<'a> {
    pub(crate) provider_id: &'a str,
    pub(crate) text: &'a str,
    pub(crate) long_text: Option<&'a str>,
    pub(crate) published_at: Option<DateTime<Utc>>,
    pub(crate) availability: &'a str,
    pub(crate) author_provider_id: &'a str,
    pub(crate) author_username: Option<&'a str>,
    pub(crate) author_display_name: Option<&'a str>,
    pub(crate) expanded_urls: &'a [String],
}

impl SourceFacts<'_> {
    /// The semantic content digest: unchanged observations produce the same digest.
    pub(crate) fn digest(&self, provenance: SourceProvenance) -> Result<Value, SnapshotError> {
        let semantic = json!({
            "provider_id": self.provider_id,
            "text": self.long_text.unwrap_or(self.text),
            "published_at": self.published_at.map(wire_timestamp),
            "availability": self.availability,
            "author_provider_id": self.author_provider_id,
            "author_username": self.author_username,
            "author_display_name": self.author_display_name,
            "expanded_urls": self.expanded_urls,
            "acquisition": provenance.acquisition,
            "saved_authority": provenance.saved_authority,
        });
        content_digest(&semantic)
    }

    /// The shared contract snapshot of these facts.
    pub(crate) fn snapshot(
        &self,
        source_id: Uuid,
        owner: Uuid,
        digest: &Value,
        captured_at: DateTime<Utc>,
        provenance: SourceProvenance,
    ) -> Result<SocialSourceSnapshot, SnapshotError> {
        serde_json::from_value(json!({
            "social_source_id": source_id,
            "platform": "x",
            "external_post_id": self.provider_id,
            "owner": format!("user:{owner}"),
            "author": {
                "platform": "x",
                "external_author_id": self.author_provider_id,
                "handle": self.author_username,
                "display_name": self.author_display_name,
            },
            "published_at": self.published_at.map(wire_timestamp),
            "captured_at": wire_timestamp(captured_at),
            "text": self.long_text.unwrap_or(self.text),
            "content_digest": digest,
            "acquisition": provenance.acquisition,
            "saved_authority": provenance.saved_authority,
            "completeness": "complete",
            "upstream_availability": upstream_availability(self.availability),
        }))
        .map_err(SnapshotError::Contract)
    }
}

/// Whether a revision introduced the source or changed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceFact {
    Captured,
    Updated,
}

/// Queues the complete `social.source.captured|updated.v1` envelope for one revision on the
/// caller's transaction. Both source paths end here.
pub(crate) async fn queue_source_event(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner: Uuid,
    fact: SourceFact,
    snapshot: SocialSourceSnapshot,
    captured_at: DateTime<Utc>,
) -> Result<(), SnapshotError> {
    let source_id = snapshot.social_source_id.0;
    let aggregate = entity("social_source", source_id)?;
    let facts = EnvelopeFacts {
        correlation: aggregate.clone(),
        aggregate,
        causation: None,
        owner,
        occurred_at: captured_at,
    };
    match fact {
        SourceFact::Captured => {
            let payload = SocialSourceCaptured {
                source: snapshot,
                extensions: Extensions::default(),
            };
            queue_event(transaction, facts, &payload).await?;
        }
        SourceFact::Updated => {
            let payload = SocialSourceUpdated {
                source: snapshot,
                extensions: Extensions::default(),
            };
            queue_event(transaction, facts, &payload).await?;
        }
    }
    Ok(())
}

type SourceRow = (
    Uuid,
    Option<Uuid>,
    Option<String>,
    Option<DateTime<Utc>>,
    String,
    String,
    Option<String>,
    Option<DateTime<Utc>>,
    String,
    String,
    Option<String>,
    Option<String>,
    Json<Vec<String>>,
);

/// An account-scoped source as the bookmark path reads it.
struct LoadedSource {
    owner: Uuid,
    source_id: Option<Uuid>,
    digest: Option<String>,
    removed_at: Option<DateTime<Utc>>,
    provider_id: String,
    text: String,
    long_text: Option<String>,
    published_at: Option<DateTime<Utc>>,
    availability: String,
    author_provider_id: String,
    author_username: Option<String>,
    author_display_name: Option<String>,
    expanded_urls: Vec<String>,
}

impl LoadedSource {
    fn facts(&self) -> SourceFacts<'_> {
        SourceFacts {
            provider_id: &self.provider_id,
            text: &self.text,
            long_text: self.long_text.as_deref(),
            published_at: self.published_at,
            availability: &self.availability,
            author_provider_id: &self.author_provider_id,
            author_username: self.author_username.as_deref(),
            author_display_name: self.author_display_name.as_deref(),
            expanded_urls: &self.expanded_urls,
        }
    }

    fn digest_is_current(&self, digest: &Value) -> bool {
        self.digest.as_deref().is_some_and(|stored| {
            serde_json::from_str::<Value>(stored).ok().as_ref() == Some(digest)
        })
    }
}

impl From<SourceRow> for LoadedSource {
    fn from(row: SourceRow) -> Self {
        let (
            owner,
            source_id,
            digest,
            removed_at,
            provider_id,
            text,
            long_text,
            published_at,
            availability,
            author_provider_id,
            author_username,
            author_display_name,
            expanded_urls,
        ) = row;
        Self {
            owner,
            source_id,
            digest,
            removed_at,
            provider_id,
            text,
            long_text,
            published_at,
            availability,
            author_provider_id,
            author_username,
            author_display_name,
            expanded_urls: expanded_urls.0,
        }
    }
}

/// Stores an account-scoped source revision and exactly one corresponding
/// state-carried event when the normalized post materially changed.
pub(crate) async fn publish_bookmark_sources(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    post_ids: impl IntoIterator<Item = Uuid>,
    captured_at: DateTime<Utc>,
) -> Result<(), SnapshotError> {
    for post_id in post_ids {
        publish_source(transaction, account_id, post_id, captured_at).await?;
    }
    Ok(())
}

async fn load_source(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    post_id: Uuid,
) -> Result<LoadedSource, SnapshotError> {
    let row: SourceRow = sqlx::query_as(
        "select account.internal_user_id, source.social_source_id, \
         source.current_content_digest::text, source.removed_at, \
         post.provider_id, post.text, post.long_text, \
         post.published_at, post.availability, author.provider_id, author.username, \
         author.display_name, post.expanded_urls from x_archive.accounts account \
         join x_archive.posts post on post.id = $2 \
         join x_archive.users author on author.id = post.author_user_id \
         left join x_archive.social_sources source on source.account_id = account.id \
           and source.post_id = post.id where account.id = $1",
    )
    .bind(account_id)
    .bind(post_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(SnapshotError::Query)?;
    Ok(row.into())
}

async fn publish_source(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    post_id: Uuid,
    captured_at: DateTime<Utc>,
) -> Result<(), SnapshotError> {
    let provenance = BOOKMARK_PROVENANCE;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("social_source:{account_id}:{post_id}"))
        .execute(&mut **transaction)
        .await
        .map_err(SnapshotError::Query)?;

    let loaded = load_source(transaction, account_id, post_id).await?;
    if loaded.removed_at.is_some() {
        return Ok(());
    }
    let facts = loaded.facts();
    let digest = facts.digest(provenance)?;
    if loaded.digest_is_current(&digest) {
        return Ok(());
    }

    let source_id = loaded.source_id.unwrap_or_else(Uuid::now_v7);
    let snapshot = facts.snapshot(source_id, loaded.owner, &digest, captured_at, provenance)?;
    let fact = if loaded.source_id.is_some() {
        sqlx::query(
            "update x_archive.social_sources set current_content_digest = $2::jsonb, \
             captured_at = $3, acquisition = $4, saved_authority = $5, updated_at = now() \
             where social_source_id = $1",
        )
        .bind(source_id)
        .bind(digest.to_string())
        .bind(captured_at)
        .bind(provenance.acquisition)
        .bind(provenance.saved_authority)
        .execute(&mut **transaction)
        .await
        .map_err(SnapshotError::Query)?;
        SourceFact::Updated
    } else {
        sqlx::query(
            "insert into x_archive.social_sources (account_id, post_id, social_source_id, \
             current_content_digest, acquisition, saved_authority, captured_at) \
             values ($1, $2, $3, $4::jsonb, $5, $6, $7)",
        )
        .bind(account_id)
        .bind(post_id)
        .bind(source_id)
        .bind(digest.to_string())
        .bind(provenance.acquisition)
        .bind(provenance.saved_authority)
        .bind(captured_at)
        .execute(&mut **transaction)
        .await
        .map_err(SnapshotError::Query)?;
        SourceFact::Captured
    };
    sqlx::query(
        "insert into x_archive.social_source_revisions (social_source_id, content_digest, \
         captured_at, acquisition, saved_authority) values ($1, $2::jsonb, $3, $4, $5)",
    )
    .bind(source_id)
    .bind(digest.to_string())
    .bind(captured_at)
    .bind(provenance.acquisition)
    .bind(provenance.saved_authority)
    .execute(&mut **transaction)
    .await
    .map_err(SnapshotError::Query)?;
    queue_source_event(transaction, loaded.owner, fact, snapshot, captured_at).await?;
    crate::articles::capture_expanded_links_in_transaction(
        transaction,
        account_id,
        source_id,
        &loaded.expanded_urls,
    )
    .await?;
    Ok(())
}

pub(crate) fn content_digest(semantic: &Value) -> Result<Value, SnapshotError> {
    let bytes = serde_json::to_vec(semantic).map_err(SnapshotError::Contract)?;
    Ok(json!({"algorithm": "sha256", "hex": lower_hex(&Sha256::digest(bytes))}))
}

/// Lowercase hexadecimal text of `bytes`.
pub(crate) fn lower_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

fn upstream_availability(availability: &str) -> &'static str {
    match availability {
        "active" => "available",
        "deleted" => "deleted_upstream",
        _ => "unavailable",
    }
}
