use super::source_validation::sha256;
use super::{
    LegacyTransitionError, ShadowDiff, ShadowDiffClass, ShadowReport, ShadowSummary, Value,
};
use serde_json::Map;
use sqlx::Row as _;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug)]
pub(super) struct ShadowLegacyRow {
    row_digest: String,
    provider_post_id: Option<String>,
    content_digest: Option<String>,
    category_metadata: Value,
    folder_metadata: Value,
    resolution: String,
}

pub(super) async fn load_shadow_rows(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    import_run_id: uuid::Uuid,
    snapshot_id: uuid::Uuid,
) -> Result<(Vec<ShadowLegacyRow>, Vec<(String, String)>), LegacyTransitionError> {
    let item_rows = sqlx::query(
        "select source_row_digest, provider_post_id, content_digest, \
                legacy_category_metadata, legacy_folder_metadata, resolution \
         from x_archive.legacy_import_items \
         where import_run_id = $1 order by source_row_digest",
    )
    .bind(import_run_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| LegacyTransitionError::Database)?;
    let mut legacy_rows = Vec::with_capacity(item_rows.len());
    for row in item_rows {
        legacy_rows.push(ShadowLegacyRow {
            row_digest: row
                .try_get("source_row_digest")
                .map_err(|_| LegacyTransitionError::Database)?,
            provider_post_id: row
                .try_get("provider_post_id")
                .map_err(|_| LegacyTransitionError::Database)?,
            content_digest: row
                .try_get("content_digest")
                .map_err(|_| LegacyTransitionError::Database)?,
            category_metadata: row
                .try_get("legacy_category_metadata")
                .map_err(|_| LegacyTransitionError::Database)?,
            folder_metadata: row
                .try_get("legacy_folder_metadata")
                .map_err(|_| LegacyTransitionError::Database)?,
            resolution: row
                .try_get("resolution")
                .map_err(|_| LegacyTransitionError::Database)?,
        });
    }
    let official_rows = sqlx::query_as(
        "select post.provider_id, post.text \
         from x_archive.snapshot_bookmark_items item \
         join x_archive.posts post on post.id = item.post_id \
         where item.snapshot_id = $1 order by post.provider_id",
    )
    .bind(snapshot_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| LegacyTransitionError::Database)?;
    Ok((legacy_rows, official_rows))
}

pub(super) fn build_shadow_report(
    account_id: uuid::Uuid,
    import_run_id: uuid::Uuid,
    snapshot_id: uuid::Uuid,
    legacy_rows: Vec<ShadowLegacyRow>,
    official_rows: Vec<(String, String)>,
) -> Result<ShadowReport, LegacyTransitionError> {
    let import_digest = sha256(
        legacy_rows
            .iter()
            .map(|row| row.row_digest.as_str())
            .collect::<Vec<_>>()
            .join("\n")
            .as_bytes(),
    );
    let snapshot_digest = sha256(
        official_rows
            .iter()
            .map(|(provider_id, text)| format!("{provider_id}:{}", sha256(text.as_bytes())))
            .collect::<Vec<_>>()
            .join("\n")
            .as_bytes(),
    );
    let entries = classify_shadow_entries(legacy_rows, official_rows)?;
    Ok(ShadowReport {
        report_version: 1,
        account_id,
        import_run_id,
        snapshot_id,
        import_digest,
        snapshot_digest,
        summary: ShadowSummary {
            matched: shadow_count(&entries, ShadowDiffClass::Matched),
            url_matched: shadow_count(&entries, ShadowDiffClass::UrlMatched),
            legacy_only: shadow_count(&entries, ShadowDiffClass::LegacyOnly),
            official_only: shadow_count(&entries, ShadowDiffClass::OfficialOnly),
            identity_conflicts: shadow_count(&entries, ShadowDiffClass::IdentityConflict),
            unmapped: shadow_count(&entries, ShadowDiffClass::Unmapped),
        },
        entries,
    })
}

