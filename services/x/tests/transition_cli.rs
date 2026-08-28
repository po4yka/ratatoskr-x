//! Closed, credential-free local transition CLI.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "assertions in a test binary"
)]

use std::ffi::{OsStr, OsString};

use ratatoskr_x::transition_cli::{
    SourceArgumentKind, TransitionCliError, TransitionCommand, TransitionInvocation,
    parse_arguments, parse_command,
};
use x_persistence::test_support::TestDatabase;
use x_sync::{
    ApprovalDecision, LegacyTransitionError, LegacyTransitionService, TransitionApprovalRequest,
};

fn arguments(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

#[test]
fn transition_cli_never_accepts_credential_or_session_arguments() {
    let commands = [
        ("preflight", TransitionCommand::Preflight),
        ("import", TransitionCommand::Import),
        ("shadow-report", TransitionCommand::ShadowReport),
        ("checklist", TransitionCommand::Checklist),
        ("record-approval", TransitionCommand::RecordApproval),
    ];
    for (name, expected) in commands {
        assert_eq!(
            parse_command(OsStr::new(name)).expect("the command is in the closed vocabulary"),
            expected
        );
    }
    for unsupported in ["sync", "cutover", "activate", "rollback", "serve"] {
        assert_eq!(
            parse_command(OsStr::new(unsupported)),
            Err(TransitionCliError::UnsupportedCommand)
        );
    }

    let allowed = parse_arguments(arguments(&[
        "preflight",
        "--source-kind",
        "field-theory-jsonl",
        "--source",
        "/private/synthetic/bookmarks.jsonl",
    ]));
    assert!(
        !matches!(allowed, Err(TransitionCliError::NotImplemented)),
        "the bounded argument parser remains NotImplemented"
    );
    assert_eq!(
        allowed.expect("the non-secret preflight invocation parses"),
        TransitionInvocation::Preflight {
            source_kind: SourceArgumentKind::FieldTheoryJsonl,
            source_path: "/private/synthetic/bookmarks.jsonl".into(),
        }
    );

    let secret_value = "synthetic-secret-must-not-surface";
    for forbidden_name in [
        "--access-token",
        "--refresh_token",
        "--cookie",
        "--session-file",
        "--authorization",
        "--client-secret",
    ] {
        let error = parse_arguments(arguments(&[
            "preflight",
            forbidden_name,
            secret_value,
            "--source-kind",
            "monolith-csv",
            "--source",
            "/private/synthetic/archive.csv",
        ]))
        .expect_err("credential/session-shaped CLI fields are rejected");
        assert!(matches!(
            error,
            TransitionCliError::ForbiddenArgument { .. }
        ));
        assert!(!error.to_string().contains(secret_value));
        assert!(!format!("{error:?}").contains(secret_value));
    }
    let help_bypass = parse_arguments(arguments(&["--help", "--token", secret_value]))
        .expect_err("help cannot bypass credential-field rejection");
    assert!(matches!(
        help_bypass,
        TransitionCliError::ForbiddenArgument { .. }
    ));
    assert!(!help_bypass.to_string().contains(secret_value));
}

async fn seed_report(
    test: &TestDatabase,
    account_id: uuid::Uuid,
    import_run_id: uuid::Uuid,
    snapshot_id: uuid::Uuid,
    marker: char,
) -> uuid::Uuid {
    let payload = serde_json::json!({
        "report_version": 1,
        "account_id": account_id,
        "import_run_id": import_run_id,
        "snapshot_id": snapshot_id,
        "import_digest": "c".repeat(64),
        "snapshot_digest": "d".repeat(64),
        "entries": [],
        "summary": {
            "matched": 0,
            "url_matched": 0,
            "legacy_only": 0,
            "official_only": 0,
            "identity_conflicts": 0,
            "unmapped": 0
        }
    });
    sqlx::query_scalar(
        "insert into x_archive.legacy_shadow_reports \
             (account_id, import_run_id, snapshot_id, import_digest, snapshot_digest, \
              report_payload, report_digest) \
         values ($1, $2, $3, $4, $5, $6, $7) returning id",
    )
    .bind(account_id)
    .bind(import_run_id)
    .bind(snapshot_id)
    .bind("c".repeat(64))
    .bind(format!("{marker}").repeat(64))
    .bind(payload)
    .bind(format!("{marker}").repeat(64))
    .fetch_one(test.database.pool())
    .await
    .expect("the synthetic shadow report seeds")
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "the end-to-end approval scenario keeps setup, supersession, retention, and no-routing assertions together"
)]
async fn checklist_and_matching_approval_are_digest_bound_and_stale_evidence_revokes_reviewability()
{
    let test = TestDatabase::create().await.expect("a disposable database");
    let account_id = test
        .seed_account("checklist-owner-provider")
        .await
        .expect("the checklist target account seeds");
    let internal_owner_id: uuid::Uuid =
        sqlx::query_scalar("select internal_user_id from x_archive.accounts where id = $1")
            .bind(account_id)
            .fetch_one(test.database.pool())
            .await
            .expect("the internal owner is readable");
    let import_run_id: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.legacy_import_runs \
             (account_id, source_kind, source_version, source_digest, importer_parser_version, \
              ownership_approval_digest, status, finished_at) \
         values ($1, 'field_theory_jsonl', 1, $2, 1, $3, 'completed', now()) returning id",
    )
    .bind(account_id)
    .bind("a".repeat(64))
    .bind("b".repeat(64))
    .fetch_one(test.database.pool())
    .await
    .expect("the completed import seeds");
    let sync_run_id: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.sync_runs \
             (account_id, run_type, state, finished_at) \
         values ($1, 'full', 'completed', now()) returning id",
    )
    .bind(account_id)
    .fetch_one(test.database.pool())
    .await
    .expect("the completed full run seeds");
    let snapshot_id: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.snapshots (sync_run_id, complete, completed_at) \
         values ($1, true, now()) returning id",
    )
    .bind(sync_run_id)
    .fetch_one(test.database.pool())
    .await
    .expect("the complete snapshot seeds");
    sqlx::query(
        "insert into x_archive.bookmark_snapshot_authority (account_id, snapshot_id) \
         values ($1, $2)",
    )
    .bind(account_id)
    .bind(snapshot_id)
    .execute(test.database.pool())
    .await
    .expect("the complete snapshot is current authority");
    let first_report_id = seed_report(&test, account_id, import_run_id, snapshot_id, 'e').await;
    let service = LegacyTransitionService::new(test.database.clone());

    let checklist_result = service.checklist_for(account_id, first_report_id).await;
    assert!(
        !matches!(checklist_result, Err(LegacyTransitionError::NotImplemented)),
        "checklist generation remains NotImplemented"
    );
    let checklist = checklist_result.expect("the deterministic checklist is generated");
    let repeated = service
        .checklist_for(account_id, first_report_id)
        .await
        .expect("the same checklist is deterministic");
    assert_eq!(repeated, checklist);
    for section in [
        "## Backup",
        "## Import counts",
        "## Complete official snapshot",
        "## Shadow findings",
        "## Privacy and cost review",
        "## Workspace changeset",
        "## Stability window",
        "## Rollback",
        "## Evidence retention",
    ] {
        assert!(checklist.markdown.contains(section), "missing `{section}`");
    }
    let owner_evidence_digest = "f".repeat(64);
    let approval = service
        .record_transition_approval(&TransitionApprovalRequest {
            account_id,
            internal_owner_id,
            shadow_report_id: first_report_id,
            checklist_digest: checklist.checklist_digest.clone(),
            owner_evidence_digest: owner_evidence_digest.clone(),
            decision: ApprovalDecision::Approved,
            decided_at: "2026-08-28T08:00:00Z"
                .parse()
                .expect("a fixed decision timestamp"),
        })
        .await
        .expect("the exact owner approval is recorded");
    assert!(!approval.reused);
    let first_state: String = sqlx::query_scalar(
        "select review_state from x_archive.legacy_shadow_reports where id = $1",
    )
    .bind(first_report_id)
    .fetch_one(test.database.pool())
    .await
    .expect("the first report state is readable");
    assert_eq!(first_state, "reviewable");

    let rejection = service
        .record_transition_approval(&TransitionApprovalRequest {
            account_id,
            internal_owner_id,
            shadow_report_id: first_report_id,
            checklist_digest: checklist.checklist_digest.clone(),
            owner_evidence_digest: owner_evidence_digest.clone(),
            decision: ApprovalDecision::Rejected,
            decided_at: "2026-08-28T08:00:30Z"
                .parse()
                .expect("a fixed rejection timestamp"),
        })
        .await
        .expect("the opposite owner decision is never mistaken for an idempotent reuse");
    assert!(!rejection.reused);
    assert_ne!(rejection.approval_id, approval.approval_id);
    let rejected_state: String = sqlx::query_scalar(
        "select review_state from x_archive.legacy_shadow_reports where id = $1",
    )
    .bind(first_report_id)
    .fetch_one(test.database.pool())
    .await
    .expect("the rejected report state is readable");
    assert_eq!(rejected_state, "owner_review_required");

    let second_report_id = seed_report(&test, account_id, import_run_id, snapshot_id, '9').await;
    let second_checklist = service
        .checklist_for(account_id, second_report_id)
        .await
        .expect("new evidence generates a new checklist and supersedes stale approval");
    let old_approval: (String, Option<chrono::DateTime<chrono::Utc>>) = sqlx::query_as(
        "select owner_evidence_digest, superseded_at \
         from x_archive.legacy_transition_approvals where id = $1",
    )
    .bind(approval.approval_id)
    .fetch_one(test.database.pool())
    .await
    .expect("the retained old approval is readable");
    assert_eq!(old_approval.0, owner_evidence_digest);
    assert!(old_approval.1.is_some());
    let first_state: String = sqlx::query_scalar(
        "select review_state from x_archive.legacy_shadow_reports where id = $1",
    )
    .bind(first_report_id)
    .fetch_one(test.database.pool())
    .await
    .expect("the stale report state is readable");
    assert_eq!(first_state, "owner_review_required");

    let stale_error = service
        .record_transition_approval(&TransitionApprovalRequest {
            account_id,
            internal_owner_id,
            shadow_report_id: second_report_id,
            checklist_digest: checklist.checklist_digest,
            owner_evidence_digest: "8".repeat(64),
            decision: ApprovalDecision::Approved,
            decided_at: "2026-08-28T08:01:00Z"
                .parse()
                .expect("a fixed stale decision timestamp"),
        })
        .await
        .expect_err("an old checklist digest cannot approve new evidence");
    assert!(matches!(
        stale_error,
        LegacyTransitionError::ChecklistDigestMismatch
    ));
    assert_ne!(second_checklist.checklist_digest, repeated.checklist_digest);
    let routing_side_effects: (i64, i64) = sqlx::query_as(
        "select (select count(*) from x_archive.outbox_events), \
                (select count(*) from x_archive.bookmarks)",
    )
    .fetch_one(test.database.pool())
    .await
    .expect("routing side-effect counts are readable");
    assert_eq!(routing_side_effects, (0, 0));

    let newer_run_id: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.sync_runs \
             (account_id, run_type, state, finished_at) \
         values ($1, 'full', 'completed', now()) returning id",
    )
    .bind(account_id)
    .fetch_one(test.database.pool())
    .await
    .expect("a newer complete run seeds");
    let newer_snapshot_id: uuid::Uuid = sqlx::query_scalar(
        "insert into x_archive.snapshots (sync_run_id, complete, completed_at) \
         values ($1, true, now()) returning id",
    )
    .bind(newer_run_id)
    .fetch_one(test.database.pool())
    .await
    .expect("a newer complete snapshot seeds");
    sqlx::query(
        "update x_archive.bookmark_snapshot_authority set snapshot_id = $2 \
         where account_id = $1",
    )
    .bind(account_id)
    .bind(newer_snapshot_id)
    .execute(test.database.pool())
    .await
    .expect("newer official evidence supersedes the old snapshot authority");
    let stale_snapshot_error = service
        .record_transition_approval(&TransitionApprovalRequest {
            account_id,
            internal_owner_id,
            shadow_report_id: second_report_id,
            checklist_digest: second_checklist.checklist_digest,
            owner_evidence_digest: "7".repeat(64),
            decision: ApprovalDecision::Approved,
            decided_at: "2026-08-28T08:02:00Z"
                .parse()
                .expect("a fixed stale-snapshot decision timestamp"),
        })
        .await
        .expect_err("a report for superseded official authority cannot be approved");
    assert!(matches!(
        stale_snapshot_error,
        LegacyTransitionError::SnapshotNotAuthoritative
    ));
    test.cleanup().await.expect("cleanup drops the database");
}
