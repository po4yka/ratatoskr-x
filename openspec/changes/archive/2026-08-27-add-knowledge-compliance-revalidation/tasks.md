## 1. Knowledge request deduplication

- [x] 1.1 Add `crates/x-sync/tests/knowledge_integration.rs::concurrent_identical_observations_emit_one_knowledge_request` and run it RED; both snapshot calls must be expected to succeed, while the current race fails one call or produces more than one `(social_source_id, content_digest)` request.
- [x] 1.2 Serialize publication on the account/post identity, preserve the existing digest uniqueness, and run the exact test GREEN with one revision and one social-source outbox fact.

## 2. Owned schema

- [x] 2.1 Extend `crates/x-persistence/tests/schema.rs::knowledge_linkage_and_compliance_inventory_is_owned_and_scoped` and run it RED; it must report the missing analysis-link and compliance-ledger tables plus source-removal/tombstone evidence columns.
- [x] 2.2 Edit `schema.sql` in place to add the first-version linkage, ledger, removal, and tombstone constraints, update the exact owned-table inventory, and run the schema test GREEN.

## 3. Knowledge completion linkage

- [x] 3.1 Add `crates/x-sync/tests/knowledge_integration.rs::completion_redelivery_links_exact_revision_once` with the minimal compileable consumer seam and run it RED; the stub must refuse the valid typed completion and leave zero links.
- [x] 3.2 Implement typed Knowledge-envelope validation, owner/source/digest matching, atomic inbox/link persistence, and replay admission; run the exact test GREEN with one link and one receipt.
- [x] 3.3 Add `crates/x-sync/tests/knowledge_integration.rs::older_digest_completion_is_historical_not_current` and run it RED; the first implementation must reject or misclassify the retained older digest.
- [x] 3.4 Retain exact historical links while deriving currentness from the source head, reject removed/foreign/unknown revisions before inbox claim, and run the Knowledge integration test binary GREEN.

## 4. Compliance revalidation ledger

- [x] 4.1 Add `crates/x-sync/tests/compliance_revalidation.rs::available_due_source_records_one_revalidation_ledger_entry` with the minimal compileable provider/service seam and run it RED; the stub must leave the expected ledger row absent.
- [x] 4.2 Implement bounded due selection, one existing-budget reservation per official-provider call, available-state persistence, and non-sensitive request evidence; run the exact test GREEN.
- [x] 4.3 Add `crates/x-sync/tests/compliance_revalidation.rs::recent_and_excess_sources_are_not_checked` and run it RED against a deliberately minimal first selector; the fake must observe a recent or excess call beyond the requested bound.
- [x] 4.4 Enforce `due_before`, account ownership, unremoved state, deterministic oldest-first ordering, and the explicit item bound; run the exact test GREEN.
- [x] 4.5 Add `crates/x-sync/tests/compliance_revalidation.rs::indeterminate_failure_is_ledgered_without_takedown` and run it RED; the source error must currently escape without the required ledger row.
- [x] 4.6 Persist closed indeterminate failure classes without changing source/tombstone/outbox state, then run the exact test GREEN.

## 5. Atomic takedown propagation

- [x] 5.1 Add `crates/x-sync/tests/compliance_revalidation.rs::authoritative_deletion_records_tombstone_and_one_knowledge_deletion_request` and run it RED; a deleted observation must currently lack the source removal, tombstone, and `social.source.removed.v1` outbox fact.
- [x] 5.2 Atomically write the ledger, provider state, source removal, tombstone, and typed retention-policy removal fact, then run the exact test GREEN.
- [x] 5.3 Add `crates/x-sync/tests/compliance_revalidation.rs::repeated_takedown_and_delayed_work_cannot_resurrect_source` and run it RED; a repeated check or later ordinary observation/completion must currently create duplicate or active derived state.
- [x] 5.4 Make takedown singleton state replay-safe and suppress source publication/completion linkage after removal, then run both affected integration test binaries GREEN.

## 6. Documentation and validation

- [x] 6.1 Update README, architecture/data-model/testing documentation, and plan status with the agreed Knowledge boundary, ledger fields, periodic scheduling seam, retention limitation, and takedown path; no failing test applies because these are documentation artifacts, verify with `git diff --check` and OpenSpec strict validation.
- [x] 6.2 Run format and strict Clippy, the affected crates and doc tests, then the complete `DEVELOPMENT.md` gate through `build-gate` for compiler-backed commands; verify every command is green and review the final diff for scope, secrets, stale generated files, and unfinished markers.
