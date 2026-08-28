use super::imports::{
    ExistingImportRow, bounded_count, nonnegative_count, persist_import_records,
    source_database_identity, validate_locked_import_account,
};
use super::shadow::{
    build_shadow_report, load_shadow_rows, persist_shadow_report, report_summary_count,
};
use super::source_validation::{sha256, valid_sha256};
use super::sources::{
    preflight_field_theory_jsonl, preflight_field_theory_sqlite, preflight_monolith_csv,
};
use super::{
    ApprovalDecision, ChecklistOutcome, CurrentAccountIdentity, Database, DateTime,
    IMPORTER_PARSER_VERSION, ImportCounts, ImportOutcome, LegacySourceSelection,
    LegacyTransitionError, LegacyTransitionService, OwnershipApproval, PreflightBatch,
    ShadowReportOutcome, SourceLimits, TransitionApprovalOutcome, TransitionApprovalRequest, Utc,
    Value,
};
use sqlx::Row as _;

impl LegacyTransitionService {
    /// Builds the transition service over the existing bounded database pool.
    #[must_use]
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    /// Exposes the owned database to the implementation tasks that follow.
    #[must_use]
    pub fn database(&self) -> &Database {
        &self.database
    }

    /// Compilable RED seam for source preflight.
    ///
    /// # Errors
    /// Always returns [`LegacyTransitionError::NotImplemented`] until source behavior lands.
    pub async fn preflight(
        &self,
        selection: LegacySourceSelection,
        limits: SourceLimits,
    ) -> Result<PreflightBatch, LegacyTransitionError> {
        Self::preflight_source(selection, limits).await
    }

    /// Validates one source without connecting to the target database or provider.
    ///
    /// # Errors
    /// Always returns [`LegacyTransitionError::NotImplemented`] until source behavior lands.
    pub async fn preflight_source(
        selection: LegacySourceSelection,
        limits: SourceLimits,
    ) -> Result<PreflightBatch, LegacyTransitionError> {
        match selection {
            LegacySourceSelection::MonolithCsv(path) => preflight_monolith_csv(path, limits).await,
            LegacySourceSelection::FieldTheory { jsonl, sqlite } => {
                if let Some(path) = jsonl {
                    preflight_field_theory_jsonl(path, limits).await
                } else if let Some(path) = sqlite {
                    preflight_field_theory_sqlite(path, limits).await
                } else {
                    Err(LegacyTransitionError::MissingSource)
                }
            }
        }
    }

    /// Imports one preflight batch only after explicit mapping and current OAuth identity checks.
    ///
    /// # Errors
    /// Returns a classified refusal before writes, or an atomic database failure.
    pub async fn import_batch(
        &self,
        identity: &dyn CurrentAccountIdentity,
        batch: &PreflightBatch,
        approval: Option<&OwnershipApproval>,
    ) -> Result<ImportOutcome, LegacyTransitionError> {
        let approval = approval.ok_or(LegacyTransitionError::MissingOwnershipApproval)?;
        if approval.source_digest != batch.source_digest || !valid_sha256(&approval.approval_digest)
        {
            return Err(LegacyTransitionError::InvalidOwnershipApproval);
        }

        let account: Option<(uuid::Uuid, String, String)> = sqlx::query_as(
            "select internal_user_id, provider_user_id, state \
             from x_archive.accounts where id = $1",
        )
        .bind(approval.account_id)
        .fetch_optional(self.database.pool())
        .await
        .map_err(|_| LegacyTransitionError::Database)?;
        let (internal_owner_id, stored_provider_user_id, state) =
            account.ok_or(LegacyTransitionError::TargetAccountNotFound)?;
        if internal_owner_id != approval.internal_owner_id {
            return Err(LegacyTransitionError::InvalidOwnershipApproval);
        }
        if state != "connected" {
            return Err(LegacyTransitionError::TargetAccountNotConnected);
        }
        let current_provider_user_id = identity.provider_user_id(approval.account_id).await?;
        if current_provider_user_id != stored_provider_user_id {
            return Err(LegacyTransitionError::CurrentIdentityMismatch);
        }

        self.persist_import(batch, approval, &stored_provider_user_id)
            .await
    }

