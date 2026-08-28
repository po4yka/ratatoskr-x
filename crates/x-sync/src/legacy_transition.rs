//! Credential-free legacy import and shadow-transition application boundary.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::{Connection as _, Row as _};
use x_oauth::cipher::{Purpose, TokenCipher};
use x_oauth::payload::CredentialPayload;
use x_persistence::database::Database;

/// Parser stamp owned by the Ratatoskr legacy importer.
pub const IMPORTER_PARSER_VERSION: i32 = 1;

/// The closed legacy source family accepted by the transition workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacySourceKind {
    /// Retired-monolith `x_bookmark_metadata` CSV.
    MonolithBookmarkMetadataCsv,
    /// Field Theory raw bookmark cache.
    FieldTheoryJsonl,
    /// Field Theory derived search index.
    FieldTheorySqlite,
}

/// The exact supported source schema version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacySourceVersion {
    /// The retired-monolith nine-column export.
    MonolithBookmarkMetadata,
    /// Field Theory JSONL schema version 1.
    FieldTheoryJsonl1,
    /// Field Theory `SQLite` schema version 6.
    FieldTheorySqlite6,
}

/// One legacy row's normalized identity outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyResolution {
    /// Stable provider post identity was mapped.
    ProviderPost,
    /// A canonical URL supplied the only unambiguous secondary match.
    UrlDerivedPost,
    /// Explicit and URL-derived provider identities disagree.
    IdentityConflict,
    /// No stable post identity could be resolved.
    Unmapped,
}

/// Deterministic terminal counts for one import attempt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportCounts {
    /// Newly inserted normalized records.
    pub inserted: u64,
    /// Rows matched to existing records without updating them.
    pub matched: u64,
    /// Existing normalized records updated by accepted legacy evidence.
    pub updated: u64,
    /// Rows retained as identity conflicts.
    pub conflicted: u64,
    /// Rows rejected by the selected source contract.
    pub rejected: u64,
    /// Rows retained without stable post identity.
    pub unmapped: u64,
}

/// Explicit owner mapping bound to one target account and preflight source digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnershipApproval {
    /// Existing Ratatoskr X account selected by the owner.
    pub account_id: uuid::Uuid,
    /// Internal owner already bound to that account.
    pub internal_owner_id: uuid::Uuid,
    /// Exact selected source artifact digest.
    pub source_digest: String,
    /// Digest of the reviewed owner mapping evidence.
    pub approval_digest: String,
}

/// Durable result of one accepted or idempotently reused import.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportOutcome {
    /// Persisted import run identity.
    pub run_id: uuid::Uuid,
    /// Deterministic terminal counts.
    pub counts: ImportCounts,
    /// Whether a prior identical completed run was reused without writes.
    pub reused: bool,
}

/// Closed shadow comparison classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShadowDiffClass {
    /// Stable provider identity appears in both imported and official evidence.
    Matched,
    /// A provider identity parsed from a canonical status URL appears in official evidence.
    UrlMatched,
    /// Stable imported identity is absent from the complete official snapshot.
    LegacyOnly,
    /// Official snapshot identity has no imported counterpart.
    OfficialOnly,
    /// Legacy explicit and URL-derived provider identities disagree.
    IdentityConflict,
    /// Legacy evidence has no stable provider identity.
    Unmapped,
}

/// One redacted, deterministic shadow difference entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowDiff {
    /// Difference classification.
    pub class: ShadowDiffClass,
    /// Provider post ID when stable; no URL, body, or mutable handle is exposed.
    pub provider_post_id: Option<String>,
    /// Legacy row digest used as a non-content reference.
    pub legacy_record_digest: Option<String>,
    /// Whether official normalized content differs from the imported digest.
    pub content_changed: bool,
    /// Whether the legacy row retained category metadata.
    pub legacy_category_present: bool,
    /// Whether the legacy row retained folder metadata.
    pub legacy_folder_present: bool,
}