pub(super) fn classify_shadow_entries(
    legacy_rows: Vec<ShadowLegacyRow>,
    official_rows: Vec<(String, String)>,
) -> Result<Vec<ShadowDiff>, LegacyTransitionError> {
    let official: BTreeMap<String, String> = official_rows.into_iter().collect();
    let mut matched_official_ids = BTreeSet::new();
    let mut entries = Vec::with_capacity(legacy_rows.len() + official.len());
    for legacy in legacy_rows {
        let category_present = json_array_has_values(&legacy.category_metadata);
        let folder_present = json_array_has_values(&legacy.folder_metadata);
        let entry = match legacy.resolution.as_str() {
            "identity_conflict" => ShadowDiff {
                class: ShadowDiffClass::IdentityConflict,
                provider_post_id: legacy.provider_post_id,
                legacy_record_digest: Some(legacy.row_digest),
                content_changed: false,
                legacy_category_present: category_present,
                legacy_folder_present: folder_present,
            },
            "unmapped" => ShadowDiff {
                class: ShadowDiffClass::Unmapped,
                provider_post_id: None,
                legacy_record_digest: Some(legacy.row_digest),
                content_changed: false,
                legacy_category_present: category_present,
                legacy_folder_present: folder_present,
            },
            "provider_post" | "canonical_url" => {
                let provider_post_id = legacy
                    .provider_post_id
                    .ok_or(LegacyTransitionError::Database)?;
                if let Some(official_text) = official.get(&provider_post_id) {
                    matched_official_ids.insert(provider_post_id.clone());
                    ShadowDiff {
                        class: if legacy.resolution == "canonical_url" {
                            ShadowDiffClass::UrlMatched
                        } else {
                            ShadowDiffClass::Matched
                        },
                        provider_post_id: Some(provider_post_id),
                        legacy_record_digest: Some(legacy.row_digest),
                        content_changed: legacy
                            .content_digest
                            .as_deref()
                            .is_some_and(|digest| digest != sha256(official_text.as_bytes())),
                        legacy_category_present: category_present,
                        legacy_folder_present: folder_present,
                    }
                } else {
                    ShadowDiff {
                        class: ShadowDiffClass::LegacyOnly,
                        provider_post_id: Some(provider_post_id),
                        legacy_record_digest: Some(legacy.row_digest),
                        content_changed: false,
                        legacy_category_present: category_present,
                        legacy_folder_present: folder_present,
                    }
                }
            }
            _ => return Err(LegacyTransitionError::Database),
        };
        entries.push(entry);
    }
    entries.extend(official.into_keys().filter_map(|provider_post_id| {
        (!matched_official_ids.contains(&provider_post_id)).then_some(ShadowDiff {
            class: ShadowDiffClass::OfficialOnly,
            provider_post_id: Some(provider_post_id),
            legacy_record_digest: None,
            content_changed: false,
            legacy_category_present: false,
            legacy_folder_present: false,
        })
    }));
    entries.sort_by(|left, right| {
        (
            left.class,
            left.provider_post_id.as_deref(),
            left.legacy_record_digest.as_deref(),
        )
            .cmp(&(
                right.class,
                right.provider_post_id.as_deref(),
                right.legacy_record_digest.as_deref(),
            ))
    });
    Ok(entries)
}

pub(super) async fn persist_shadow_report(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    report: &ShadowReport,
) -> Result<(uuid::Uuid, String, bool), LegacyTransitionError> {
    let report_payload =
        serde_json::to_value(report).map_err(|_| LegacyTransitionError::Database)?;
    let report_digest =
        sha256(&serde_json::to_vec(report).map_err(|_| LegacyTransitionError::Database)?);
    let inserted: Option<uuid::Uuid> = sqlx::query_scalar(
        "insert into x_archive.legacy_shadow_reports \
             (account_id, import_run_id, snapshot_id, import_digest, snapshot_digest, \
              report_payload, report_digest) \
         values ($1, $2, $3, $4, $5, $6, $7) \
         on conflict (account_id, import_run_id, snapshot_id, import_digest, snapshot_digest) \
         do nothing returning id",
    )
    .bind(report.account_id)
    .bind(report.import_run_id)
    .bind(report.snapshot_id)
    .bind(&report.import_digest)
    .bind(&report.snapshot_digest)
    .bind(report_payload)
    .bind(&report_digest)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| LegacyTransitionError::Database)?;
    if let Some(report_id) = inserted {
        return Ok((report_id, report_digest, false));
    }
    let existing: (uuid::Uuid, String) = sqlx::query_as(
        "select id, report_digest from x_archive.legacy_shadow_reports \
         where account_id = $1 and import_run_id = $2 and snapshot_id = $3 \
           and import_digest = $4 and snapshot_digest = $5",
    )
    .bind(report.account_id)
    .bind(report.import_run_id)
    .bind(report.snapshot_id)
    .bind(&report.import_digest)
    .bind(&report.snapshot_digest)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|_| LegacyTransitionError::Database)?;
    if existing.1 != report_digest {
        return Err(LegacyTransitionError::Database);
    }
    Ok((existing.0, report_digest, true))
}

pub(super) fn json_array_has_values(value: &Value) -> bool {
    value.as_array().is_some_and(|values| !values.is_empty())
}

pub(super) fn shadow_count(entries: &[ShadowDiff], class: ShadowDiffClass) -> u64 {
    entries
        .iter()
        .filter(|entry| entry.class == class)
        .fold(0_u64, |count, _| count.saturating_add(1))
}

pub(super) fn report_summary_count(
    summary: &Map<String, Value>,
    field: &'static str,
) -> Result<u64, LegacyTransitionError> {
    summary
        .get(field)
        .and_then(Value::as_u64)
        .ok_or(LegacyTransitionError::Database)
}
