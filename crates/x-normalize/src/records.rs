//! Row-shaped normalized records: the output contract of normalization.
//!
//! Fields map one-to-one onto the `x_archive` columns of their tables minus
//! database-owned columns (uuid surrogate keys and audit timestamps). Every
//! record carries [`PARSER_VERSION`] so persisted rows remember which parser
//! produced them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::dto::Extension;

/// Upstream availability vocabulary mirrored from the schema constraint.
///
/// Normalization from a live payload emits only [`Availability::Active`];
/// the other states arrive later through compliance revalidation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// The post is present and served by the provider.
    Active,
    /// The provider reports the post deleted.
    Deleted,
    /// The author's account is protected.
    Protected,
    /// The author's account is suspended.
    AuthorSuspended,
    /// The post is unavailable for another stated reason.
    Unavailable,
    /// The provider supplies no usable state.
    Unknown,
}

/// The closed relation vocabulary of `x_archive.post_relations`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    /// A reply to another post.
    Reply,
    /// A quote of another post.
    Quote,
    /// A repost of another post.
    Repost,
}

/// The media kinds admitted by the schema constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// A still image.
    Photo,
    /// A video with duration and renditions.
    Video,
    /// An animated GIF.
    AnimatedGif,
}

/// A normalized author row for `x_archive.users`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalizedUser {
    /// Parser generation that produced this record.
    pub parser_version: i32,
    /// Provider identity of the user.
    pub provider_id: String,
    /// Handle at normalization time.
    pub username: Option<String>,
    /// Display name at normalization time.
    pub display_name: Option<String>,
    /// Preserved unmodeled payload members of the user object.
    pub extension: Extension,
}

/// A normalized post row for `x_archive.posts`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalizedPost {
    /// Parser generation that produced this record.
    pub parser_version: i32,
    /// Provider identity of the post.
    pub provider_id: String,
    /// Provider identity of the author; persistence resolves the uuid.
    pub author_provider_id: String,
    /// Canonical short text; never overwritten by long-form content.
    pub text: String,
    /// Long-form note body when the provider supplies one.
    pub long_text: Option<String>,
    /// BCP47 language tag when supplied.
    pub language: Option<String>,
    /// Publication time parsed strictly from the provider timestamp.
    pub published_at: Option<DateTime<Utc>>,
    /// Exact edit time; always unset unless the provider ever documents one.
    pub edited_at: Option<DateTime<Utc>>,
    /// Availability as observed through this payload.
    pub availability: Availability,
    /// Conversation linkage copied verbatim from the payload.
    pub conversation_provider_id: Option<String>,
    /// Like count as last stated; absent means never stated.
    pub like_count: Option<i64>,
    /// Repost count as last stated; absent means never stated.
    pub retweet_count: Option<i64>,
    /// Reply count as last stated; absent means never stated.
    pub reply_count: Option<i64>,
    /// Quote count as last stated; absent means never stated.
    pub quote_count: Option<i64>,
    /// Bookmark count as last stated; absent means never stated.
    pub bookmark_count: Option<i64>,
    /// Impression count as last stated; absent means never stated.
    pub impression_count: Option<i64>,
    /// Attachment keys owned by this post, sorted without duplicates.
    pub media_keys: Vec<String>,
    /// Provider-resolved URL entities, sorted and deduplicated without fetching them.
    pub expanded_urls: Vec<String>,
    /// Preserved unmodeled payload members plus reserved-key material
    /// (`ratatoskr.x/unresolved_references`) for unmapped reference types.
    pub extension: Extension,
}

/// A normalized relation row for `x_archive.post_relations`, keyed by
/// provider identities so targets may dangle outside the batch.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NormalizedRelation {
    /// Parser generation that produced this record.
    pub parser_version: i32,
    /// Provider identity of the owning post.
    pub post_provider_id: String,
    /// Provider identity of the related post, possibly absent from the batch.
    pub related_post_provider_id: String,
    /// Which closed-vocabulary relation links the two posts.
    pub relation: Relation,
}

/// A normalized media metadata row for `x_archive.media`. Bytes are never
/// fetched or stored here, so blob references stay unset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalizedMedia {
    /// Parser generation that produced this record.
    pub parser_version: i32,
    /// Provider identity of the owning post.
    pub post_provider_id: String,
    /// Provider key of the media object.
    pub provider_id: String,
    /// Media kind when the payload states a known kind.
    pub kind: Option<MediaKind>,
    /// Metadata document: documented fields plus preserved extension material
    /// under the reserved `extension` key.
    pub metadata: serde_json::Value,
    /// Blob reference; normalization never sets it.
    pub blob_ref: Option<String>,
}