/// Deterministic counts for one shadow report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowSummary {
    /// Matched entries.
    pub matched: u64,
    /// Matches resolved through the secondary canonical status URL hint.
    pub url_matched: u64,
    /// Legacy-only entries.
    pub legacy_only: u64,
    /// Official-only entries.
    pub official_only: u64,
    /// Identity conflicts.
    pub identity_conflicts: u64,
    /// Unmapped legacy entries.
    pub unmapped: u64,
}

/// Canonical redacted report bound to one import and one authoritative snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowReport {
    /// First and only report schema version during development.
    pub report_version: i32,
    /// Explicit account scope.
    pub account_id: uuid::Uuid,
    /// Completed import evidence compared.
    pub import_run_id: uuid::Uuid,
    /// Complete successful official snapshot compared.
    pub snapshot_id: uuid::Uuid,
    /// Digest of sorted imported evidence.
    pub import_digest: String,
    /// Digest of sorted official snapshot evidence.
    pub snapshot_digest: String,
    /// Deterministically ordered redacted entries.
    pub entries: Vec<ShadowDiff>,
    /// Deterministic classification counts.
    pub summary: ShadowSummary,
}

/// Persisted result of a shadow comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowReportOutcome {
    /// Persisted report identity.
    pub report_id: uuid::Uuid,
    /// SHA-256 of canonical report JSON.
    pub report_digest: String,
    /// Canonical report payload.
    pub report: ShadowReport,
    /// Whether identical persisted evidence was reused.
    pub reused: bool,
}

/// Deterministic owner checklist bound to one persisted shadow report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChecklistOutcome {
    /// Exact Markdown reviewed by the owner.
    pub markdown: String,
    /// SHA-256 of the exact Markdown bytes.
    pub checklist_digest: String,
    /// Shadow report evidence bound into the checklist.
    pub shadow_report_id: uuid::Uuid,
}

/// Closed owner decision vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Owner approved cutover prerequisites for these exact evidence digests.
    Approved,
    /// Owner rejected cutover for these exact evidence digests.
    Rejected,
}

/// Exact owner decision input, containing digests rather than private evidence bodies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionApprovalRequest {
    /// Explicit account scope.
    pub account_id: uuid::Uuid,
    /// Internal owner bound to the account.
    pub internal_owner_id: uuid::Uuid,
    /// Persisted shadow report reviewed by the owner.
    pub shadow_report_id: uuid::Uuid,
    /// Digest of the exact generated checklist.
    pub checklist_digest: String,
    /// Digest of retained owner decision evidence.
    pub owner_evidence_digest: String,
    /// Explicit owner decision.
    pub decision: ApprovalDecision,
    /// Time of the explicit decision.
    pub decided_at: DateTime<Utc>,
}

/// Durable result of recording one owner decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionApprovalOutcome {
    /// Persisted approval identity.
    pub approval_id: uuid::Uuid,
    /// Whether the exact decision already existed.
    pub reused: bool,
}

/// Bounded resource limits applied before a source can reach `PostgreSQL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceLimits {
    /// Maximum bytes read from one selected artifact.
    pub max_bytes: u64,
    /// Maximum records materialized by one preflight.
    pub max_rows: usize,
    /// Maximum UTF-8 bytes accepted in any one text field.
    pub max_text_bytes: usize,
    /// Maximum nested JSON containers in an input value.
    pub max_json_depth: usize,
}

impl Default for SourceLimits {
    fn default() -> Self {
        Self {
            max_bytes: 64 * 1024 * 1024,
            max_rows: 100_000,
            max_text_bytes: 1_000_000,
            max_json_depth: 32,
        }
    }
}

