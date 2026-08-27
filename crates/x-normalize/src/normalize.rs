//! The normalization entry point: an envelope in, a [`NormalizedBatch`] out.
//!
//! Pure and deterministic: no network, no database, no clock. Refusals are
//! typed ([`NormalizeError`]) and leave no partial output behind. Posts are
//! processed in provider-id order so the first refusal is stable across runs;
//! every output collection is sorted before returning.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::dto::{Envelope, MediaObject, PostObject, ReferencedTweet, UserObject};
use crate::error::NormalizeError;
use crate::records::{
    Availability, MediaKind, NormalizedMedia, NormalizedPost, NormalizedRelation, NormalizedUser,
    Relation,
};

/// Reserved extension key holding reference entries whose type falls outside
/// the closed relation vocabulary; see design D15.
pub const UNRESOLVED_REFERENCES_KEY: &str = "ratatoskr.x/unresolved_references";

/// The complete normalized output of one envelope.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NormalizedBatch {
    /// Author records, sorted by provider id without duplicates.
    users: Vec<NormalizedUser>,
    /// Post records, sorted by provider id.
    posts: Vec<NormalizedPost>,
    /// Relation records, sorted by owning post then target then kind.
    relations: Vec<NormalizedRelation>,
    /// Media metadata records, sorted by owning post then media key.
    media: Vec<NormalizedMedia>,
}

impl NormalizedBatch {
    /// Emitted author records in deterministic order.
    #[must_use]
    pub fn users(&self) -> &[NormalizedUser] {
        &self.users
    }

    /// Emitted post records in deterministic order.
    #[must_use]
    pub fn posts(&self) -> &[NormalizedPost] {
        &self.posts
    }

    /// Emitted relation records in deterministic order.
    #[must_use]
    pub fn relations(&self) -> &[NormalizedRelation] {
        &self.relations
    }

    /// Emitted media metadata records in deterministic order.
    #[must_use]
    pub fn media(&self) -> &[NormalizedMedia] {
        &self.media
    }

    fn push_user_once(
        &mut self,
        seen: &mut BTreeSet<String>,
        author: &UserObject,
        provider_id: &str,
    ) {
        if seen.insert(provider_id.to_owned()) {
            self.users.push(NormalizedUser {
                parser_version: crate::PARSER_VERSION,
                provider_id: provider_id.to_owned(),
                username: author.username.clone(),
                display_name: author.name.clone(),
                extension: author.extension.clone(),
            });
        }
    }
}

/// Indexes included users by provider id for batch-wide author resolution.
fn author_index(envelope: &Envelope) -> BTreeMap<&str, &UserObject> {
    let mut index = BTreeMap::new();
    for user in envelope.included_users() {
        if let Some(id) = user.id.as_deref() {
            index.insert(id, user);
        }
    }
    index
}

/// Indexes included media objects by provider key for attachment joining.
fn media_index(envelope: &Envelope) -> BTreeMap<&str, &MediaObject> {
    let mut index = BTreeMap::new();
    for media in envelope.included_media() {
        if let Some(key) = media.media_key.as_deref() {
            index.insert(key, media);
        }
    }
    index
}

/// Maps the provider kind string onto the closed vocabulary when known.
fn classify_media_kind(kind: Option<&str>) -> Option<MediaKind> {
    match kind? {
        "photo" => Some(MediaKind::Photo),
        "video" => Some(MediaKind::Video),
        "animated_gif" => Some(MediaKind::AnimatedGif),
        _ => None,
    }
}

