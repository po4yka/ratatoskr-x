use super::source_validation::{
    forbidden_field_name, json_depth, merge_legacy_lists, nonempty, optional_string,
    optional_string_array, optional_timestamp_from_json, parse_legacy_list,
    parse_optional_timestamp, parse_required_timestamp, parse_x_status_url,
    reject_forbidden_fields, required_string, required_timestamp_from_json, sha256, sort_unique,
    sqlite_optional_string, sqlite_string, validate_field_theory_record_shape, validate_text,
};
use super::{
    LegacyRecord, LegacyRecordInput, LegacySourceKind, LegacySourceVersion, LegacyTransitionError,
    PathBuf, PreflightBatch, SourceLimits, Value,
};
use sqlx::Connection as _;
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use std::collections::BTreeMap;

const MONOLITH_HEADERS: &[&str] = &[
    "request_id",
    "bookmark_external_id",
    "x_category",
    "tweet_text",
    "tweet_text_tsv",
    "tweet_author",
    "tweet_url",
    "posted_at",
    "synced_at",
];

const FIELD_THEORY_SQLITE_COLUMNS: &[&str] = &[
    "id",
    "tweet_id",
    "url",
    "text",
    "author_handle",
    "author_name",
    "author_profile_image_url",
    "posted_at",
    "bookmarked_at",
    "synced_at",
    "conversation_id",
    "in_reply_to_status_id",
    "quoted_status_id",
    "language",
    "like_count",
    "repost_count",
    "reply_count",
    "quote_count",
    "bookmark_count",
    "view_count",
    "media_count",
    "link_count",
    "links_json",
    "tags_json",
    "ingested_via",
    "categories",
    "primary_category",
    "github_urls",
    "domains",
    "primary_domain",
    "quoted_tweet_json",
    "article_title",
    "article_text",
    "article_site",
    "enriched_at",
    "folder_ids",
    "folder_names",
];

const FIELD_THEORY_JSONL_FIELDS: &[&str] = &[
    "id",
    "tweetId",
    "authorHandle",
    "authorName",
    "authorProfileImageUrl",
    "author",
    "url",
    "text",
    "postedAt",
    "bookmarkedAt",
    "sortIndex",
    "syncedAt",
    "conversationId",
    "inReplyToStatusId",
    "inReplyToUserId",
    "quotedStatusId",
    "quotedTweet",
    "articleTitle",
    "articleText",
    "articleSite",
    "enrichedAt",
    "language",
    "sourceApp",
    "possiblySensitive",
    "engagement",
    "media",
    "mediaObjects",
    "links",
    "tags",
    "ingestedVia",
    "folderIds",
    "folderNames",
    "textExpandedAt",
    "quotedTweetFailedAt",
];

pub(super) async fn preflight_monolith_csv(
    path: PathBuf,
    limits: SourceLimits,
) -> Result<PreflightBatch, LegacyTransitionError> {
    let bytes = read_source_bytes(path, limits.max_bytes).await?;
    let source_digest = sha256(&bytes);
    let text = std::str::from_utf8(&bytes).map_err(|_| LegacyTransitionError::InvalidEncoding)?;
    let mut rows = parse_csv(text)?;
    if rows.is_empty()
        || rows
            .remove(0)
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            != MONOLITH_HEADERS
    {
        return Err(LegacyTransitionError::UnsupportedSourceVersion);
    }
    if rows.len() > limits.max_rows {
        return Err(LegacyTransitionError::RowLimitExceeded);
    }

    let mut records = Vec::with_capacity(rows.len());
    for row in rows {
        let [
            request_id,
            bookmark_external_id,
            category,
            tweet_text,
            _tweet_text_tsv,
            _tweet_author,
            tweet_url,
            posted_at,
            synced_at,
        ] = <[String; 9]>::try_from(row).map_err(|_| LegacyTransitionError::InvalidEncoding)?;
        validate_text("tweet_text", &tweet_text, limits.max_text_bytes)?;
        let provider_post_id = nonempty(&bookmark_external_id).map(ToOwned::to_owned);
        let (url_provider_post_id, canonical_url_digest) = parse_x_status_url(&tweet_url);
        let categories = nonempty(&category).map_or_else(Vec::new, |value| vec![value.to_owned()]);
        let record = build_record(LegacyRecordInput {
            source_record_key: request_id,
            provider_post_id,
            url_provider_post_id,
            canonical_url_digest,
            text: tweet_text,
            posted_at: parse_optional_timestamp("posted_at", &posted_at)?,
            bookmarked_at: None,
            synced_at: parse_required_timestamp("synced_at", &synced_at)?,
            categories,
            folder_ids: Vec::new(),
            folder_names: Vec::new(),
        })?;
        records.push(record);
    }

    finish_batch(
        LegacySourceKind::MonolithBookmarkMetadataCsv,
        LegacySourceVersion::MonolithBookmarkMetadata,
        source_digest,
        records,
    )
}