/// Explicit allow-listed source selection; no neighboring file is discovered implicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacySourceSelection {
    /// Retired-monolith bookmark metadata CSV.
    MonolithCsv(PathBuf),
    /// Field Theory raw cache, optionally paired with its derived index.
    FieldTheory {
        /// Explicit `bookmarks.jsonl` path; preferred when present.
        jsonl: Option<PathBuf>,
        /// Explicit `bookmarks.db` path; used only when JSONL is absent.
        sqlite: Option<PathBuf>,
    },
}

impl LegacySourceSelection {
    /// Selects one retired-monolith CSV artifact.
    #[must_use]
    pub fn monolith_csv(path: impl AsRef<Path>) -> Self {
        Self::MonolithCsv(path.as_ref().to_path_buf())
    }

    /// Selects explicit Field Theory artifacts without scanning their directory.
    #[must_use]
    pub fn field_theory(jsonl: Option<PathBuf>, sqlite: Option<PathBuf>) -> Self {
        Self::FieldTheory { jsonl, sqlite }
    }
}

/// A validated, bounded source batch safe to hand to the import transaction.
#[derive(Clone)]
pub struct PreflightBatch {
    source_kind: LegacySourceKind,
    source_version: LegacySourceVersion,
    source_digest: String,
    row_count: usize,
    records: Vec<LegacyRecord>,
}

impl std::fmt::Debug for PreflightBatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreflightBatch")
            .field("source_kind", &self.source_kind)
            .field("source_version", &self.source_version)
            .field("source_digest", &self.source_digest)
            .field("row_count", &self.row_count)
            .finish_non_exhaustive()
    }
}

/// One validated legacy observation. Source text and mutable handles never become identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyRecord {
    source_record_key: String,
    provider_post_id: Option<String>,
    url_provider_post_id: Option<String>,
    canonical_url_digest: Option<String>,
    text: String,
    posted_at: Option<DateTime<Utc>>,
    bookmarked_at: Option<DateTime<Utc>>,
    synced_at: DateTime<Utc>,
    categories: Vec<String>,
    folder_ids: Vec<String>,
    folder_names: Vec<String>,
    record_digest: String,
}

struct LegacyRecordInput {
    source_record_key: String,
    provider_post_id: Option<String>,
    url_provider_post_id: Option<String>,
    canonical_url_digest: Option<String>,
    text: String,
    posted_at: Option<DateTime<Utc>>,
    bookmarked_at: Option<DateTime<Utc>>,
    synced_at: DateTime<Utc>,
    categories: Vec<String>,
    folder_ids: Vec<String>,
    folder_names: Vec<String>,
}

impl LegacyRecord {
    /// Stable key from the selected source row.
    #[must_use]
    pub fn source_record_key(&self) -> &str {
        &self.source_record_key
    }

    /// Explicit provider post ID supplied by the legacy source.
    #[must_use]
    pub fn provider_post_id(&self) -> Option<&str> {
        self.provider_post_id.as_deref()
    }

    /// Provider post ID parsed only from an allow-listed X status URL.
    #[must_use]
    pub fn url_provider_post_id(&self) -> Option<&str> {
        self.url_provider_post_id.as_deref()
    }

    /// SHA-256 of the normalized legacy post text.
    #[must_use]
    pub fn content_digest(&self) -> String {
        sha256(self.text.as_bytes())
    }

    /// Legacy publication observation, when syntactically valid and present.
    #[must_use]
    pub fn posted_at(&self) -> Option<DateTime<Utc>> {
        self.posted_at
    }

    /// Legacy bookmark observation; never an authoritative native save timestamp.
    #[must_use]
    pub fn bookmarked_at(&self) -> Option<DateTime<Utc>> {
        self.bookmarked_at
    }

    /// Time at which the legacy cache reported synchronizing the row.
    #[must_use]
    pub fn synced_at(&self) -> DateTime<Utc> {
        self.synced_at
    }

    /// Legacy categories retained as metadata, not native X folders.
    #[must_use]
    pub fn categories(&self) -> &[String] {
        &self.categories
    }

