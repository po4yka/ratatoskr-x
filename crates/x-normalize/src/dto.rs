//! Typed views of the official X API response envelope.
//!
//! Every known object deserializes tolerantly: members this parser generation
//! models get typed fields, everything else is captured verbatim into an
//! [`Extension`] map attached to the same object (the record-and-preserve
//! policy of the change design). Refusal happens only through
//! [`Envelope::validate`] when a documented required member is absent or
//! unparseable.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::NormalizeError;

/// Preserved material: payload members the current DTO layer does not model,
/// captured name-and-value intact at the object where they appeared.
pub type Extension = Map<String, Value>;

/// The official X API response envelope: a `data` array of post objects plus
/// the `includes` collections that carry authors and media.
#[derive(Debug, Clone, Deserialize)]
pub struct Envelope {
    /// The primary posts returned by the endpoint.
    #[serde(default)]
    data: Vec<PostObject>,
    /// Included child objects joined into the response by id.
    #[serde(default)]
    includes: Includes,
    /// Unmodeled envelope members, preserved verbatim.
    #[serde(flatten)]
    pub extension: Extension,
}

impl Envelope {
    /// The primary posts carried by the envelope.
    #[must_use]
    pub fn posts(&self) -> &[PostObject] {
        &self.data
    }

    /// The included user objects available for author resolution.
    #[must_use]
    pub fn included_users(&self) -> &[UserObject] {
        &self.includes.users
    }

    /// The included media objects available for attachment joining.
    #[must_use]
    pub fn included_media(&self) -> &[MediaObject] {
        &self.includes.media
    }

    /// The included embedded post objects preserved as relation-target
    /// evidence; they are not normalized into records themselves.
    #[must_use]
    pub fn included_tweets(&self) -> &[PostObject] {
        &self.includes.tweets
    }

    /// Unmodeled members of the `includes` container, preserved verbatim.
    #[must_use]
    pub fn included_extension(&self) -> &Extension {
        &self.includes.extension
    }

    /// Checks the members this archive requires before normalization: every
    /// post must carry a provider id, canonical text, an author id, and a
    /// parseable publication timestamp. An envelope without any posts is
    /// refused rather than silently producing nothing.
    ///
    /// # Errors
    /// Returns the typed refusal for the first offending object in decode
    /// order: [`NormalizeError::MissingMember`] for absent required members
    /// and [`NormalizeError::InvalidMember`] for a member present but
    /// unparseable.
    pub fn validate(&self) -> Result<(), NormalizeError> {
        if self.data.is_empty() {
            return Err(NormalizeError::MissingMember {
                object: "envelope",
                member: "data",
            });
        }
        for post in &self.data {
            require_member(post.id.as_deref(), "post", "id")?;
            require_member(post.text.as_deref(), "post", "text")?;
            require_member(post.author_id.as_deref(), "post", "author_id")?;
            let raw_timestamp = require_member(post.created_at.as_deref(), "post", "created_at")?;
            let parsed = chrono::DateTime::parse_from_rfc3339(raw_timestamp);
            if parsed.is_err() {
                return Err(NormalizeError::InvalidMember {
                    object: "post",
                    member: "created_at",
                });
            }
        }
        Ok(())
    }
}

/// Returns the member value or the typed absence refusal.
fn require_member<'a>(
    value: Option<&'a str>,
    object: &'static str,
    member: &'static str,
) -> Result<&'a str, NormalizeError> {
    match value {
        Some(found) if !found.is_empty() => Ok(found),
        _ => Err(NormalizeError::MissingMember { object, member }),
    }
}

/// The `includes` container: users, tweets, and media keyed by provider id.
///
/// Other documented containers (`polls`, `places`, `topics`) and any newer
/// container ride the preserved extension instead of being dropped.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Includes {
    /// User objects joined by author id.
    #[serde(default)]
    users: Vec<UserObject>,
    /// Embedded post objects (quoted or retweeted targets).
    #[serde(default)]
    tweets: Vec<PostObject>,
    /// Media objects joined through attachment keys.
    #[serde(default)]
    media: Vec<MediaObject>,
    /// Unmodeled include containers, preserved verbatim.
    #[serde(flatten)]
    pub extension: Extension,
}

/// A post object from the official API, carrying only what this parser
/// generation models; every other member lands in `extension`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PostObject {
    /// Provider identity of the post.
    pub id: Option<String>,
    /// Canonical short text.
    pub text: Option<String>,
    /// Provider identity of the author.
    pub author_id: Option<String>,
    /// Conversation linkage copied verbatim.
    pub conversation_id: Option<String>,
    /// Publication timestamp in the documented ISO 8601 shape.
    pub created_at: Option<String>,
    /// BCP47 language tag.
    pub lang: Option<String>,
    /// Public metric counters.
    pub public_metrics: Option<PublicMetrics>,
    /// References to other posts (replies, quotes, reposts).
    #[serde(default)]
    pub referenced_tweets: Option<Vec<ReferencedTweet>>,
    /// Long-form note body for posts longer than the canonical text.
    pub note_tweet: Option<NoteTweet>,
    /// Attached media keys and polls.
    pub attachments: Option<Attachments>,
    /// Edit metadata as supplied by the provider.
    pub edit_controls: Option<EditControls>,
    /// Edit history identifiers.
    #[serde(default)]
    pub edit_history_tweet_ids: Option<Vec<String>>,
    /// Unmodeled members, preserved verbatim (article wrappers included).
    #[serde(flatten)]
    pub extension: Extension,
}