    async fn persist_import(
        &self,
        batch: &PreflightBatch,
        approval: &OwnershipApproval,
        verified_provider_user_id: &str,
    ) -> Result<ImportOutcome, LegacyTransitionError> {
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(|_| LegacyTransitionError::Database)?;
        validate_locked_import_account(&mut transaction, approval, verified_provider_user_id)
            .await?;

        let (source_kind, source_version) = source_database_identity(batch)?;
        let existing: Option<ExistingImportRow> = sqlx::query_as(
            "select id, status, inserted_count, matched_count, updated_count, \
                        conflicted_count, rejected_count, unmapped_count \
                 from x_archive.legacy_import_runs \
                 where account_id = $1 and source_kind = $2 and source_version = $3 \
                   and source_digest = $4 and importer_parser_version = $5",
        )
        .bind(approval.account_id)
        .bind(source_kind)
        .bind(source_version)
        .bind(&batch.source_digest)
        .bind(IMPORTER_PARSER_VERSION)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| LegacyTransitionError::Database)?;
        if let Some((run_id, status, inserted, matched, updated, conflicted, rejected, unmapped)) =
            existing
        {
            if status != "completed" {
                return Err(LegacyTransitionError::Database);
            }
            transaction
                .commit()
                .await
                .map_err(|_| LegacyTransitionError::Database)?;
            return Ok(ImportOutcome {
                run_id,
                counts: ImportCounts {
                    inserted: nonnegative_count(inserted)?,
                    matched: nonnegative_count(matched)?,
                    updated: nonnegative_count(updated)?,
                    conflicted: nonnegative_count(conflicted)?,
                    rejected: nonnegative_count(rejected)?,
                    unmapped: nonnegative_count(unmapped)?,
                },
                reused: true,
            });
        }
        let run_id: uuid::Uuid = sqlx::query_scalar(
            "insert into x_archive.legacy_import_runs \
                 (account_id, source_kind, source_version, source_digest, \
                  importer_parser_version, ownership_approval_digest) \
             values ($1, $2, $3, $4, $5, $6) returning id",
        )
        .bind(approval.account_id)
        .bind(source_kind)
        .bind(source_version)
        .bind(&batch.source_digest)
        .bind(IMPORTER_PARSER_VERSION)
        .bind(&approval.approval_digest)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| LegacyTransitionError::Database)?;

        let counts = persist_import_records(&mut transaction, run_id, &batch.records).await?;

        sqlx::query(
            "update x_archive.legacy_import_runs set status = 'completed', \
                 inserted_count = $2, matched_count = $3, updated_count = $4, \
                 conflicted_count = $5, rejected_count = $6, unmapped_count = $7, \
                 finished_at = now() where id = $1",
        )
        .bind(run_id)
        .bind(bounded_count(counts.inserted)?)
        .bind(bounded_count(counts.matched)?)
        .bind(bounded_count(counts.updated)?)
        .bind(bounded_count(counts.conflicted)?)
        .bind(bounded_count(counts.rejected)?)
        .bind(bounded_count(counts.unmapped)?)
        .execute(&mut *transaction)
        .await
        .map_err(|_| LegacyTransitionError::Database)?;
        transaction
            .commit()
            .await
            .map_err(|_| LegacyTransitionError::Database)?;
        Ok(ImportOutcome {
            run_id,
            counts,
            reused: false,
        })
    }

    /// Compares immutable legacy evidence with one complete current official snapshot.
    ///
    /// # Errors
    /// Refuses non-authoritative or cross-account inputs before persisting a report.
    pub async fn shadow_report_for(
        &self,
        account_id: uuid::Uuid,
        import_run_id: uuid::Uuid,
        snapshot_id: uuid::Uuid,
    ) -> Result<ShadowReportOutcome, LegacyTransitionError> {
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(|_| LegacyTransitionError::Database)?;
        let import_available: bool = sqlx::query_scalar(
            "select exists (select 1 from x_archive.legacy_import_runs \
             where id = $1 and account_id = $2 and status = 'completed')",
        )
        .bind(import_run_id)
        .bind(account_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| LegacyTransitionError::Database)?;
        if !import_available {
            return Err(LegacyTransitionError::ImportRunUnavailable);
        }
        require_current_authoritative_snapshot(&mut transaction, account_id, snapshot_id).await?;

        let (legacy_rows, official_rows) =
            load_shadow_rows(&mut transaction, import_run_id, snapshot_id).await?;
        let report = build_shadow_report(
            account_id,
            import_run_id,
            snapshot_id,
            legacy_rows,
            official_rows,
        )?;
        let (report_id, report_digest, reused) =
            persist_shadow_report(&mut transaction, &report).await?;
        transaction
            .commit()
            .await
            .map_err(|_| LegacyTransitionError::Database)?;
        Ok(ShadowReportOutcome {
            report_id,
            report_digest,
            report,
            reused,
        })
    }

    /// Generates and hashes the deterministic owner checklist for one report.
    ///
    /// # Errors
    /// Refuses missing or cross-account report evidence.
    pub async fn checklist_for(
        &self,
        account_id: uuid::Uuid,
        shadow_report_id: uuid::Uuid,
    ) -> Result<ChecklistOutcome, LegacyTransitionError> {
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(|_| LegacyTransitionError::Database)?;
        let row = sqlx::query(
            "select report.report_digest, report.import_digest, report.snapshot_digest, \
                    report.report_payload, report.import_run_id, report.snapshot_id, \
                    run.inserted_count, run.matched_count, run.updated_count, \
                    run.conflicted_count, run.rejected_count, run.unmapped_count \
             from x_archive.legacy_shadow_reports report \
             join x_archive.legacy_import_runs run on run.id = report.import_run_id \
             where report.id = $1 and report.account_id = $2 and run.account_id = $2",
        )
        .bind(shadow_report_id)
        .bind(account_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| LegacyTransitionError::Database)?
        .ok_or(LegacyTransitionError::ShadowReportUnavailable)?;
        let report_digest: String = row
            .try_get("report_digest")
            .map_err(|_| LegacyTransitionError::Database)?;
        let import_digest: String = row
            .try_get("import_digest")
            .map_err(|_| LegacyTransitionError::Database)?;
        let snapshot_digest: String = row
            .try_get("snapshot_digest")
            .map_err(|_| LegacyTransitionError::Database)?;
        let report_payload: Value = row
            .try_get("report_payload")
            .map_err(|_| LegacyTransitionError::Database)?;
        let import_run_id: uuid::Uuid = row
            .try_get("import_run_id")
            .map_err(|_| LegacyTransitionError::Database)?;
        let snapshot_id: uuid::Uuid = row
            .try_get("snapshot_id")
            .map_err(|_| LegacyTransitionError::Database)?;
        require_current_authoritative_snapshot(&mut transaction, account_id, snapshot_id).await?;
        let inserted: i32 = row
            .try_get("inserted_count")
            .map_err(|_| LegacyTransitionError::Database)?;
        let matched: i32 = row
            .try_get("matched_count")
            .map_err(|_| LegacyTransitionError::Database)?;
        let updated: i32 = row
            .try_get("updated_count")
            .map_err(|_| LegacyTransitionError::Database)?;
        let conflicted: i32 = row
            .try_get("conflicted_count")
            .map_err(|_| LegacyTransitionError::Database)?;
        let rejected: i32 = row
            .try_get("rejected_count")
            .map_err(|_| LegacyTransitionError::Database)?;
        let unmapped: i32 = row
            .try_get("unmapped_count")
            .map_err(|_| LegacyTransitionError::Database)?;

        supersede_approvals_for_new_report(&mut transaction, account_id, shadow_report_id).await?;

        let summary = report_payload
            .get("summary")
            .and_then(Value::as_object)
            .ok_or(LegacyTransitionError::Database)?;
        let markdown = format!(
            "# Field Theory cutover checklist\n\n\
             Account: `{account_id}`  \n\
             Import run: `{import_run_id}`  \n\
             Shadow report: `{shadow_report_id}`  \n\
             Complete snapshot: `{snapshot_id}`\n\n\
             ## Backup\n\n- [ ] Confirm the read-only legacy archive backup and PostgreSQL backup are retained.\n\n\
             ## Import counts\n\n- Inserted: {inserted}\n- Matched: {matched}\n- Updated: {updated}\n- Conflicted: {conflicted}\n- Rejected: {rejected}\n- Unmapped: {unmapped}\n- Import digest: `{import_digest}`\n\n\
             ## Complete official snapshot\n\n- [ ] Confirm snapshot `{snapshot_id}` remains the current complete successful full snapshot.\n- Snapshot digest: `{snapshot_digest}`\n\n\
             ## Shadow findings\n\n- Provider-ID matched: {}\n- URL matched: {}\n- Legacy only: {}\n- Official only: {}\n- Identity conflicts: {}\n- Unmapped: {}\n- Report digest: `{report_digest}`\n\n\
             ## Privacy and cost review\n\n- [ ] Confirm no cookies, tokens, sessions, source paths, post bodies, URLs, or handles entered evidence records.\n- [ ] Confirm the official full snapshot cost and rate-limit evidence are accepted.\n\n\
             ## Workspace changeset\n\n- [ ] Approve a separate `ratatoskr-workspace` changeset before any external routing change.\n\n\
             ## Stability window\n\n- [ ] Record the owner-approved shadow stability window and monitoring owner.\n\n\
             ## Rollback\n\n1. Stop the external routing rollout in the workspace changeset.\n2. Restore the previous routing revision; do not delete imported, report, or approval evidence.\n3. Re-run `shadow-report` against the current complete official snapshot before another approval.\n\n\
             ## Evidence retention\n\n- [ ] Retain the legacy source unchanged, import run, shadow JSON, this checklist, owner decision evidence, and rollback record.\n",
            report_summary_count(summary, "matched")?,
            report_summary_count(summary, "url_matched")?,
            report_summary_count(summary, "legacy_only")?,
            report_summary_count(summary, "official_only")?,
            report_summary_count(summary, "identity_conflicts")?,
            report_summary_count(summary, "unmapped")?,
        );
        let checklist_digest = sha256(markdown.as_bytes());
        transaction
            .commit()
            .await
            .map_err(|_| LegacyTransitionError::Database)?;
        Ok(ChecklistOutcome {
            markdown,
            checklist_digest,
            shadow_report_id,
        })
    }

    /// Records one owner decision for the exact current report/checklist evidence.
    ///
    /// # Errors
    /// Refuses stale digests, mismatched ownership, or unavailable reports.
    pub async fn record_transition_approval(
        &self,
        request: &TransitionApprovalRequest,
    ) -> Result<TransitionApprovalOutcome, LegacyTransitionError> {
        if !valid_sha256(&request.owner_evidence_digest) || !valid_sha256(&request.checklist_digest)
        {
            return Err(LegacyTransitionError::ChecklistDigestMismatch);
        }
        let checklist = self
            .checklist_for(request.account_id, request.shadow_report_id)
            .await?;
        if checklist.checklist_digest != request.checklist_digest {
            return Err(LegacyTransitionError::ChecklistDigestMismatch);
        }
        let mut transaction = self
            .database
            .pool()
            .begin()
            .await
            .map_err(|_| LegacyTransitionError::Database)?;
        if let Some(approval_id) = validate_approval_target(&mut transaction, request).await? {
            transaction
                .commit()
                .await
                .map_err(|_| LegacyTransitionError::Database)?;
            return Ok(TransitionApprovalOutcome {
                approval_id,
                reused: true,
            });
        }
        let decision = match request.decision {
            ApprovalDecision::Approved => "approved",
            ApprovalDecision::Rejected => "rejected",
        };
        supersede_current_approvals(&mut transaction, request.account_id, request.decided_at)
            .await?;
        let approval_id: uuid::Uuid = sqlx::query_scalar(
            "insert into x_archive.legacy_transition_approvals \
                 (account_id, internal_owner_id, shadow_report_id, checklist_digest, \
                  owner_evidence_digest, decision, decided_at) \
             values ($1, $2, $3, $4, $5, $6, $7) returning id",
        )
        .bind(request.account_id)
        .bind(request.internal_owner_id)
        .bind(request.shadow_report_id)
        .bind(&request.checklist_digest)
        .bind(&request.owner_evidence_digest)
        .bind(decision)
        .bind(request.decided_at)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| LegacyTransitionError::Database)?;
        let review_state = match request.decision {
            ApprovalDecision::Approved => "reviewable",
            ApprovalDecision::Rejected => "owner_review_required",
        };
        sqlx::query(
            "update x_archive.legacy_shadow_reports set review_state = $2 \
             where id = $1 and account_id = $3",
        )
        .bind(request.shadow_report_id)
        .bind(review_state)
        .bind(request.account_id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| LegacyTransitionError::Database)?;
        transaction
            .commit()
            .await
            .map_err(|_| LegacyTransitionError::Database)?;
        Ok(TransitionApprovalOutcome {
            approval_id,
            reused: false,
        })
    }
}