    /// Legacy Field Theory folder IDs retained as source metadata only.
    #[must_use]
    pub fn folder_ids(&self) -> &[String] {
        &self.folder_ids
    }

    /// Legacy Field Theory folder names retained as source metadata only.
    #[must_use]
    pub fn folder_names(&self) -> &[String] {
        &self.folder_names
    }

    /// Digest of the validated row projection used for restart safety.
    #[must_use]
    pub fn record_digest(&self) -> &str {
        &self.record_digest
    }
}

impl PreflightBatch {
    /// The allow-listed source family selected after precedence rules.
    #[must_use]
    pub fn source_kind(&self) -> LegacySourceKind {
        self.source_kind
    }

    /// The exact validated source schema version.
    #[must_use]
    pub fn source_version(&self) -> LegacySourceVersion {
        self.source_version
    }

    /// Lowercase SHA-256 digest of the selected artifact bytes.
    #[must_use]
    pub fn source_digest(&self) -> &str {
        &self.source_digest
    }

    /// Number of fully validated source records.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.row_count
    }

    /// Fully validated records supplied to the account-bound import transaction.
    #[must_use]
    pub fn records(&self) -> &[LegacyRecord] {
        &self.records
    }
}

/// Resolves the provider user currently authenticated by one Ratatoskr account.
pub trait CurrentAccountIdentity: Send + Sync {
    /// Returns the current OAuth user-context provider identity.
    fn provider_user_id<'a>(
        &'a self,
        account_id: uuid::Uuid,
    ) -> Pin<Box<dyn Future<Output = Result<String, LegacyTransitionError>> + Send + 'a>>;
}

/// Official `/2/users/me` adapter using only the selected account's current encrypted credential.
#[derive(Clone)]
pub struct OfficialCurrentAccountIdentity {
    database: Database,
    cipher: TokenCipher,
    base_url: String,
    http: Option<reqwest::Client>,
}

impl std::fmt::Debug for OfficialCurrentAccountIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OfficialCurrentAccountIdentity")
            .field("http_ready", &self.http.is_some())
            .finish_non_exhaustive()
    }
}

impl OfficialCurrentAccountIdentity {
    /// Builds the bounded official identity adapter.
    #[must_use]
    pub fn new(database: Database, cipher: TokenCipher, base_url: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .retry(reqwest::retry::never())
            .build()
            .ok();
        Self {
            database,
            cipher,
            base_url: base_url.into().trim_end_matches('/').to_owned(),
            http,
        }
    }

    async fn current_provider_user_id(
        &self,
        account_id: uuid::Uuid,
    ) -> Result<String, LegacyTransitionError> {
        let credential: Option<(Vec<u8>, Vec<String>)> = sqlx::query_as(
            "select credential.encrypted_payload, credential.granted_scopes \
             from x_archive.accounts account \
             join x_archive.credentials credential \
               on credential.account_id = account.id and credential.status = 'active' \
             where account.id = $1 and account.state = 'connected' \
             order by credential.created_at desc, credential.id desc limit 1",
        )
        .bind(account_id)
        .fetch_optional(self.database.pool())
        .await
        .map_err(|_| LegacyTransitionError::CurrentIdentityUnavailable)?;
        let Some((encrypted, scopes)) = credential else {
            return Err(LegacyTransitionError::CurrentIdentityUnavailable);
        };
        if !scopes.iter().any(|scope| scope == "users.read") {
            return Err(LegacyTransitionError::CurrentIdentityUnavailable);
        }
        let plaintext = self
            .cipher
            .open(account_id, Purpose::Credential, &encrypted)
            .map_err(|_| LegacyTransitionError::CurrentIdentityUnavailable)?;
        let credential = CredentialPayload::decode(&plaintext)
            .map_err(|_| LegacyTransitionError::CurrentIdentityUnavailable)?;
        let http = self
            .http
            .as_ref()
            .ok_or(LegacyTransitionError::CurrentIdentityUnavailable)?;
        let mut response = http
            .get(format!("{}/2/users/me", self.base_url))
            .bearer_auth(credential.access_token)
            .send()
            .await
            .map_err(|_| LegacyTransitionError::CurrentIdentityUnavailable)?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|length| length > 64 * 1024)
        {
            return Err(LegacyTransitionError::CurrentIdentityUnavailable);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| LegacyTransitionError::CurrentIdentityUnavailable)?
        {
            if body.len().saturating_add(chunk.len()) > 64 * 1024 {
                return Err(LegacyTransitionError::CurrentIdentityUnavailable);
            }
            body.extend_from_slice(&chunk);
        }
        let provider_user_id = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|value| {
                value
                    .pointer("/data/id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .filter(|provider_user_id| !provider_user_id.is_empty());
        provider_user_id.ok_or(LegacyTransitionError::CurrentIdentityUnavailable)
    }
}

impl CurrentAccountIdentity for OfficialCurrentAccountIdentity {
    fn provider_user_id<'a>(
        &'a self,
        account_id: uuid::Uuid,
    ) -> Pin<Box<dyn Future<Output = Result<String, LegacyTransitionError>> + Send + 'a>> {
        Box::pin(self.current_provider_user_id(account_id))
    }
}

