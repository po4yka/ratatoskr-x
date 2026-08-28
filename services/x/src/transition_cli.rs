//! Closed command vocabulary for the local legacy-transition operator binary.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

/// The only operator operations exposed by `ratatoskr-x-transition`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionCommand {
    /// Validate an allow-listed source without target writes.
    Preflight,
    /// Import one preflighted source for an approved account.
    Import,
    /// Compare one completed import with one complete official snapshot.
    ShadowReport,
    /// Generate the evidence-bound transition checklist.
    Checklist,
    /// Record explicit owner approval for the current checklist digest.
    RecordApproval,
}

/// Exact source-format selector accepted by local preflight/import commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceArgumentKind {
    /// Retired-monolith nine-column CSV.
    MonolithCsv,
    /// Field Theory JSONL v1 raw cache.
    FieldTheoryJsonl,
    /// Field Theory `SQLite` v6 derived index.
    FieldTheorySqlite,
}

/// Fully parsed, credential-free local operator invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionInvocation {
    /// Validate one explicit local artifact.
    Preflight {
        /// Exact source shape.
        source_kind: SourceArgumentKind,
        /// Explicit local path, never persisted or logged.
        source_path: PathBuf,
    },
    /// Import one explicit local artifact using current service OAuth identity internally.
    Import {
        /// Exact source shape.
        source_kind: SourceArgumentKind,
        /// Explicit local path, never persisted or logged.
        source_path: PathBuf,
        /// Explicit existing Ratatoskr X account.
        account_id: uuid::Uuid,
        /// Explicit internal owner bound to the account.
        internal_owner_id: uuid::Uuid,
        /// SHA-256 of reviewed owner mapping evidence.
        ownership_approval_digest: String,
    },
    /// Compare one completed import to one complete official snapshot.
    ShadowReport {
        /// Explicit account scope.
        account_id: uuid::Uuid,
        /// Completed import run.
        import_run_id: uuid::Uuid,
        /// Complete current official snapshot.
        snapshot_id: uuid::Uuid,
        /// Optional local report output; stdout is used when absent.
        output: Option<PathBuf>,
    },
    /// Generate the deterministic owner checklist for one report.
    Checklist {
        /// Explicit account scope.
        account_id: uuid::Uuid,
        /// Persisted shadow report.
        shadow_report_id: uuid::Uuid,
        /// Optional local checklist output; stdout is used when absent.
        output: Option<PathBuf>,
    },
    /// Persist one exact owner decision.
    RecordApproval {
        /// Explicit account scope.
        account_id: uuid::Uuid,
        /// Explicit internal owner bound to the account.
        internal_owner_id: uuid::Uuid,
        /// Persisted shadow report reviewed by the owner.
        shadow_report_id: uuid::Uuid,
        /// SHA-256 of the exact generated checklist.
        checklist_digest: String,
        /// SHA-256 of retained owner decision evidence.
        owner_evidence_digest: String,
        /// Closed owner decision.
        decision: String,
    },
}

/// Command-line parsing failures that occur before application work.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TransitionCliError {
    /// No operation was selected.
    #[error("one transition operation is required")]
    MissingCommand,
    /// The operation is outside the closed transition vocabulary.
    #[error("the transition operation is not supported")]
    UnsupportedCommand,
    /// The operation seam is intentionally incomplete during the first RED scaffold.
    #[error("the transition operation is not implemented")]
    NotImplemented,
    /// A credential/session-shaped CLI field is never accepted.
    #[error("transition argument `{name}` is forbidden")]
    ForbiddenArgument {
        /// Rejected argument name; never its value.
        name: String,
    },
    /// A required non-secret field is absent.
    #[error("transition argument `{name}` is required")]
    MissingArgument {
        /// Required argument name.
        name: &'static str,
    },
    /// A field value does not satisfy its bounded public syntax.
    #[error("transition argument `{name}` is invalid")]
    InvalidArgument {
        /// Invalid argument name; never its value.
        name: &'static str,
    },
    /// An unknown or positional argument was supplied.
    #[error("the transition argument is not supported")]
    UnsupportedArgument,
}

