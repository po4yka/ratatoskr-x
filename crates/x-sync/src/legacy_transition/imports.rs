use super::source_validation::sha256;
use super::{
    DateTime, IMPORTER_PARSER_VERSION, ImportCounts, LegacyRecord, LegacyResolution,
    LegacySourceKind, LegacySourceVersion, LegacyTransitionError, OwnershipApproval,
    PreflightBatch, Utc, Value,
};

pub(super) type ExistingImportRow = (uuid::Uuid, String, i32, i32, i32, i32, i32, i32);

pub(super) async fn validate_locked_import_account(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    approval: &OwnershipApproval,
    verified_provider_user_id: &str,
) -> Result<(), LegacyTransitionError> {
    let locked: Option<(uuid::Uuid, String, String)> = sqlx::query_as(
        "select internal_user_id, provider_user_id, state \
         from x_archive.accounts where id = $1 for update",
    )
    .bind(approval.account_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| LegacyTransitionError::Database)?;
    let (internal_owner_id, provider_user_id, state) =
        locked.ok_or(LegacyTransitionError::TargetAccountNotFound)?;
    if internal_owner_id != approval.internal_owner_id {
        return Err(LegacyTransitionError::InvalidOwnershipApproval);
    }
    if state != "connected" {
        return Err(LegacyTransitionError::TargetAccountNotConnected);
    }
    if provider_user_id != verified_provider_user_id {
        return Err(LegacyTransitionError::CurrentIdentityMismatch);
    }
    Ok(())
}

pub(super) async fn persist_import_records(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: uuid::Uuid,
    records: &[LegacyRecord],
) -> Result<ImportCounts, LegacyTransitionError> {
    let mut counts = ImportCounts::default();
    for record in records {
        let (resolution, resolved_provider_id, conflicting_provider_id) =
            resolve_record_identity(record);
        let post_id = if let Some(provider_post_id) = resolved_provider_id.as_deref() {
            Some(upsert_imported_post(transaction, provider_post_id, record, &mut counts).await?)
        } else {
            match resolution {
                LegacyResolution::IdentityConflict => counts.conflicted += 1,
                LegacyResolution::Unmapped => counts.unmapped += 1,
                LegacyResolution::ProviderPost | LegacyResolution::UrlDerivedPost => {}
            }
            None
        };
        let category_metadata = serde_json::to_value(&record.categories)
            .map_err(|_| LegacyTransitionError::Database)?;
        let folder_metadata: Vec<Value> = record
            .folder_ids
            .iter()
            .map(|id| serde_json::json!({"id": id}))
            .chain(
                record
                    .folder_names
                    .iter()
                    .map(|name| serde_json::json!({"name": name})),
            )
            .collect();
        sqlx::query(
            "insert into x_archive.legacy_import_items \
                 (import_run_id, source_record_key, source_row_digest, provider_post_id, \
                  conflicting_provider_post_id, post_id, canonical_url_digest, content_digest, \
                  source_observed_saved_at, source_synced_at, legacy_category_metadata, \
                  legacy_folder_metadata, author_identity_resolved, resolution, provenance) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, false, $13, \
                     'legacy-import')",
        )
        .bind(run_id)
        .bind(&record.source_record_key)
        .bind(&record.record_digest)
        .bind(resolved_provider_id.as_deref().or_else(|| {
            record
                .provider_post_id
                .as_deref()
                .filter(|provider_id| valid_provider_post_id(provider_id))
        }))
        .bind(conflicting_provider_id)
        .bind(post_id)
        .bind(&record.canonical_url_digest)
        .bind(sha256(record.text.as_bytes()))
        .bind(record.bookmarked_at)
        .bind(record.synced_at)
        .bind(category_metadata)
        .bind(Value::Array(folder_metadata))
        .bind(resolution_database_value(resolution))
        .execute(&mut **transaction)
        .await
        .map_err(|_| LegacyTransitionError::Database)?;
    }
    Ok(counts)
}