pub(super) async fn preflight_field_theory_jsonl(
    path: PathBuf,
    limits: SourceLimits,
) -> Result<PreflightBatch, LegacyTransitionError> {
    let bytes = read_source_bytes(path, limits.max_bytes).await?;
    let source_digest = sha256(&bytes);
    let text = std::str::from_utf8(&bytes).map_err(|_| LegacyTransitionError::InvalidEncoding)?;
    let mut records = Vec::new();

    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        if records.len() == limits.max_rows {
            return Err(LegacyTransitionError::RowLimitExceeded);
        }
        let value: Value =
            serde_json::from_str(line).map_err(|_| LegacyTransitionError::InvalidEncoding)?;
        if json_depth(&value) > limits.max_json_depth {
            return Err(LegacyTransitionError::JsonDepthExceeded);
        }
        reject_forbidden_fields(&value)?;
        let object = value
            .as_object()
            .ok_or(LegacyTransitionError::InvalidField { field: "record" })?;
        for field in object.keys() {
            if !FIELD_THEORY_JSONL_FIELDS.contains(&field.as_str()) {
                return Err(LegacyTransitionError::UnsupportedField {
                    field: field.clone(),
                });
            }
        }
        validate_field_theory_record_shape(object)?;

        let source_record_key = required_string(object, "id")?.to_owned();
        let provider_post_id = optional_string(object, "tweetId")?.map(ToOwned::to_owned);
        let url = required_string(object, "url")?;
        let text = required_string(object, "text")?.to_owned();
        validate_text("text", &text, limits.max_text_bytes)?;
        let (url_provider_post_id, canonical_url_digest) = parse_x_status_url(url);
        let folder_ids = optional_string_array(object, "folderIds")?;
        let folder_names = optional_string_array(object, "folderNames")?;
        let record = build_record(LegacyRecordInput {
            source_record_key,
            provider_post_id,
            url_provider_post_id,
            canonical_url_digest,
            text,
            posted_at: optional_timestamp_from_json(object, "postedAt")?,
            bookmarked_at: optional_timestamp_from_json(object, "bookmarkedAt")?,
            synced_at: required_timestamp_from_json(object, "syncedAt")?,
            categories: Vec::new(),
            folder_ids,
            folder_names,
        })?;
        records.push(record);
    }

    finish_batch(
        LegacySourceKind::FieldTheoryJsonl,
        LegacySourceVersion::FieldTheoryJsonl1,
        source_digest,
        records,
    )
}