/// Builds the deterministic metadata document for one media record:
/// documented members where supplied, preserved material under the reserved
/// `extension` key.
fn media_metadata_document(media: &MediaObject) -> Value {
    let mut document = serde_json::Map::new();
    for (key, value) in [
        ("url", media.url.clone().map(Value::String)),
        (
            "preview_image_url",
            media.preview_image_url.clone().map(Value::String),
        ),
        ("width", media.width.map(Value::from)),
        ("height", media.height.map(Value::from)),
        ("alt_text", media.alt_text.clone().map(Value::String)),
        ("duration_ms", media.duration_ms.map(Value::from)),
    ] {
        if let Some(value) = value {
            document.insert(key.to_owned(), value);
        }
    }
    if let Some(variants) = media.variants.as_ref() {
        let rendered: Vec<Value> = variants
            .iter()
            .map(|variant| {
                let mut entry = serde_json::Map::new();
                for (key, value) in [
                    ("bit_rate", variant.bit_rate.map(Value::from)),
                    (
                        "content_type",
                        variant.content_type.clone().map(Value::String),
                    ),
                    ("url", variant.url.clone().map(Value::String)),
                ] {
                    if let Some(value) = value {
                        entry.insert(key.to_owned(), value);
                    }
                }
                Value::Object(entry)
            })
            .collect();
        document.insert("variants".to_owned(), Value::Array(rendered));
    }
    if let Some(metrics) = media.public_metrics.as_ref() {
        let mut rendered = serde_json::Map::new();
        if let Some(views) = metrics.view_count {
            rendered.insert("view_count".to_owned(), Value::from(views));
        }
        rendered.extend(metrics.extension.clone());
        document.insert("public_metrics".to_owned(), Value::Object(rendered));
    }
    document.insert(
        "extension".to_owned(),
        Value::Object(media.extension.clone()),
    );
    Value::Object(document)
}

/// Builds one normalized media metadata record. Bytes are never touched, so
/// the blob reference stays unset by construction.
fn normalize_media_record(post_provider_id: &str, media: &MediaObject) -> NormalizedMedia {
    NormalizedMedia {
        parser_version: crate::PARSER_VERSION,
        post_provider_id: post_provider_id.to_owned(),
        provider_id: media.media_key.clone().unwrap_or_default(),
        kind: classify_media_kind(media.kind.as_deref()),
        metadata: media_metadata_document(media),
        blob_ref: None,
    }
}

/// Splits a post's references into mapped `(target, relation)` pairs and the
/// serialized entries whose type falls outside the closed vocabulary.
fn split_references(post: &PostObject) -> (Vec<(String, Relation)>, Vec<Value>) {
    let mut mapped = Vec::new();
    let mut unresolved = Vec::new();
    for reference in post.referenced_tweets.iter().flatten() {
        match classify_reference(reference) {
            Some(pair) => mapped.push(pair),
            // An unmappable entry is preserved verbatim on the owning post so
            // provider evolution never silently drops evidence (D15).
            None => unresolved.push(serde_json::to_value(reference).unwrap_or(Value::Null)),
        }
    }
    (mapped, unresolved)
}

/// Maps one reference entry onto the closed vocabulary when possible.
fn classify_reference(reference: &ReferencedTweet) -> Option<(String, Relation)> {
    let target = reference.id.as_deref()?;
    let relation = match reference.reference_type.as_deref()? {
        "replied_to" => Relation::Reply,
        "quoted" => Relation::Quote,
        "retweeted" => Relation::Repost,
        _ => return None,
    };
    Some((target.to_owned(), relation))
}

