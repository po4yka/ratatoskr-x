## 1. Durable incremental state

- [x] 1.1 Add `crates/x-persistence/tests/schema.rs::incremental_scan_schema_carries_watermark_escalation_and_unique_repairs`, run it against the current schema, and confirm its catalog assertion fails because incremental state and repair evidence are absent.
- [x] 1.2 Edit `schema.sql` in place and extend the schema inventory so task 1.1 passes; no migration file or tooling. Verify with `build-gate -- cargo nextest run --locked -p ratatoskr-x-persistence --test schema incremental_scan_schema_carries_watermark_escalation_and_unique_repairs`.

## 2. Observation-only incremental scans

- [x] 2.1 Add `crates/x-sync/tests/incremental_scan.rs::advances_watermark_only_after_reaching_prior_watermark`, run it, and confirm its watermark assertion fails because no incremental scan API exists (add only the minimal compiling API stub if necessary).
- [x] 2.2 Implement the bounded incremental source/clock composition and durable observation upserts so task 2.1 passes: it must not infer removals and must advance only after reaching the committed provider watermark. Verify with the named test through the disposable PostgreSQL harness.
- [x] 2.3 Add `crates/x-sync/tests/incremental_scan.rs::page_bound_gap_requires_a_full_rescan`, run it, and confirm its `requires_full_snapshot` assertion fails under the current runner.
- [x] 2.4 Implement gap escalation and the refusal of later incremental scans until a complete snapshot succeeds; the full completion clears the escalation. Verify task 2.3 passes.

## 3. Tighter admission and scheduler command consumption

- [x] 3.1 Add `crates/x-sync/tests/incremental_scan.rs::budget_refusal_stops_before_incremental_provider_contact`, run it, and confirm its provider-call assertion fails under the current runner.
- [x] 3.2 Implement strict incremental request/page caps and durable budget-before-fetch admission so task 3.1 passes, with no provider call after refusal or cap exhaustion. Verify the named test passes.
- [x] 3.3 Add `crates/x-sync/tests/incremental_scan.rs::scheduled_scan_command_uses_the_platform_type_grammar`, run it, and confirm its command-selection assertion fails because the scheduler command is not consumed.
- [x] 3.4 Implement typed consumption of the existing scheduler command type `x.bookmarks.scan_requested.v1`: select a full snapshot for an escalated account and otherwise an incremental scan. Verify task 3.3 passes without adding a transport client or route.

## 4. Complete-snapshot repair evidence

- [x] 4.1 Extend `crates/x-sync/tests/bookmark_snapshot.rs::records_unbookmark_observation_without_deleting_the_bookmark` with incremental provenance and a repair assertion, run it, and confirm its unique repair-record assertion fails because completion stores no incremental-drift repair evidence.
- [x] 4.2 Extend full-snapshot finalization to clear escalation and record a unique repair for a bookmark whose incremental observation is corrected by the completed snapshot; verify task 4.1 passes, including the schema-enforced repeated-insert idempotence.

## 5. Change validation and integration

- [x] 5.1 Run `cargo fmt --all -- --check`, `build-gate -- cargo clippy --workspace --all-targets --locked -- -D warnings`, `build-gate -- cargo test --workspace --locked`, `build-gate -- cargo build --workspace --locked --release`, `npm exec --yes --package=@fission-ai/openspec@1.10.0 -- openspec validate safe-frequent-partial-scans --strict`, `npm exec --yes --package=@fission-ai/openspec@1.10.0 -- openspec validate --all --strict`, and `npm exec --yes --package=@fission-ai/openspec@1.10.0 -- openspec validate --archived`; verify all are green before marking this task complete. This is a validation-only task, so a RED test does not apply.
