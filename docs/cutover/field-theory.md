# Field Theory import, shadow, and cutover runbook

This workflow imports legacy observations without granting them official X snapshot authority. It
never imports cookies, OAuth tokens, browser sessions, password material, or Field Theory's
`oauth-token.json`. Source archives remain read-only and are not deleted.

The commands below use synthetic IDs and paths. Run `ratatoskr-x-transition --help` or append
`--help` to any of its five commands to see the closed argument vocabulary.

## Prerequisites and stop gate

Stop before a real import unless the owner has supplied all of:

1. a reviewed read-only copy of `bookmarks.jsonl`, `bookmarks.db`, or the retired-monolith CSV;
2. an explicit existing Ratatoskr `account_id` and its matching `internal_owner_id`;
3. a current connected OAuth credential whose official `GET /2/users/me` identity matches the
   selected account;
4. acceptance of the exact shadow findings and checklist digest;
5. a separate `ratatoskr-workspace` changeset for any fleet, consumer, or routing cutover.

No real owner archive was available in this checkout when this runbook was written. The committed
fixtures under `crates/x-sync/tests/fixtures/legacy/` are synthetic acceptance evidence only.

## 1. Back up and preflight

Retain a byte-for-byte archive backup outside this repository. Do not move the source into the
checkout and do not make it writable for the importer.

```sh
ratatoskr-x-transition preflight \
  --source-kind field-theory-jsonl \
  --source /private/ratatoskr-transition/bookmarks.jsonl
```

Use `field-theory-sqlite` for the exact SQLite v6 projection or `monolith-csv` for the exact
nine-column retired-monolith export. JSONL is the preferred Field Theory evidence when both
artifacts exist. Record the returned source digest and parser version; a later source-byte change
requires a new preflight and owner mapping approval.

## 2. Import with explicit owner mapping

The approval digest is SHA-256 of the retained owner-mapping evidence, not of a token or session.
The command re-preflights the source, loads only the selected account's existing encrypted current
OAuth credential from service configuration, calls official `/2/users/me`, and refuses before
target writes on any identity mismatch.

```sh
ratatoskr-x-transition import \
  --source-kind field-theory-jsonl \
  --source /private/ratatoskr-transition/bookmarks.jsonl \
  --account-id 018f0000-0000-7000-8000-000000000101 \
  --internal-owner-id 018f0000-0000-7000-8000-000000000102 \
  --ownership-approval-digest aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
```

Repeat the exact command once and confirm `reused: true`, the same `run_id`, and identical terminal
counts. Conflicts and unmapped rows are evidence, not failures hidden by inference. Imported rows
must not create authoritative bookmarks, native folder membership, or outbox events.

## 3. Compare a complete official snapshot

Run the existing supported OAuth full-bookmark snapshot workflow first. Partial, running, failed,
cancelled, rate-limited, truncated, schema-invalid, stale, or cross-account snapshots cannot
produce a reviewable report.

```sh
ratatoskr-x-transition shadow-report \
  --account-id 018f0000-0000-7000-8000-000000000101 \
  --import-run-id 018f0000-0000-7000-8000-000000000201 \
  --snapshot-id 018f0000-0000-7000-8000-000000000301 \
  --output /private/ratatoskr-transition/shadow-report.json
```

The output file is created with private permissions and is never overwritten. Review `matched`,
`legacy_only`, `official_only`, `identity_conflict`, and `unmapped` entries plus independent
content/category/folder flags. It contains digests and scoped IDs, never source paths, post bodies,
URLs, handles, credentials, or session values.

## 4. Generate and approve the exact checklist

```sh
ratatoskr-x-transition checklist \
  --account-id 018f0000-0000-7000-8000-000000000101 \
  --shadow-report-id 018f0000-0000-7000-8000-000000000401 \
  --output /private/ratatoskr-transition/field-theory-checklist.md

shasum -a 256 /private/ratatoskr-transition/field-theory-checklist.md
shasum -a 256 /private/ratatoskr-transition/owner-decision.txt

ratatoskr-x-transition record-approval \
  --account-id 018f0000-0000-7000-8000-000000000101 \
  --internal-owner-id 018f0000-0000-7000-8000-000000000102 \
  --shadow-report-id 018f0000-0000-7000-8000-000000000401 \
  --checklist-digest bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb \
  --owner-evidence-digest cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc \
  --decision approved
```

Approval is valid only for the exact report and checklist digests. A new report/checklist
supersedes the current approval, retains the old evidence, and returns the old report to
`owner_review_required`. A rejected decision never makes a report reviewable.

## 5. Cutover boundary

This repository deliberately has no cutover command, routing mutation, or HTTP route. Only after
the exact checklist is owner-approved may the separate `ratatoskr-workspace` changeset change
fleet/consumer routing. That changeset must name compatibility, rollout order, privacy and cost
impact, monitoring/stability window, and the prior routing revision used for rollback.

## Rollback

1. Stop or revert only the external routing revision through the workspace changeset's documented
   deployment command.
2. Keep the legacy archive, import rows, normalized legacy projections, shadow reports,
   checklists, approvals, and supersession evidence. Do not delete either source or target data.
3. Restore the prior official-OAuth-only routing revision.
4. Run a new complete official snapshot, regenerate the shadow report and checklist, and require a
   new exact owner approval before attempting cutover again.

Rollback never makes imported absence authoritative and never restores cookies, tokens, or session
material.
