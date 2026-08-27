//! Transactional projection of normalized X posts into shared `SocialSource` events.

use chrono::{DateTime, SecondsFormat, Utc};
use ratatoskr_event_envelope::EventPayload;
use ratatoskr_identifiers::Extensions;
use ratatoskr_social_contracts::{SocialSourceCaptured, SocialSourceSnapshot, SocialSourceUpdated};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::types::{Json, Uuid};

use crate::SnapshotError;

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

/// Stores an account-scoped source revision and exactly one corresponding
/// state-carried event when the normalized post materially changed.
pub(crate) async fn publish_bookmark_sources(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    post_ids: impl IntoIterator<Item = Uuid>,
    captured_at: DateTime<Utc>,
) -> Result<(), SnapshotError> {
    for post_id in post_ids {
        publish_source(
            transaction,
            account_id,
            post_id,
            captured_at,
            BOOKMARK_PROVENANCE,
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn publish_explicit_source(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    post_id: Uuid,
    captured_at: DateTime<Utc>,
) -> Result<(), SnapshotError> {
    publish_source(
        transaction,
        account_id,
        post_id,
        captured_at,
        EXPLICIT_PROVENANCE,
    )
    .await
}

#[expect(
    clippy::too_many_lines,
    reason = "source, revision, and outbox writes must stay visibly within one transaction"
)]
async fn publish_source(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    post_id: Uuid,
    captured_at: DateTime<Utc>,
    provenance: SourceProvenance,
) -> Result<(), SnapshotError> {
    sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("social_source:{account_id}:{post_id}"))
        .execute(&mut **transaction)
        .await
        .map_err(SnapshotError::Query)?;

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

    let (
        internal_user_id,
        existing_source_id,
        existing_digest,
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
    if removed_at.is_some() {
        return Ok(());
    }
    let expanded_urls = expanded_urls.0;
    let semantic = json!({
        "provider_id": provider_id,
        "text": long_text.as_deref().unwrap_or(&text),
        "published_at": published_at.map(wire_timestamp),
        "availability": availability,
        "author_provider_id": author_provider_id,
        "author_username": author_username,
        "author_display_name": author_display_name,
        "expanded_urls": &expanded_urls,
        "acquisition": provenance.acquisition,
        "saved_authority": provenance.saved_authority,
    });
    let digest = content_digest(&semantic)?;
    if existing_digest
        .as_deref()
        .is_some_and(|stored| serde_json::from_str::<Value>(stored).ok().as_ref() == Some(&digest))
    {
        return Ok(());
    }

    let source_id = existing_source_id.unwrap_or_else(Uuid::now_v7);
    let snapshot = snapshot(
        source_id,
        internal_user_id,
        &provider_id,
        &text,
        long_text.as_deref(),
        published_at,
        &availability,
        &author_provider_id,
        author_username.as_deref(),
        author_display_name.as_deref(),
        &digest,
        captured_at,
        provenance,
    )?;
    let event_type;
    let payload;
    if existing_source_id.is_some() {
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
        event_type = SocialSourceUpdated::EVENT_TYPE;
        payload = serde_json::to_value(SocialSourceUpdated {
            source: snapshot,
            extensions: Extensions::default(),
        })
        .map_err(SnapshotError::Contract)?;
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
        event_type = SocialSourceCaptured::EVENT_TYPE;
        payload = serde_json::to_value(SocialSourceCaptured {
            source: snapshot,
            extensions: Extensions::default(),
        })
        .map_err(SnapshotError::Contract)?;
    }
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
    sqlx::query(
        "insert into x_archive.outbox_events (aggregate, event_type, payload, correlation_id) \
         values ($1, $2, $3::jsonb, $4)",
    )
    .bind(format!("social_source:{source_id}"))
    .bind(event_type)
    .bind(payload.to_string())
    .bind(format!("social_source:{source_id}"))
    .execute(&mut **transaction)
    .await
    .map_err(SnapshotError::Query)?;
    crate::articles::capture_expanded_links_in_transaction(
        transaction,
        account_id,
        source_id,
        &expanded_urls,
    )
    .await
    .map_err(SnapshotError::Query)?;
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "the query row maps directly to the shared snapshot"
)]
fn snapshot(
    source_id: Uuid,
    owner_id: Uuid,
    provider_id: &str,
    text: &str,
    long_text: Option<&str>,
    published_at: Option<DateTime<Utc>>,
    availability: &str,
    author_provider_id: &str,
    author_username: Option<&str>,
    author_display_name: Option<&str>,
    digest: &Value,
    captured_at: DateTime<Utc>,
    provenance: SourceProvenance,
) -> Result<SocialSourceSnapshot, SnapshotError> {
    serde_json::from_value(json!({
        "social_source_id": source_id,
        "platform": "x",
        "external_post_id": provider_id,
        "owner": format!("user:{owner_id}"),
        "author": {
            "platform": "x",
            "external_author_id": author_provider_id,
            "handle": author_username,
            "display_name": author_display_name,
        },
        "published_at": published_at.map(wire_timestamp),
        "captured_at": wire_timestamp(captured_at),
        "text": long_text.unwrap_or(text),
        "content_digest": digest,
        "acquisition": provenance.acquisition,
        "saved_authority": provenance.saved_authority,
        "completeness": "complete",
        "upstream_availability": upstream_availability(availability),
    }))
    .map_err(SnapshotError::Contract)
}

fn content_digest(semantic: &Value) -> Result<Value, SnapshotError> {
    let bytes = serde_json::to_vec(semantic).map_err(SnapshotError::Contract)?;
    let hash = Sha256::digest(bytes);
    let mut hex = String::with_capacity(64);
    for byte in hash {
        hex.push(hex_digit(byte >> 4));
        hex.push(hex_digit(byte & 0x0f));
    }
    Ok(json!({"algorithm": "sha256", "hex": hex}))
}

fn hex_digit(value: u8) -> char {
    match value {
        0 => '0',
        1 => '1',
        2 => '2',
        3 => '3',
        4 => '4',
        5 => '5',
        6 => '6',
        7 => '7',
        8 => '8',
        9 => '9',
        10 => 'a',
        11 => 'b',
        12 => 'c',
        13 => 'd',
        14 => 'e',
        15 => 'f',
        _ => unreachable!("a SHA-256 nibble is in the hexadecimal range"),
    }
}

fn upstream_availability(availability: &str) -> &'static str {
    match availability {
        "active" => "available",
        "deleted" => "deleted_upstream",
        _ => "unavailable",
    }
}

fn wire_timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::AutoSi, true)
}