async fn require_current_authoritative_snapshot(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: uuid::Uuid,
    snapshot_id: uuid::Uuid,
) -> Result<(), LegacyTransitionError> {
    let authoritative_snapshot: Option<uuid::Uuid> = sqlx::query_scalar(
        "select authority.snapshot_id \
         from x_archive.bookmark_snapshot_authority authority \
         join x_archive.snapshots snapshot on snapshot.id = authority.snapshot_id \
         join x_archive.sync_runs run on run.id = snapshot.sync_run_id \
         where authority.account_id = $1 and authority.snapshot_id = $2 \
           and run.account_id = $1 and run.run_type = 'full' and run.state = 'completed' \
           and run.finished_at is not null and run.checkpoint is null and snapshot.complete \
           and snapshot.completed_at is not null and snapshot.page_count = run.pages_fetched \
         for share of authority, snapshot, run",
    )
    .bind(account_id)
    .bind(snapshot_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| LegacyTransitionError::Database)?;
    authoritative_snapshot
        .map(drop)
        .ok_or(LegacyTransitionError::SnapshotNotAuthoritative)
}

async fn supersede_approvals_for_new_report(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: uuid::Uuid,
    shadow_report_id: uuid::Uuid,
) -> Result<(), LegacyTransitionError> {
    let stale_reports: Vec<uuid::Uuid> = sqlx::query_scalar(
        "select shadow_report_id from x_archive.legacy_transition_approvals \
         where account_id = $1 and superseded_at is null and shadow_report_id <> $2 for update",
    )
    .bind(account_id)
    .bind(shadow_report_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| LegacyTransitionError::Database)?;
    if stale_reports.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "update x_archive.legacy_transition_approvals set superseded_at = now() \
         where account_id = $1 and superseded_at is null and shadow_report_id <> $2",
    )
    .bind(account_id)
    .bind(shadow_report_id)
    .execute(&mut **transaction)
    .await
    .map_err(|_| LegacyTransitionError::Database)?;
    reset_report_reviewability(transaction, account_id, &stale_reports).await
}