/// Normalizes one official API envelope into row-shaped archive records.
///
/// # Errors
/// Returns the typed refusal of the first offending object when required
/// members are absent or unparseable, or when a post's author cannot be
/// resolved against the envelope's user objects. Nothing is emitted for a
/// refused envelope.
pub fn normalize(envelope: &Envelope) -> Result<NormalizedBatch, NormalizeError> {
    envelope.validate()?;

    let authors = author_index(envelope);
    let media = media_index(envelope);
    let mut ordered: Vec<&PostObject> = envelope.posts().iter().collect();
    ordered.sort_by(|left, right| left.id.as_deref().cmp(&right.id.as_deref()));

    let mut batch = NormalizedBatch::default();
    let mut seen_authors = BTreeSet::new();

    for post in ordered {
        // Validation guarantees these members exist on every carried post.
        let provider_id = post.id.as_deref().unwrap_or_default();
        let author_id = post.author_id.as_deref().unwrap_or_default();
        let Some(author) = authors.get(author_id).copied() else {
            return Err(NormalizeError::UnresolvedAuthor {
                post_provider_id: provider_id.to_owned(),
            });
        };

        batch.push_user_once(&mut seen_authors, author, author_id);

        let (mapped_references, unresolved_references) = split_references(post);
        batch
            .posts
            .push(normalize_post(post, author_id, unresolved_references)?);
        batch.relations.extend(mapped_references.into_iter().map(
            |(related_post_provider_id, relation)| NormalizedRelation {
                parser_version: crate::PARSER_VERSION,
                post_provider_id: provider_id.to_owned(),
                related_post_provider_id,
                relation,
            },
        ));

        // Attachment keys without a matching included media object are
        // tolerated and skipped; their evidence survives raw-payload
        // retention (D14).
        if let Some(attachments) = post.attachments.as_ref() {
            for key in attachments.media_keys.iter().flatten() {
                if let Some(media_object) = media.get(key.as_str()).copied() {
                    batch
                        .media
                        .push(normalize_media_record(provider_id, media_object));
                }
            }
        }
    }

    batch
        .users
        .sort_by(|l, r| l.provider_id.cmp(&r.provider_id));
    batch
        .posts
        .sort_by(|l, r| l.provider_id.cmp(&r.provider_id));
    batch.relations.sort();
    batch.relations.dedup();
    batch.media.sort_by(|l, r| {
        (&*l.post_provider_id, &*l.provider_id).cmp(&(&*r.post_provider_id, &*r.provider_id))
    });
    Ok(batch)
}

/// Builds one normalized post record from its payload object.
///
/// # Errors
/// Returns [`NormalizeError::InvalidMember`] when the publication timestamp
/// fails strict parsing; validation has already guaranteed presence.
fn normalize_post(
    post: &PostObject,
    author_provider_id: &str,
    unresolved_references: Vec<Value>,
) -> Result<NormalizedPost, NormalizeError> {
    let raw_created_at = post.created_at.as_deref().unwrap_or_default();
    let parsed = DateTime::parse_from_rfc3339(raw_created_at).map_err(|_| {
        NormalizeError::InvalidMember {
            object: "post",
            member: "created_at",
        }
    })?;

    let metrics = post.public_metrics.as_ref();
    let mut media_keys = post
        .attachments
        .as_ref()
        .and_then(|a| a.media_keys.clone())
        .unwrap_or_default();
    media_keys.sort();
    media_keys.dedup();

    let mut expanded_urls: Vec<String> = post
        .entities
        .as_ref()
        .into_iter()
        .flat_map(|entities| &entities.urls)
        .filter_map(|url| url.unwound_url.clone().or_else(|| url.expanded_url.clone()))
        .collect();
    expanded_urls.sort();
    expanded_urls.dedup();

    let mut extension = post.extension.clone();
    if !unresolved_references.is_empty() {
        extension.insert(
            UNRESOLVED_REFERENCES_KEY.to_owned(),
            Value::Array(unresolved_references),
        );
    }

    Ok(NormalizedPost {
        parser_version: crate::PARSER_VERSION,
        provider_id: post.id.clone().unwrap_or_default(),
        author_provider_id: author_provider_id.to_owned(),
        text: post.text.clone().unwrap_or_default(),
        long_text: post.note_tweet.as_ref().and_then(|n| n.text.clone()),
        language: post.lang.clone(),
        published_at: Some(parsed.with_timezone(&Utc)),
        // The provider documents no exact edit timestamp member; honesty keeps
        // this unset instead of deriving one from edit metadata.
        edited_at: None,
        availability: Availability::Active,
        conversation_provider_id: post.conversation_id.clone(),
        like_count: metrics.and_then(|m| m.like_count),
        retweet_count: metrics.and_then(|m| m.retweet_count),
        reply_count: metrics.and_then(|m| m.reply_count),
        quote_count: metrics.and_then(|m| m.quote_count),
        bookmark_count: metrics.and_then(|m| m.bookmark_count),
        impression_count: metrics.and_then(|m| m.impression_count),
        media_keys,
        expanded_urls,
        extension,
    })
}