pub(super) async fn preflight_field_theory_sqlite(
    path: PathBuf,
    limits: SourceLimits,
) -> Result<PreflightBatch, LegacyTransitionError> {
    let before = read_source_bytes(path.clone(), limits.max_bytes).await?;
    let source_digest = sha256(&before);
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .read_only(true)
        .create_if_missing(false)
        .immutable(true);
    let mut connection = SqliteConnection::connect_with(&options)
        .await
        .map_err(|_| LegacyTransitionError::SourceDatabase)?;

    let schema_version: String =
        sqlx::query_scalar("SELECT value FROM meta WHERE key = 'schema_version'")
            .fetch_one(&mut connection)
            .await
            .map_err(|_| LegacyTransitionError::SourceDatabase)?;
    if schema_version != "6" {
        return Err(LegacyTransitionError::UnsupportedSourceVersion);
    }
    let columns: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_table_info('bookmarks') ORDER BY cid")
            .fetch_all(&mut connection)
            .await
            .map_err(|_| LegacyTransitionError::SourceDatabase)?;
    if let Some(field) = columns.iter().find(|field| forbidden_field_name(field)) {
        return Err(LegacyTransitionError::ForbiddenField {
            field: field.clone(),
        });
    }
    if columns.iter().map(String::as_str).collect::<Vec<_>>() != FIELD_THEORY_SQLITE_COLUMNS {
        return Err(LegacyTransitionError::UnsupportedSqliteSchema);
    }

    let limit = i64::try_from(limits.max_rows)
        .unwrap_or(i64::MAX - 1)
        .saturating_add(1);
    let rows = sqlx::query(
        "SELECT id, tweet_id, url, text, posted_at, bookmarked_at, synced_at, \
                categories, primary_category, folder_ids, folder_names \
         FROM bookmarks ORDER BY id LIMIT ?",
    )
    .bind(limit)
    .fetch_all(&mut connection)
    .await
    .map_err(|_| LegacyTransitionError::SourceDatabase)?;
    if rows.len() > limits.max_rows {
        return Err(LegacyTransitionError::RowLimitExceeded);
    }

    let mut records = Vec::with_capacity(rows.len());
    for row in rows {
        let source_record_key = sqlite_string(&row, "id")?;
        let provider_post_id = Some(sqlite_string(&row, "tweet_id")?);
        let url = sqlite_string(&row, "url")?;
        let text = sqlite_string(&row, "text")?;
        validate_text("text", &text, limits.max_text_bytes)?;
        let posted_at = sqlite_optional_string(&row, "posted_at")?;
        let bookmarked_at = sqlite_optional_string(&row, "bookmarked_at")?;
        let synced_at = sqlite_string(&row, "synced_at")?;
        let categories = merge_legacy_lists(
            sqlite_optional_string(&row, "categories")?.as_deref(),
            sqlite_optional_string(&row, "primary_category")?.as_deref(),
        )?;
        let folder_ids = parse_legacy_list(sqlite_optional_string(&row, "folder_ids")?.as_deref())?;
        let folder_names =
            parse_legacy_list(sqlite_optional_string(&row, "folder_names")?.as_deref())?;
        let (url_provider_post_id, canonical_url_digest) = parse_x_status_url(&url);
        records.push(build_record(LegacyRecordInput {
            source_record_key,
            provider_post_id,
            url_provider_post_id,
            canonical_url_digest,
            text,
            posted_at: parse_optional_timestamp(
                "posted_at",
                posted_at.as_deref().unwrap_or_default(),
            )?,
            bookmarked_at: parse_optional_timestamp(
                "bookmarked_at",
                bookmarked_at.as_deref().unwrap_or_default(),
            )?,
            synced_at: parse_required_timestamp("synced_at", &synced_at)?,
            categories,
            folder_ids,
            folder_names,
        })?);
    }
    connection
        .close()
        .await
        .map_err(|_| LegacyTransitionError::SourceDatabase)?;
    let after = read_source_bytes(path, limits.max_bytes).await?;
    if before != after {
        return Err(LegacyTransitionError::SourceChanged);
    }

    finish_batch(
        LegacySourceKind::FieldTheorySqlite,
        LegacySourceVersion::FieldTheorySqlite6,
        source_digest,
        records,
    )
}

async fn read_source_bytes(
    path: PathBuf,
    max_bytes: u64,
) -> Result<Vec<u8>, LegacyTransitionError> {
    tokio::task::spawn_blocking(move || {
        let metadata =
            std::fs::metadata(&path).map_err(|_| LegacyTransitionError::SourceUnavailable)?;
        if !metadata.is_file() {
            return Err(LegacyTransitionError::SourceUnavailable);
        }
        if metadata.len() > max_bytes {
            return Err(LegacyTransitionError::SourceTooLarge);
        }
        let first = std::fs::read(&path).map_err(|_| LegacyTransitionError::SourceUnavailable)?;
        if u64::try_from(first.len()).unwrap_or(u64::MAX) > max_bytes {
            return Err(LegacyTransitionError::SourceTooLarge);
        }
        let second = std::fs::read(path).map_err(|_| LegacyTransitionError::SourceUnavailable)?;
        if u64::try_from(second.len()).unwrap_or(u64::MAX) > max_bytes {
            return Err(LegacyTransitionError::SourceTooLarge);
        }
        if first != second {
            return Err(LegacyTransitionError::SourceChanged);
        }
        Ok(first)
    })
    .await
    .map_err(|_| LegacyTransitionError::SourceUnavailable)?
}