async fn validate_approval_target(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request: &TransitionApprovalRequest,
) -> Result<Option<uuid::Uuid>, LegacyTransitionError> {
    let owner: Option<uuid::Uuid> = sqlx::query_scalar(
        "select internal_user_id from x_archive.accounts where id = $1 for update",
    )
    .bind(request.account_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| LegacyTransitionError::Database)?;
    if owner != Some(request.internal_owner_id) {
        return Err(LegacyTransitionError::InvalidOwnershipApproval);
    }
    let report_snapshot_id: Option<uuid::Uuid> = sqlx::query_scalar(
        "select snapshot_id from x_archive.legacy_shadow_reports \
         where id = $1 and account_id = $2 for update",
    )
    .bind(request.shadow_report_id)
    .bind(request.account_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| LegacyTransitionError::Database)?;
    let report_snapshot_id =
        report_snapshot_id.ok_or(LegacyTransitionError::ShadowReportUnavailable)?;
    require_current_authoritative_snapshot(transaction, request.account_id, report_snapshot_id)
        .await?;
    let decision = match request.decision {
        ApprovalDecision::Approved => "approved",
        ApprovalDecision::Rejected => "rejected",
    };
    let existing: Option<(uuid::Uuid, Option<DateTime<Utc>>)> = sqlx::query_as(
        "select id, superseded_at from x_archive.legacy_transition_approvals \
         where account_id = $1 and shadow_report_id = $2 and checklist_digest = $3 \
           and owner_evidence_digest = $4 and decision = $5",
    )
    .bind(request.account_id)
    .bind(request.shadow_report_id)
    .bind(&request.checklist_digest)
    .bind(&request.owner_evidence_digest)
    .bind(decision)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| LegacyTransitionError::Database)?;
    match existing {
        Some((_, Some(_))) => Err(LegacyTransitionError::ChecklistDigestMismatch),
        Some((approval_id, None)) => Ok(Some(approval_id)),
        None => Ok(None),
    }
}

