use super::{DateTime, LegacyTransitionError, Utc, Value};
use serde_json::Map;
use sha2::{Digest as _, Sha256};
use sqlx::Row as _;

pub(super) fn required_string<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
) -> Result<&'a str, LegacyTransitionError> {
    optional_string(object, field)?.ok_or(LegacyTransitionError::InvalidField { field })
}

pub(super) fn optional_string<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
) -> Result<Option<&'a str>, LegacyTransitionError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value)),
        _ => Err(LegacyTransitionError::InvalidField { field }),
    }
}

pub(super) fn optional_string_array(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<Vec<String>, LegacyTransitionError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                    .ok_or(LegacyTransitionError::InvalidField { field })
            })
            .collect(),
        _ => Err(LegacyTransitionError::InvalidField { field }),
    }
}

pub(super) fn validate_field_theory_record_shape(
    object: &Map<String, Value>,
) -> Result<(), LegacyTransitionError> {
    required_string(object, "tweetId")?;
    for field in [
        "tweetId",
        "authorHandle",
        "authorName",
        "authorProfileImageUrl",
        "postedAt",
        "bookmarkedAt",
        "sortIndex",
        "conversationId",
        "inReplyToStatusId",
        "inReplyToUserId",
        "quotedStatusId",
        "articleTitle",
        "articleText",
        "articleSite",
        "enrichedAt",
        "language",
        "sourceApp",
        "ingestedVia",
        "textExpandedAt",
        "quotedTweetFailedAt",
    ] {
        validate_optional_string_value(object, field)?;
    }
    validate_optional_bool_value(object, "possiblySensitive")?;
    if let Some(ingested_via) = optional_string(object, "ingestedVia")?
        && !matches!(ingested_via, "api" | "browser" | "graphql")
    {
        return Err(LegacyTransitionError::InvalidField {
            field: "ingestedVia",
        });
    }
    for field in ["media", "links", "tags", "folderIds", "folderNames"] {
        optional_string_array(object, field)?;
    }
    validate_optional_object(
        object,
        "author",
        &[
            "handle",
            "name",
            "profileImageUrl",
            "description",
            "location",
            "url",
            "verified",
            "followersCount",
            "followingCount",
            "statusesCount",
        ],
        validate_author_shape,
    )?;
    validate_optional_object(
        object,
        "engagement",
        &[
            "likeCount",
            "repostCount",
            "replyCount",
            "quoteCount",
            "bookmarkCount",
            "viewCount",
        ],
        validate_engagement_shape,
    )?;
    validate_optional_object(
        object,
        "quotedTweet",
        &[
            "id",
            "text",
            "authorHandle",
            "authorName",
            "authorProfileImageUrl",
            "postedAt",
            "media",
            "mediaObjects",
            "url",
        ],
        validate_quoted_tweet_shape,
    )?;
    validate_media_objects(object, "mediaObjects")
}