/// Failures exposed by the operator-only transition workflow.
#[derive(Debug, thiserror::Error)]
pub enum LegacyTransitionError {
    /// The behavior seam exists so RED tests compile, but no operation is implemented yet.
    #[error("the legacy transition operation is not implemented")]
    NotImplemented,
    /// No allow-listed source artifact was selected.
    #[error("no supported legacy source artifact was selected")]
    MissingSource,
    /// A selected artifact could not be read without exposing its local path.
    #[error("the selected legacy source is unavailable")]
    SourceUnavailable,
    /// The selected artifact exceeds the configured byte budget.
    #[error("the selected legacy source exceeds the byte limit")]
    SourceTooLarge,
    /// The selected source contains more records than the configured bound.
    #[error("the selected legacy source exceeds the row limit")]
    RowLimitExceeded,
    /// A text field exceeds the configured UTF-8 byte limit.
    #[error("legacy source field `{field}` exceeds the text limit")]
    TextLimitExceeded {
        /// Public contract field name; never its value.
        field: &'static str,
    },
    /// A JSON value exceeds the configured nesting bound.
    #[error("legacy source JSON exceeds the nesting limit")]
    JsonDepthExceeded,
    /// Input bytes are not valid UTF-8 or do not conform to their pinned encoding.
    #[error("the selected legacy source encoding is invalid")]
    InvalidEncoding,
    /// A timestamp does not satisfy the pinned RFC 3339 contract.
    #[error("legacy source field `{field}` is not a valid RFC 3339 timestamp")]
    InvalidTimestamp {
        /// Public contract field name; never its value.
        field: &'static str,
    },
    /// An input field is outside the pinned public source contract.
    #[error("legacy source field `{field}` is not supported")]
    UnsupportedField {
        /// Rejected field name; never its value.
        field: String,
    },
    /// A credential/session-shaped field is rejected before any target write.
    #[error("legacy source credential/session field `{field}` is forbidden")]
    ForbiddenField {
        /// Rejected credential/session-shaped field name; never its value.
        field: String,
    },
    /// Source schema metadata does not identify the one supported version.
    #[error("the selected legacy source schema version is unsupported")]
    UnsupportedSourceVersion,
    /// Exact `SQLite` table shape differs from the pinned Field Theory projection.
    #[error("the selected Field Theory SQLite schema is unsupported")]
    UnsupportedSqliteSchema,
    /// A row is missing a required public-source field.
    #[error("legacy source field `{field}` is missing or invalid")]
    InvalidField {
        /// Public contract field name; never its value.
        field: &'static str,
    },
    /// The same source record key carried different evidence.
    #[error("legacy source record key is duplicated with conflicting evidence")]
    DuplicateSourceKey,
    /// The source changed during validation.
    #[error("the selected legacy source changed during preflight")]
    SourceChanged,
    /// `SQLite` could not be opened or queried through the immutable connection.
    #[error("the selected Field Theory SQLite source could not be validated")]
    SourceDatabase,
    /// Import execution requires explicit owner mapping evidence.
    #[error("explicit owner mapping approval is required")]
    MissingOwnershipApproval,
    /// Owner mapping evidence is not bound to the selected source.
    #[error("owner mapping approval does not match the selected source")]
    InvalidOwnershipApproval,
    /// The explicitly selected target account does not exist.
    #[error("the selected target account does not exist")]
    TargetAccountNotFound,
    /// The selected target account is not currently connected.
    #[error("the selected target account is not connected")]
    TargetAccountNotConnected,
    /// Current official OAuth identity could not be resolved.
    #[error("current official OAuth account identity is unavailable")]
    CurrentIdentityUnavailable,
    /// The current official OAuth identity differs from the selected target account.
    #[error("current official OAuth identity does not match the selected target account")]
    CurrentIdentityMismatch,
    /// `PostgreSQL` rejected or could not atomically complete the operation.
    #[error("the legacy transition database operation failed")]
    Database,
    /// Completed import evidence is absent or belongs to another account.
    #[error("the selected completed legacy import is unavailable for this account")]
    ImportRunUnavailable,
    /// Snapshot is incomplete, unsuccessful, non-full, stale, or not current authority.
    #[error("the selected official snapshot has no complete absence authority")]
    SnapshotNotAuthoritative,
    /// Shadow report is absent or belongs to another account.
    #[error("the selected shadow report is unavailable for this account")]
    ShadowReportUnavailable,
    /// Approval is not bound to the current deterministic checklist.
    #[error("the owner approval checklist digest is stale or invalid")]
    ChecklistDigestMismatch,
}

