## 1. Membership reconciliation

- [x] 1.1 Add `crates/x-sync/tests/folder_snapshot.rs::complete_membership_snapshot_records_addition_and_observed_removal`, backed by redacted folder fixtures, and run it to confirm its active-membership assertion fails before folder synchronization exists.
- [x] 1.2 Add folder, membership-staging, authority, and observation persistence plus the minimal folder snapshot service needed for `complete_membership_snapshot_records_addition_and_observed_removal` to pass.

## 2. Atomic authority

- [x] 2.1 Add `crates/x-sync/tests/folder_snapshot.rs::keeps_previous_membership_authority_until_complete_swap` and verify the already-complete authority swap from task 1.2 against an independently seeded prior authority.
- [x] 2.2 Confirm the completion transaction swaps only the per-folder authority pointer and `keeps_previous_membership_authority_until_complete_swap` passes without exposing a partial replacement.

## 3. Capability limits

- [x] 3.1 Add `crates/x-sync/tests/folder_snapshot.rs::folder_capability_limit_records_no_authority_or_fabricated_state` and run it to confirm its durable-capability assertion fails before capability-limit handling exists.
- [x] 3.2 Model provider folder capability limits as non-authoritative terminal outcomes so `folder_capability_limit_records_no_authority_or_fabricated_state` passes.

## 4. Documentation and verification

- [x] 4.1 Add the redacted folder fixtures and update `README.md` with the independent, consistent bookmark/folder/local-organization authority boundary; documentation and fixtures cannot start from a meaningful failing behavior test.
- [x] 4.2 Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`, `cargo nextest run --locked --workspace`, `git diff --check`, `openspec validate --all --strict`, and `openspec validate --archived` through the machine build gate and record the observed outcomes.