pub(super) fn validate_optional_string_value(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<(), LegacyTransitionError> {
    match object.get(field) {
        None | Some(Value::Null | Value::String(_)) => Ok(()),
        _ => Err(LegacyTransitionError::InvalidField { field }),
    }
}

pub(super) fn validate_optional_bool_value(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<(), LegacyTransitionError> {
    match object.get(field) {
        None | Some(Value::Bool(_)) => Ok(()),
        _ => Err(LegacyTransitionError::InvalidField { field }),
    }
}

pub(super) fn validate_optional_number_value(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<(), LegacyTransitionError> {
    match object.get(field) {
        None | Some(Value::Number(_)) => Ok(()),
        _ => Err(LegacyTransitionError::InvalidField { field }),
    }
}

pub(super) fn validate_optional_object(
    object: &Map<String, Value>,
    field: &'static str,
    allowed_fields: &[&str],
    validate: fn(&Map<String, Value>) -> Result<(), LegacyTransitionError>,
) -> Result<(), LegacyTransitionError> {
    match object.get(field) {
        None => Ok(()),
        Some(Value::Object(nested)) => {
            reject_unknown_fields(nested, allowed_fields)?;
            validate(nested)
        }
        _ => Err(LegacyTransitionError::InvalidField { field }),
    }
}

pub(super) fn reject_unknown_fields(
    object: &Map<String, Value>,
    allowed_fields: &[&str],
) -> Result<(), LegacyTransitionError> {
    for field in object.keys() {
        if !allowed_fields.contains(&field.as_str()) {
            return Err(LegacyTransitionError::UnsupportedField {
                field: field.clone(),
            });
        }
    }
    Ok(())
}

pub(super) fn validate_author_shape(
    object: &Map<String, Value>,
) -> Result<(), LegacyTransitionError> {
    for field in [
        "handle",
        "name",
        "profileImageUrl",
        "description",
        "location",
        "url",
    ] {
        validate_optional_string_value(object, field)?;
    }
    validate_optional_bool_value(object, "verified")?;
    for field in ["followersCount", "followingCount", "statusesCount"] {
        validate_optional_number_value(object, field)?;
    }
    Ok(())
}

pub(super) fn validate_engagement_shape(
    object: &Map<String, Value>,
) -> Result<(), LegacyTransitionError> {
    for field in [
        "likeCount",
        "repostCount",
        "replyCount",
        "quoteCount",
        "bookmarkCount",
        "viewCount",
    ] {
        validate_optional_number_value(object, field)?;
    }
    Ok(())
}

pub(super) fn validate_quoted_tweet_shape(
    object: &Map<String, Value>,
) -> Result<(), LegacyTransitionError> {
    required_string(object, "id")?;
    required_string(object, "text")?;
    required_string(object, "url")?;
    for field in [
        "authorHandle",
        "authorName",
        "authorProfileImageUrl",
        "postedAt",
    ] {
        validate_optional_string_value(object, field)?;
    }
    optional_string_array(object, "media")?;
    validate_media_objects(object, "mediaObjects")
}

pub(super) fn validate_media_objects(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<(), LegacyTransitionError> {
    let Some(value) = object.get(field) else {
        return Ok(());
    };
    let Value::Array(media_objects) = value else {
        return Err(LegacyTransitionError::InvalidField { field });
    };
    for media in media_objects {
        let Value::Object(media) = media else {
            return Err(LegacyTransitionError::InvalidField { field });
        };
        reject_unknown_fields(
            media,
            &[
                "url",
                "mediaUrl",
                "expandedUrl",
                "previewUrl",
                "type",
                "altText",
                "extAltText",
                "width",
                "height",
                "videoVariants",
                "variants",
            ],
        )?;
        for string_field in [
            "url",
            "mediaUrl",
            "expandedUrl",
            "previewUrl",
            "type",
            "altText",
            "extAltText",
        ] {
            validate_optional_string_value(media, string_field)?;
        }
        for number_field in ["width", "height"] {
            validate_optional_number_value(media, number_field)?;
        }
        for variants_field in ["videoVariants", "variants"] {
            validate_media_variants(media, variants_field)?;
        }
    }
    Ok(())
}

pub(super) fn validate_media_variants(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<(), LegacyTransitionError> {
    let Some(value) = object.get(field) else {
        return Ok(());
    };
    let Value::Array(variants) = value else {
        return Err(LegacyTransitionError::InvalidField { field });
    };
    for variant in variants {
        let Value::Object(variant) = variant else {
            return Err(LegacyTransitionError::InvalidField { field });
        };
        reject_unknown_fields(variant, &["url", "contentType", "bitrate"])?;
        validate_optional_string_value(variant, "url")?;
        validate_optional_string_value(variant, "contentType")?;
        validate_optional_number_value(variant, "bitrate")?;
    }
    Ok(())
}

pub(super) fn parse_optional_timestamp(
    field: &'static str,
    value: &str,
) -> Result<Option<DateTime<Utc>>, LegacyTransitionError> {
    nonempty(value)
        .map(|value| {
            DateTime::parse_from_rfc3339(value)
                .map(|timestamp| timestamp.with_timezone(&Utc))
                .map_err(|_| LegacyTransitionError::InvalidTimestamp { field })
        })
        .transpose()
}

pub(super) fn parse_required_timestamp(
    field: &'static str,
    value: &str,
) -> Result<DateTime<Utc>, LegacyTransitionError> {
    parse_optional_timestamp(field, value)?.ok_or(LegacyTransitionError::InvalidField { field })
}

pub(super) fn optional_timestamp_from_json(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<Option<DateTime<Utc>>, LegacyTransitionError> {
    optional_string(object, field)?
        .map(|value| parse_required_timestamp(field, value))
        .transpose()
}

pub(super) fn required_timestamp_from_json(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<DateTime<Utc>, LegacyTransitionError> {
    parse_required_timestamp(field, required_string(object, field)?)
}

pub(super) fn validate_text(
    field: &'static str,
    value: &str,
    max_bytes: usize,
) -> Result<(), LegacyTransitionError> {
    if value.len() > max_bytes {
        Err(LegacyTransitionError::TextLimitExceeded { field })
    } else {
        Ok(())
    }
}

pub(super) fn reject_forbidden_fields(value: &Value) -> Result<(), LegacyTransitionError> {
    match value {
        Value::Object(object) => {
            for (field, nested) in object {
                if forbidden_field_name(field) {
                    return Err(LegacyTransitionError::ForbiddenField {
                        field: field.clone(),
                    });
                }
                reject_forbidden_fields(nested)?;
            }
            Ok(())
        }
        Value::Array(values) => values.iter().try_for_each(reject_forbidden_fields),
        _ => Ok(()),
    }
}

pub(super) fn forbidden_field_name(field: &str) -> bool {
    let normalized: String = field
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect();
    [
        "token",
        "cookie",
        "session",
        "password",
        "secret",
        "authorization",
        "bearer",
        "credential",
    ]
    .iter()
    .any(|forbidden| normalized.contains(forbidden))
}

pub(super) fn json_depth(value: &Value) -> usize {
    match value {
        Value::Object(object) => 1 + object.values().map(json_depth).max().unwrap_or(0),
        Value::Array(values) => 1 + values.iter().map(json_depth).max().unwrap_or(0),
        _ => 0,
    }
}

pub(super) fn parse_x_status_url(value: &str) -> (Option<String>, Option<String>) {
    let Ok(mut url) = reqwest::Url::parse(value) else {
        return (None, None);
    };
    let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
        return (None, None);
    };
    if !matches!(
        host.as_str(),
        "x.com" | "www.x.com" | "twitter.com" | "www.twitter.com"
    ) {
        return (None, None);
    }
    let segments: Vec<_> = url
        .path_segments()
        .map(Iterator::collect)
        .unwrap_or_default();
    let Some(status_position) = segments.iter().position(|segment| *segment == "status") else {
        return (None, None);
    };
    let Some(provider_id) = segments
        .get(status_position + 1)
        .map(|value| (*value).to_owned())
    else {
        return (None, None);
    };
    if provider_id.is_empty()
        || !provider_id
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        return (None, None);
    }
    url.set_fragment(None);
    url.set_query(None);
    (Some(provider_id), Some(sha256(url.as_str().as_bytes())))
}

pub(super) fn sqlite_string(
    row: &sqlx::sqlite::SqliteRow,
    field: &'static str,
) -> Result<String, LegacyTransitionError> {
    row.try_get::<String, _>(field)
        .map_err(|_| LegacyTransitionError::InvalidField { field })
}

pub(super) fn sqlite_optional_string(
    row: &sqlx::sqlite::SqliteRow,
    field: &'static str,
) -> Result<Option<String>, LegacyTransitionError> {
    row.try_get::<Option<String>, _>(field)
        .map_err(|_| LegacyTransitionError::InvalidField { field })
}

pub(super) fn parse_legacy_list(value: Option<&str>) -> Result<Vec<String>, LegacyTransitionError> {
    let Some(value) = value.and_then(nonempty) else {
        return Ok(Vec::new());
    };
    if value.trim_start().starts_with('[') {
        let values: Vec<String> =
            serde_json::from_str(value).map_err(|_| LegacyTransitionError::InvalidEncoding)?;
        return Ok(values.into_iter().filter(|item| !item.is_empty()).collect());
    }
    Ok(value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

pub(super) fn merge_legacy_lists(
    categories: Option<&str>,
    primary: Option<&str>,
) -> Result<Vec<String>, LegacyTransitionError> {
    let mut values = parse_legacy_list(categories)?;
    if let Some(primary) = primary.and_then(nonempty) {
        values.push(primary.to_owned());
    }
    sort_unique(&mut values);
    Ok(values)
}

pub(super) fn sort_unique(values: &mut Vec<String>) {
    values.sort();
    values.dedup();
}

pub(super) fn nonempty(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

pub(super) fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        for nibble in [byte >> 4, byte & 0x0f] {
            if let Some(character) = char::from_digit(u32::from(nibble), 16) {
                encoded.push(character);
            }
        }
    }
    encoded
}

pub(super) fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
}