/// Coordinates the transition workflow over the owned database.
#[derive(Debug, Clone)]
pub struct LegacyTransitionService {
    database: Database,
}

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

type ExistingImportRow = (uuid::Uuid, String, i32, i32, i32, i32, i32, i32);

async fn validate_locked_import_account(
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

async fn persist_import_records(
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

async fn preflight_monolith_csv(
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

async fn preflight_field_theory_jsonl(
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

async fn preflight_field_theory_sqlite(
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

fn required_string<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
) -> Result<&'a str, LegacyTransitionError> {
    optional_string(object, field)?.ok_or(LegacyTransitionError::InvalidField { field })
}

fn optional_string<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
) -> Result<Option<&'a str>, LegacyTransitionError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value)),
        _ => Err(LegacyTransitionError::InvalidField { field }),
    }
}

fn optional_string_array(
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

fn validate_field_theory_record_shape(
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

fn validate_optional_string_value(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<(), LegacyTransitionError> {
    match object.get(field) {
        None | Some(Value::Null | Value::String(_)) => Ok(()),
        _ => Err(LegacyTransitionError::InvalidField { field }),
    }
}

fn validate_optional_bool_value(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<(), LegacyTransitionError> {
    match object.get(field) {
        None | Some(Value::Bool(_)) => Ok(()),
        _ => Err(LegacyTransitionError::InvalidField { field }),
    }
}

fn validate_optional_number_value(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<(), LegacyTransitionError> {
    match object.get(field) {
        None | Some(Value::Number(_)) => Ok(()),
        _ => Err(LegacyTransitionError::InvalidField { field }),
    }
}

fn validate_optional_object(
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

fn reject_unknown_fields(
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

fn validate_author_shape(object: &Map<String, Value>) -> Result<(), LegacyTransitionError> {
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

fn validate_engagement_shape(object: &Map<String, Value>) -> Result<(), LegacyTransitionError> {
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

fn validate_quoted_tweet_shape(object: &Map<String, Value>) -> Result<(), LegacyTransitionError> {
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

fn validate_media_objects(
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

fn validate_media_variants(
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

fn parse_optional_timestamp(
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

fn parse_required_timestamp(
    field: &'static str,
    value: &str,
) -> Result<DateTime<Utc>, LegacyTransitionError> {
    parse_optional_timestamp(field, value)?.ok_or(LegacyTransitionError::InvalidField { field })
}

fn optional_timestamp_from_json(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<Option<DateTime<Utc>>, LegacyTransitionError> {
    optional_string(object, field)?
        .map(|value| parse_required_timestamp(field, value))
        .transpose()
}

fn required_timestamp_from_json(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<DateTime<Utc>, LegacyTransitionError> {
    parse_required_timestamp(field, required_string(object, field)?)
}

fn validate_text(
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

fn reject_forbidden_fields(value: &Value) -> Result<(), LegacyTransitionError> {
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

fn forbidden_field_name(field: &str) -> bool {
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

fn json_depth(value: &Value) -> usize {
    match value {
        Value::Object(object) => 1 + object.values().map(json_depth).max().unwrap_or(0),
        Value::Array(values) => 1 + values.iter().map(json_depth).max().unwrap_or(0),
        _ => 0,
    }
}

fn parse_x_status_url(value: &str) -> (Option<String>, Option<String>) {
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

fn sqlite_string(
    row: &sqlx::sqlite::SqliteRow,
    field: &'static str,
) -> Result<String, LegacyTransitionError> {
    row.try_get::<String, _>(field)
        .map_err(|_| LegacyTransitionError::InvalidField { field })
}

fn sqlite_optional_string(
    row: &sqlx::sqlite::SqliteRow,
    field: &'static str,
) -> Result<Option<String>, LegacyTransitionError> {
    row.try_get::<Option<String>, _>(field)
        .map_err(|_| LegacyTransitionError::InvalidField { field })
}

fn parse_legacy_list(value: Option<&str>) -> Result<Vec<String>, LegacyTransitionError> {
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

fn merge_legacy_lists(
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

fn sort_unique(values: &mut Vec<String>) {
    values.sort();
    values.dedup();
}

fn nonempty(value: &str) -> Option<&str> {
    (!value.is_empty()).then_some(value)
}

fn sha256(bytes: &[u8]) -> String {
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

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
}

fn source_database_identity(
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

fn bounded_count(value: u64) -> Result<i32, LegacyTransitionError> {
    i32::try_from(value).map_err(|_| LegacyTransitionError::Database)
}

fn nonnegative_count(value: i32) -> Result<u64, LegacyTransitionError> {
    u64::try_from(value).map_err(|_| LegacyTransitionError::Database)
}

#[derive(Debug)]
struct ShadowLegacyRow {
    row_digest: String,
    provider_post_id: Option<String>,
    content_digest: Option<String>,
    category_metadata: Value,
    folder_metadata: Value,
    resolution: String,
}

async fn load_shadow_rows(
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

fn build_shadow_report(
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

fn classify_shadow_entries(
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

async fn persist_shadow_report(
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

fn json_array_has_values(value: &Value) -> bool {
    value.as_array().is_some_and(|values| !values.is_empty())
}

fn shadow_count(entries: &[ShadowDiff], class: ShadowDiffClass) -> u64 {
    entries
        .iter()
        .filter(|entry| entry.class == class)
        .fold(0_u64, |count, _| count.saturating_add(1))
}

fn report_summary_count(
    summary: &Map<String, Value>,
    field: &'static str,
) -> Result<u64, LegacyTransitionError> {
    summary
        .get(field)
        .and_then(Value::as_u64)
        .ok_or(LegacyTransitionError::Database)
}