/// Parses one complete credential-free command invocation.
///
/// # Errors
/// Returns a classified name-only error without exposing argument values.
pub fn parse_arguments(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<TransitionInvocation, TransitionCliError> {
    let arguments: Vec<OsString> = arguments.into_iter().collect();
    for argument in &arguments {
        if let Some(name) = argument.to_str().filter(|value| value.starts_with("--"))
            && forbidden_argument_name(name)
        {
            return Err(TransitionCliError::ForbiddenArgument {
                name: name.to_owned(),
            });
        }
    }
    let mut arguments = arguments.into_iter();
    let command = arguments.next().ok_or(TransitionCliError::MissingCommand)?;
    let command = parse_command(&command)?;
    let remaining: Vec<OsString> = arguments.collect();
    let mut fields = parse_fields(remaining)?;
    let invocation = match command {
        TransitionCommand::Preflight => TransitionInvocation::Preflight {
            source_kind: take_source_kind(&mut fields)?,
            source_path: take_path(&mut fields, "--source")?,
        },
        TransitionCommand::Import => TransitionInvocation::Import {
            source_kind: take_source_kind(&mut fields)?,
            source_path: take_path(&mut fields, "--source")?,
            account_id: take_uuid(&mut fields, "--account-id")?,
            internal_owner_id: take_uuid(&mut fields, "--internal-owner-id")?,
            ownership_approval_digest: take_digest(&mut fields, "--ownership-approval-digest")?,
        },
        TransitionCommand::ShadowReport => TransitionInvocation::ShadowReport {
            account_id: take_uuid(&mut fields, "--account-id")?,
            import_run_id: take_uuid(&mut fields, "--import-run-id")?,
            snapshot_id: take_uuid(&mut fields, "--snapshot-id")?,
            output: take_optional_path(&mut fields, "--output"),
        },
        TransitionCommand::Checklist => TransitionInvocation::Checklist {
            account_id: take_uuid(&mut fields, "--account-id")?,
            shadow_report_id: take_uuid(&mut fields, "--shadow-report-id")?,
            output: take_optional_path(&mut fields, "--output"),
        },
        TransitionCommand::RecordApproval => TransitionInvocation::RecordApproval {
            account_id: take_uuid(&mut fields, "--account-id")?,
            internal_owner_id: take_uuid(&mut fields, "--internal-owner-id")?,
            shadow_report_id: take_uuid(&mut fields, "--shadow-report-id")?,
            checklist_digest: take_digest(&mut fields, "--checklist-digest")?,
            owner_evidence_digest: take_digest(&mut fields, "--owner-evidence-digest")?,
            decision: take_decision(&mut fields)?,
        },
    };
    if fields.is_empty() {
        Ok(invocation)
    } else {
        Err(TransitionCliError::UnsupportedArgument)
    }
}

fn parse_fields(
    arguments: Vec<OsString>,
) -> Result<BTreeMap<String, OsString>, TransitionCliError> {
    let mut fields = BTreeMap::new();
    let mut arguments = arguments.into_iter();
    while let Some(name) = arguments.next() {
        let Some(name) = name.to_str().filter(|value| value.starts_with("--")) else {
            return Err(TransitionCliError::UnsupportedArgument);
        };
        let value = arguments
            .next()
            .ok_or(TransitionCliError::UnsupportedArgument)?;
        if fields.insert(name.to_owned(), value).is_some() {
            return Err(TransitionCliError::UnsupportedArgument);
        }
    }
    Ok(fields)
}

fn take_source_kind(
    fields: &mut BTreeMap<String, OsString>,
) -> Result<SourceArgumentKind, TransitionCliError> {
    let value = take_string(fields, "--source-kind")?;
    match value.as_str() {
        "monolith-csv" => Ok(SourceArgumentKind::MonolithCsv),
        "field-theory-jsonl" => Ok(SourceArgumentKind::FieldTheoryJsonl),
        "field-theory-sqlite" => Ok(SourceArgumentKind::FieldTheorySqlite),
        _ => Err(TransitionCliError::InvalidArgument {
            name: "--source-kind",
        }),
    }
}

fn take_uuid(
    fields: &mut BTreeMap<String, OsString>,
    name: &'static str,
) -> Result<uuid::Uuid, TransitionCliError> {
    take_string(fields, name)?
        .parse()
        .map_err(|_| TransitionCliError::InvalidArgument { name })
}

fn take_digest(
    fields: &mut BTreeMap<String, OsString>,
    name: &'static str,
) -> Result<String, TransitionCliError> {
    let value = take_string(fields, name)?;
    if value.len() == 64
        && value
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    {
        Ok(value)
    } else {
        Err(TransitionCliError::InvalidArgument { name })
    }
}

fn take_decision(fields: &mut BTreeMap<String, OsString>) -> Result<String, TransitionCliError> {
    let decision = take_string(fields, "--decision")?;
    if matches!(decision.as_str(), "approved" | "rejected") {
        Ok(decision)
    } else {
        Err(TransitionCliError::InvalidArgument { name: "--decision" })
    }
}

fn take_string(
    fields: &mut BTreeMap<String, OsString>,
    name: &'static str,
) -> Result<String, TransitionCliError> {
    fields
        .remove(name)
        .ok_or(TransitionCliError::MissingArgument { name })?
        .into_string()
        .map_err(|_| TransitionCliError::InvalidArgument { name })
}

fn take_path(
    fields: &mut BTreeMap<String, OsString>,
    name: &'static str,
) -> Result<PathBuf, TransitionCliError> {
    fields
        .remove(name)
        .map(PathBuf::from)
        .ok_or(TransitionCliError::MissingArgument { name })
}

fn take_optional_path(
    fields: &mut BTreeMap<String, OsString>,
    name: &'static str,
) -> Option<PathBuf> {
    fields.remove(name).map(PathBuf::from)
}

fn forbidden_argument_name(name: &str) -> bool {
    let normalized: String = name
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

/// Parses one exact command name without inspecting environment or credentials.
///
/// # Errors
/// Returns [`TransitionCliError::MissingCommand`] for an empty value and
/// [`TransitionCliError::UnsupportedCommand`] for anything outside the closed vocabulary.
pub fn parse_command(command: &OsStr) -> Result<TransitionCommand, TransitionCliError> {
    if command.is_empty() {
        return Err(TransitionCliError::MissingCommand);
    }

    match command.to_str() {
        Some("preflight") => Ok(TransitionCommand::Preflight),
        Some("import") => Ok(TransitionCommand::Import),
        Some("shadow-report") => Ok(TransitionCommand::ShadowReport),
        Some("checklist") => Ok(TransitionCommand::Checklist),
        Some("record-approval") => Ok(TransitionCommand::RecordApproval),
        Some(_) | None => Err(TransitionCliError::UnsupportedCommand),
    }
}
