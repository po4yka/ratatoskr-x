//! Local operator entry point for legacy import, shadow comparison, and approval evidence.

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ratatoskr_x::transition_cli::{
    SourceArgumentKind, TransitionCliError, TransitionInvocation, parse_arguments,
};
use x_oauth::cipher::TokenCipher;
use x_persistence::database::Database;
use x_sync::{
    ApprovalDecision, LegacySourceSelection, LegacyTransitionError, LegacyTransitionService,
    OfficialCurrentAccountIdentity, OwnershipApproval, SourceLimits, TransitionApprovalRequest,
};

const OFFICIAL_API_BASE_URL: &str = "https://api.x.com";
const USAGE: &str = "ratatoskr-x-transition <command> [arguments]\n\n\
Commands:\n\
  preflight       --source-kind <monolith-csv|field-theory-jsonl|field-theory-sqlite> --source <path>\n\
  import          --source-kind <kind> --source <path> --account-id <uuid> --internal-owner-id <uuid> --ownership-approval-digest <sha256>\n\
  shadow-report   --account-id <uuid> --import-run-id <uuid> --snapshot-id <uuid> [--output <path>]\n\
  checklist       --account-id <uuid> --shadow-report-id <uuid> [--output <path>]\n\
  record-approval --account-id <uuid> --internal-owner-id <uuid> --shadow-report-id <uuid> --checklist-digest <sha256> --owner-evidence-digest <sha256> --decision <approved|rejected>\n\n\
No command accepts cookies, tokens, credentials, or session material.\n";