/// A `referenced_tweets` entry. The reference type stays a plain string so an
/// unrecognized type is preserved and skipped rather than rejected.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ReferencedTweet {
    /// The provider's reference type string.
    #[serde(rename = "type")]
    pub reference_type: Option<String>,
    /// Provider identity of the referenced post.
    pub id: Option<String>,
    /// Unmodeled members of this reference entry, preserved verbatim.
    #[serde(flatten)]
    pub extension: Extension,
}

/// Public metric counters as last stated by the provider. Absence means the
/// provider never stated a value; it never means zero.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PublicMetrics {
    /// Repost count under the tweet-named wire field.
    pub retweet_count: Option<i64>,
    /// Reply count.
    pub reply_count: Option<i64>,
    /// Like count.
    pub like_count: Option<i64>,
    /// Quote count.
    pub quote_count: Option<i64>,
    /// Bookmark count.
    pub bookmark_count: Option<i64>,
    /// Impression count.
    pub impression_count: Option<i64>,
    /// Unmodeled metric members, preserved verbatim.
    #[serde(flatten)]
    pub extension: Extension,
}

/// The long-form note body delivered inline on long posts.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NoteTweet {
    /// Full text of the long-form body.
    pub text: Option<String>,
    /// Unmodeled members, preserved verbatim.
    #[serde(flatten)]
    pub extension: Extension,
}

/// Media keys (and polls) attached to a post.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Attachments {
    /// Provider keys of attached media objects.
    #[serde(default)]
    pub media_keys: Option<Vec<String>>,
    /// Provider ids of attached polls.
    #[serde(default)]
    pub poll_ids: Option<Vec<String>>,
    /// Unmodeled members, preserved verbatim.
    #[serde(flatten)]
    pub extension: Extension,
}

/// Edit metadata as supplied by the provider. No member of this object is an
/// exact edit timestamp; `editable_until` bounds eligibility only.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EditControls {
    /// Remaining edits while eligible.
    pub edits_remaining: Option<i64>,
    /// Whether the post may still be edited.
    pub is_edit_eligible: Option<bool>,
    /// Eligibility horizon, not an edit time.
    pub editable_until: Option<String>,
    /// Unmodeled members, preserved verbatim.
    #[serde(flatten)]
    pub extension: Extension,
}

/// A user object from the official API.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UserObject {
    /// Provider identity of the user.
    pub id: Option<String>,
    /// Display name.
    pub name: Option<String>,
    /// Handle.
    pub username: Option<String>,
    /// Account creation timestamp in the documented ISO 8601 shape.
    pub created_at: Option<String>,
    /// Profile description.
    pub description: Option<String>,
    /// Profile location.
    pub location: Option<String>,
    /// Protected-account flag.
    pub protected: Option<bool>,
    /// Legacy verified flag.
    pub verified: Option<bool>,
    /// Avatar URL.
    pub profile_image_url: Option<String>,
    /// Profile URL.
    pub url: Option<String>,
    /// Pinned post provider id.
    pub pinned_tweet_id: Option<String>,
    /// Public follower and post counters.
    pub public_metrics: Option<UserPublicMetrics>,
    /// Unmodeled members, preserved verbatim.
    #[serde(flatten)]
    pub extension: Extension,
}

/// Public follower and post counters of a user.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UserPublicMetrics {
    /// Follower count.
    pub followers_count: Option<i64>,
    /// Following count.
    pub following_count: Option<i64>,
    /// Post count under the tweet-named wire field.
    pub tweet_count: Option<i64>,
    /// List membership count.
    pub listed_count: Option<i64>,
    /// Unmodeled metric members, preserved verbatim.
    #[serde(flatten)]
    pub extension: Extension,
}

/// A media object joined through attachment keys. Metadata only; nothing here
/// implies downloading bytes.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MediaObject {
    /// Provider key of the media object.
    pub media_key: Option<String>,
    /// Provider kind string (`photo`, `video`, or `animated_gif`).
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// Display URL for photos.
    pub url: Option<String>,
    /// Static preview URL for videos and animated GIFs.
    pub preview_image_url: Option<String>,
    /// Pixel width.
    pub width: Option<i64>,
    /// Pixel height.
    pub height: Option<i64>,
    /// Accessibility description supplied by the author.
    pub alt_text: Option<String>,
    /// Video duration in milliseconds.
    pub duration_ms: Option<i64>,
    /// Rendition variants for videos.
    #[serde(default)]
    pub variants: Option<Vec<MediaVariant>>,
    /// View counters.
    pub public_metrics: Option<MediaPublicMetrics>,
    /// Unmodeled members, preserved verbatim.
    #[serde(flatten)]
    pub extension: Extension,
}

/// One rendition variant of a video.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MediaVariant {
    /// Bit rate; absent for playlist renditions.
    pub bit_rate: Option<i64>,
    /// MIME content type.
    pub content_type: Option<String>,
    /// Rendition URL.
    pub url: Option<String>,
    /// Unmodeled members, preserved verbatim.
    #[serde(flatten)]
    pub extension: Extension,
}

/// View counters of a media object.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MediaPublicMetrics {
    /// View count.
    pub view_count: Option<i64>,
    /// Unmodeled metric members, preserved verbatim.
    #[serde(flatten)]
    pub extension: Extension,
}
