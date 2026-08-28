//! Credential-free legacy import and shadow-transition application boundary.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use x_oauth::cipher::{Purpose, TokenCipher};
use x_oauth::payload::CredentialPayload;
use x_persistence::database::Database;

mod imports;
mod service;
mod shadow;
mod source_validation;
mod sources;

use source_validation::sha256;

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