#[derive(Debug, thiserror::Error)]
enum TransitionRunError {
    #[error(transparent)]
    Cli(#[from] TransitionCliError),
    #[error("transition service configuration is unavailable")]
    Configuration,
    #[error("transition database is unavailable")]
    Database,
    #[error("transition credential protection is unavailable")]
    Cipher,
    #[error(transparent)]
    Transition(#[from] LegacyTransitionError),
    #[error("transition output could not be written")]
    Output,
    #[error("transition output could not be encoded")]
    Encoding,
}

async fn run(arguments: Vec<OsString>) -> Result<(), TransitionRunError> {
    if is_help_request(&arguments) {
        write_stdout(USAGE.as_bytes())?;
        return Ok(());
    }
    let invocation = parse_arguments(arguments)?;
    match invocation {
        TransitionInvocation::Preflight {
            source_kind,
            source_path,
        } => handle_preflight(source_kind, source_path).await?,
        TransitionInvocation::Import {
            source_kind,
            source_path,
            account_id,
            internal_owner_id,
            ownership_approval_digest,
        } => {
            handle_import(
                source_kind,
                source_path,
                account_id,
                internal_owner_id,
                ownership_approval_digest,
            )
            .await?;
        }
        TransitionInvocation::ShadowReport {
            account_id,
            import_run_id,
            snapshot_id,
            output,
        } => {
            let database = load_database().await?;
            let outcome = LegacyTransitionService::new(database)
                .shadow_report_for(account_id, import_run_id, snapshot_id)
                .await?;
            let encoded = serde_json::to_vec_pretty(&outcome.report)
                .map_err(|_| TransitionRunError::Encoding)?;
            write_output(output.as_deref(), &encoded)?;
        }
        TransitionInvocation::Checklist {
            account_id,
            shadow_report_id,
            output,
        } => {
            let database = load_database().await?;
            let outcome = LegacyTransitionService::new(database)
                .checklist_for(account_id, shadow_report_id)
                .await?;
            write_output(output.as_deref(), outcome.markdown.as_bytes())?;
        }
        TransitionInvocation::RecordApproval {
            account_id,
            internal_owner_id,
            shadow_report_id,
            checklist_digest,
            owner_evidence_digest,
            decision,
        } => {
            let database = load_database().await?;
            let decision = match decision.as_str() {
                "approved" => ApprovalDecision::Approved,
                "rejected" => ApprovalDecision::Rejected,
                _ => {
                    return Err(TransitionRunError::Cli(
                        TransitionCliError::InvalidArgument { name: "--decision" },
                    ));
                }
            };
            let outcome = LegacyTransitionService::new(database)
                .record_transition_approval(&TransitionApprovalRequest {
                    account_id,
                    internal_owner_id,
                    shadow_report_id,
                    checklist_digest,
                    owner_evidence_digest,
                    decision,
                    decided_at: chrono::Utc::now(),
                })
                .await?;
            let output = serde_json::to_vec_pretty(&serde_json::json!({
                "approval_id": outcome.approval_id,
                "reused": outcome.reused,
                "decision": decision,
            }))
            .map_err(|_| TransitionRunError::Encoding)?;
            write_stdout(&output)?;
        }
    }
    Ok(())
}

fn is_help_request(arguments: &[OsString]) -> bool {
    match arguments {
        [help] => help == "--help" || help == "-h",
        [command, help] if help == "--help" || help == "-h" => {
            ratatoskr_x::transition_cli::parse_command(command).is_ok()
        }
        _ => false,
    }
}

async fn handle_preflight(
    source_kind: SourceArgumentKind,
    source_path: PathBuf,
) -> Result<(), TransitionRunError> {
    let batch = LegacyTransitionService::preflight_source(
        source_selection(source_kind, source_path),
        SourceLimits::default(),
    )
    .await?;
    let output = serde_json::to_vec_pretty(&serde_json::json!({
        "source_kind": source_kind_name(batch.source_kind()),
        "source_version": source_version_number(batch.source_version()),
        "source_digest": batch.source_digest(),
        "row_count": batch.row_count(),
        "importer_parser_version": x_sync::IMPORTER_PARSER_VERSION,
    }))
    .map_err(|_| TransitionRunError::Encoding)?;
    write_stdout(&output)
}

async fn handle_import(
    source_kind: SourceArgumentKind,
    source_path: PathBuf,
    account_id: uuid::Uuid,
    internal_owner_id: uuid::Uuid,
    ownership_approval_digest: String,
) -> Result<(), TransitionRunError> {
    let batch = LegacyTransitionService::preflight_source(
        source_selection(source_kind, source_path),
        SourceLimits::default(),
    )
    .await?;
    let (database, cipher) = load_secure_database().await?;
    let identity =
        OfficialCurrentAccountIdentity::new(database.clone(), cipher, OFFICIAL_API_BASE_URL);
    let outcome = LegacyTransitionService::new(database)
        .import_batch(
            &identity,
            &batch,
            Some(&OwnershipApproval {
                account_id,
                internal_owner_id,
                source_digest: batch.source_digest().to_owned(),
                approval_digest: ownership_approval_digest,
            }),
        )
        .await?;
    let output = serde_json::to_vec_pretty(&serde_json::json!({
        "run_id": outcome.run_id,
        "reused": outcome.reused,
        "inserted": outcome.counts.inserted,
        "matched": outcome.counts.matched,
        "updated": outcome.counts.updated,
        "conflicted": outcome.counts.conflicted,
        "rejected": outcome.counts.rejected,
        "unmapped": outcome.counts.unmapped,
    }))
    .map_err(|_| TransitionRunError::Encoding)?;
    write_stdout(&output)
}

async fn load_database() -> Result<Database, TransitionRunError> {
    let config = x_core::config::load().map_err(|_| TransitionRunError::Configuration)?;
    Database::connect(&config.database.url, config.database.max_connections)
        .await
        .map_err(|_| TransitionRunError::Database)
}

async fn load_secure_database() -> Result<(Database, TokenCipher), TransitionRunError> {
    let config = x_core::config::load().map_err(|_| TransitionRunError::Configuration)?;
    let key = config
        .security
        .token_encryption_key
        .as_ref()
        .ok_or(TransitionRunError::Cipher)?;
    let cipher = TokenCipher::from_secret_key(key).map_err(|_| TransitionRunError::Cipher)?;
    let database = Database::connect(&config.database.url, config.database.max_connections)
        .await
        .map_err(|_| TransitionRunError::Database)?;
    Ok((database, cipher))
}

fn source_selection(kind: SourceArgumentKind, path: PathBuf) -> LegacySourceSelection {
    match kind {
        SourceArgumentKind::MonolithCsv => LegacySourceSelection::monolith_csv(path),
        SourceArgumentKind::FieldTheoryJsonl => {
            LegacySourceSelection::field_theory(Some(path), None)
        }
        SourceArgumentKind::FieldTheorySqlite => {
            LegacySourceSelection::field_theory(None, Some(path))
        }
    }
}

fn source_kind_name(kind: x_sync::LegacySourceKind) -> &'static str {
    match kind {
        x_sync::LegacySourceKind::MonolithBookmarkMetadataCsv => "monolith_csv",
        x_sync::LegacySourceKind::FieldTheoryJsonl => "field_theory_jsonl",
        x_sync::LegacySourceKind::FieldTheorySqlite => "field_theory_sqlite",
    }
}

fn source_version_number(version: x_sync::LegacySourceVersion) -> i32 {
    match version {
        x_sync::LegacySourceVersion::MonolithBookmarkMetadata
        | x_sync::LegacySourceVersion::FieldTheoryJsonl1 => 1,
        x_sync::LegacySourceVersion::FieldTheorySqlite6 => 6,
    }
}

fn write_output(path: Option<&Path>, bytes: &[u8]) -> Result<(), TransitionRunError> {
    if let Some(path) = path {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut output = options.open(path).map_err(|_| TransitionRunError::Output)?;
        output
            .write_all(bytes)
            .and_then(|()| output.sync_all())
            .map_err(|_| TransitionRunError::Output)
    } else {
        write_stdout(bytes)
    }
}

fn write_stdout(bytes: &[u8]) -> Result<(), TransitionRunError> {
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(bytes)
        .and_then(|()| stdout.write_all(b"\n"))
        .map_err(|_| TransitionRunError::Output)
}

#[tokio::main]
async fn main() -> ExitCode {
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    match run(arguments).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("ratatoskr-x-transition: {error}");
            if matches!(error, TransitionRunError::Cli(_)) {
                ExitCode::from(2)
            } else {
                ExitCode::from(1)
            }
        }
    }
}
