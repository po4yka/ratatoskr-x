## 1. Snapshot schema

- [x] 1.1 Add `crates/x-persistence/tests/schema.rs::snapshot_authority_schema_carries_staging_checkpoints_and_statistics`, run it against the current schema, and confirm its catalog assertion fails because the staging/authority/evidence columns are absent.
- [x] 1.2 Edit `schema.sql` in place and extend the schema assertions so the task 1.1 catalog test passes: no migration file/tooling; verify with `build-gate -- cargo nextest run --locked -p ratatoskr-x-persistence --test schema snapshot_authority_schema_carries_staging_checkpoints_and_statistics`.

## 2. Resumable, budget-bounded page ingestion

- [x] 2.1 Add `crates/x-sync/tests/bookmark_snapshot.rs::resumes_from_checkpoint_after_mid_run_provider_failure` with a hand-written paginated source harness, run it, and confirm the assertion fails because no durable full-run checkpoint/resume behavior exists (add only the minimal compiling API stub if needed).
- [x] 2.2 Implement the `x-sync` source/clock seams plus run creation, budget-before-call, normalization, normalized graph upserts, page-membership staging, and same-transaction opaque checkpoint persistence; verify the task 2.1 test passes through the real disposable PostgreSQL harness.
- [x] 2.3 Add `crates/x-sync/tests/bookmark_snapshot.rs::budget_exhaustion_stops_before_the_unfetched_page`, run it, and confirm its no-second-provider-call assertion fails under the current runner.
- [x] 2.4 Implement the budget-exhaustion terminal path so it preserves the committed checkpoint/staging set and never grants authority; verify the task 2.3 test passes.

## 3. Atomic completion authority

- [x] 3.1 Add `crates/x-sync/tests/bookmark_snapshot.rs::keeps_previous_authority_visible_until_complete_swap`, run it, and confirm it fails because staged pages currently cannot be finalized into an atomic authority replacement.
- [x] 3.2 Implement explicit serializable finalization with a transaction-scoped account lock, complete snapshot/run markers, current-authority replacement, and explicit commit; verify the task 3.1 test passes and an incomplete run never replaces the old authority.

## 4. Truthful removal observations and reconciliation statistics

- [x] 4.1 Add `crates/x-sync/tests/bookmark_snapshot.rs::records_unbookmark_observation_without_deleting_the_bookmark`, run it, and confirm it fails because complete-snapshot absence reconciliation is not implemented.
- [x] 4.2 Implement full-snapshot absence reconciliation that retains the bookmark row, stamps `observed_removed_at`, and records the completing snapshot evidence; verify the task 4.1 test passes.
- [x] 4.3 Add `crates/x-sync/tests/bookmark_snapshot.rs::reconciles_added_retained_and_removed_counts`, run it, and confirm its persisted statistics assertion fails because finalization does not yet count the three outcomes.
- [x] 4.4 Implement deterministic addition/retention/removal accounting, including retention of first-observation timestamps and refresh of last-observation timestamps; verify the task 4.3 test passes.

## 5. Change validation and integration

- [x] 5.1 Run `cargo fmt --all -- --check`, `build-gate -- cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`, `build-gate -- cargo nextest run --locked --workspace`, `build-gate -- cargo test --locked --workspace --doc`, `openspec validate full-bookmark-snapshot --strict`, `openspec validate --all --strict`, and `openspec validate --archived`; verify all are green before marking this task complete. This is a validation-only task, so a RED test does not apply.