fn parse_csv(source: &str) -> Result<Vec<Vec<String>>, LegacyTransitionError> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut characters = source.chars().peekable();
    let mut quoted = false;
    let mut just_closed_quote = false;

    while let Some(character) = characters.next() {
        if quoted {
            if character == '"' {
                if characters.peek() == Some(&'"') {
                    characters.next();
                    field.push('"');
                } else {
                    quoted = false;
                    just_closed_quote = true;
                }
            } else {
                field.push(character);
            }
            continue;
        }
        match character {
            '"' if field.is_empty() && !just_closed_quote => quoted = true,
            ',' => {
                row.push(std::mem::take(&mut field));
                just_closed_quote = false;
            }
            '\n' => {
                if field.ends_with('\r') && !just_closed_quote {
                    field.pop();
                }
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                just_closed_quote = false;
            }
            '\r' if characters.peek() == Some(&'\n') => {}
            _ if just_closed_quote => return Err(LegacyTransitionError::InvalidEncoding),
            _ => field.push(character),
        }
    }
    if quoted {
        return Err(LegacyTransitionError::InvalidEncoding);
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}

fn build_record(input: LegacyRecordInput) -> Result<LegacyRecord, LegacyTransitionError> {
    let LegacyRecordInput {
        source_record_key,
        provider_post_id,
        url_provider_post_id,
        canonical_url_digest,
        text,
        posted_at,
        bookmarked_at,
        synced_at,
        mut categories,
        mut folder_ids,
        mut folder_names,
    } = input;
    if source_record_key.is_empty() {
        return Err(LegacyTransitionError::InvalidField { field: "id" });
    }
    sort_unique(&mut categories);
    sort_unique(&mut folder_ids);
    sort_unique(&mut folder_names);
    let digest_value = serde_json::json!({
        "source_record_key": source_record_key,
        "provider_post_id": provider_post_id,
        "url_provider_post_id": url_provider_post_id,
        "canonical_url_digest": canonical_url_digest,
        "content_digest": sha256(text.as_bytes()),
        "posted_at": posted_at,
        "bookmarked_at": bookmarked_at,
        "synced_at": synced_at,
        "categories": categories,
        "folder_ids": folder_ids,
        "folder_names": folder_names,
    });
    let record_digest = sha256(
        &serde_json::to_vec(&digest_value).map_err(|_| LegacyTransitionError::InvalidEncoding)?,
    );
    Ok(LegacyRecord {
        source_record_key,
        provider_post_id,
        url_provider_post_id,
        canonical_url_digest,
        text,
        posted_at,
        bookmarked_at,
        synced_at,
        categories,
        folder_ids,
        folder_names,
        record_digest,
    })
}

fn finish_batch(
    source_kind: LegacySourceKind,
    source_version: LegacySourceVersion,
    source_digest: String,
    records: Vec<LegacyRecord>,
) -> Result<PreflightBatch, LegacyTransitionError> {
    let mut unique: BTreeMap<String, LegacyRecord> = BTreeMap::new();
    for record in records {
        if let Some(existing) = unique.get(record.source_record_key()) {
            if existing.record_digest() != record.record_digest() {
                return Err(LegacyTransitionError::DuplicateSourceKey);
            }
        } else {
            unique.insert(record.source_record_key.clone(), record);
        }
    }
    let records: Vec<_> = unique.into_values().collect();
    Ok(PreflightBatch {
        source_kind,
        source_version,
        source_digest,
        row_count: records.len(),
        records,
    })
}