async fn upsert_imported_post(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    provider_post_id: &str,
    record: &LegacyRecord,
    counts: &mut ImportCounts,
) -> Result<uuid::Uuid, LegacyTransitionError> {
    let existing: Option<(uuid::Uuid, String, String, Option<DateTime<Utc>>)> = sqlx::query_as(
        "select id, normalization_provenance, text, published_at \
         from x_archive.posts where provider_id = $1 for update",
    )
    .bind(provider_post_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| LegacyTransitionError::Database)?;
    let Some((post_id, provenance, text, published_at)) = existing else {
        let post_id = sqlx::query_scalar(
            "insert into x_archive.posts \
                 (provider_id, author_user_id, text, published_at, parser_version, \
                  normalization_provenance, availability) \
             values ($1, null, $2, $3, $4, 'legacy-import', 'unknown') returning id",
        )
        .bind(provider_post_id)
        .bind(&record.text)
        .bind(record.posted_at)
        .bind(IMPORTER_PARSER_VERSION)
        .fetch_one(&mut **transaction)
        .await
        .map_err(|_| LegacyTransitionError::Database)?;
        counts.inserted += 1;
        return Ok(post_id);
    };
    if provenance == "legacy-import" && (text != record.text || published_at != record.posted_at) {
        sqlx::query(
            "update x_archive.posts \
             set text = $2, published_at = $3, parser_version = $4, availability = 'unknown' \
             where id = $1 and normalization_provenance = 'legacy-import'",
        )
        .bind(post_id)
        .bind(&record.text)
        .bind(record.posted_at)
        .bind(IMPORTER_PARSER_VERSION)
        .execute(&mut **transaction)
        .await
        .map_err(|_| LegacyTransitionError::Database)?;
        counts.updated += 1;
    } else {
        counts.matched += 1;
    }
    Ok(post_id)
}

pub(super) fn source_database_identity(
    batch: &PreflightBatch,
) -> Result<(&'static str, i32), LegacyTransitionError> {
    match (batch.source_kind, batch.source_version) {
        (
            LegacySourceKind::MonolithBookmarkMetadataCsv,
            LegacySourceVersion::MonolithBookmarkMetadata,
        ) => Ok(("monolith_csv", 1)),
        (LegacySourceKind::FieldTheoryJsonl, LegacySourceVersion::FieldTheoryJsonl1) => {
            Ok(("field_theory_jsonl", 1))
        }
        (LegacySourceKind::FieldTheorySqlite, LegacySourceVersion::FieldTheorySqlite6) => {
            Ok(("field_theory_sqlite", 6))
        }
        _ => Err(LegacyTransitionError::UnsupportedSourceVersion),
    }
}

fn resolve_record_identity(
    record: &LegacyRecord,
) -> (LegacyResolution, Option<String>, Option<String>) {
    let explicit = record
        .provider_post_id
        .as_deref()
        .filter(|provider_id| valid_provider_post_id(provider_id));
    let from_url = record
        .url_provider_post_id
        .as_deref()
        .filter(|provider_id| valid_provider_post_id(provider_id));
    match (explicit, from_url) {
        (Some(explicit), Some(from_url)) if explicit != from_url => (
            LegacyResolution::IdentityConflict,
            None,
            Some(from_url.to_owned()),
        ),
        (Some(explicit), _) => (
            LegacyResolution::ProviderPost,
            Some(explicit.to_owned()),
            None,
        ),
        (None, Some(from_url)) => (
            LegacyResolution::UrlDerivedPost,
            Some(from_url.to_owned()),
            None,
        ),
        (None, None) => (LegacyResolution::Unmapped, None, None),
    }
}

fn valid_provider_post_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 19
        && value.chars().all(|character| character.is_ascii_digit())
}

fn resolution_database_value(resolution: LegacyResolution) -> &'static str {
    match resolution {
        LegacyResolution::ProviderPost => "provider_post",
        LegacyResolution::UrlDerivedPost => "canonical_url",
        LegacyResolution::IdentityConflict => "identity_conflict",
        LegacyResolution::Unmapped => "unmapped",
    }
}

pub(super) fn bounded_count(value: u64) -> Result<i32, LegacyTransitionError> {
    i32::try_from(value).map_err(|_| LegacyTransitionError::Database)
}

pub(super) fn nonnegative_count(value: i32) -> Result<u64, LegacyTransitionError> {
    u64::try_from(value).map_err(|_| LegacyTransitionError::Database)
}