async fn supersede_current_approvals(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: uuid::Uuid,
    decided_at: DateTime<Utc>,
) -> Result<(), LegacyTransitionError> {
    let stale_reports: Vec<uuid::Uuid> = sqlx::query_scalar(
        "select shadow_report_id from x_archive.legacy_transition_approvals \
         where account_id = $1 and superseded_at is null for update",
    )
    .bind(account_id)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| LegacyTransitionError::Database)?;
    if stale_reports.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "update x_archive.legacy_transition_approvals set superseded_at = $2 \
         where account_id = $1 and superseded_at is null",
    )
    .bind(account_id)
    .bind(decided_at)
    .execute(&mut **transaction)
    .await
    .map_err(|_| LegacyTransitionError::Database)?;
    reset_report_reviewability(transaction, account_id, &stale_reports).await
}

async fn reset_report_reviewability(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: uuid::Uuid,
    stale_reports: &[uuid::Uuid],
) -> Result<(), LegacyTransitionError> {
    sqlx::query(
        "update x_archive.legacy_shadow_reports set review_state = 'owner_review_required' \
         where account_id = $1 and id = any($2)",
    )
    .bind(account_id)
    .bind(stale_reports)
    .execute(&mut **transaction)
    .await
    .map_err(|_| LegacyTransitionError::Database)?;
    Ok(())
}
