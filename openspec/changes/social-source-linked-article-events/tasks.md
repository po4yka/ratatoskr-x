## 1. Contract pin and schema foundation

- [x] 1.1 Add the single `b7aa013c78ef73eb2a5798935e74167c4cf98314` ratatoskr-contracts git revision and intentionally resolve Cargo.lock; configuration has no meaningful RED, so verify `cargo metadata --locked` reports one source revision for all direct contract crates.
- [x] 1.2 Add schema assertions in `crates/x-persistence/tests/schema.rs::social_source_and_article_capture_inventory_is_owned_and_scoped` that fail because account ownership, source records, article captures, and post links do not exist.
- [x] 1.3 Extend the current `schema.sql` and persistence test support without migrations so `social_source_and_article_capture_inventory_is_owned_and_scoped` passes.

## 2. SocialSource publication

- [x] 2.1 Add `crates/x-sync/tests/social_source_events.rs::bookmark_capture_serializes_the_pinned_social_contract_fixture` and run it RED; the assertion must fail because no SocialSource captured envelope is persisted.
- [x] 2.2 Implement account-scoped source snapshot construction and transactional captured-event outbox persistence so `bookmark_capture_serializes_the_pinned_social_contract_fixture` passes.
- [x] 2.3 Add `crates/x-sync/tests/social_source_events.rs::unchanged_observation_is_silent_and_material_change_emits_one_updated_event` and run it RED; the expected updated event is absent.
- [x] 2.4 Implement revision digest comparison and idempotent captured/updated selection so `unchanged_observation_is_silent_and_material_change_emits_one_updated_event` passes.
- [x] 2.5 Add `crates/x-sync/tests/social_source_events.rs::outbox_failure_rolls_back_source_transition` and run it RED; injected outbox failure currently leaves source state durable.
- [x] 2.6 Make source revision and event persistence one transaction so `outbox_failure_rolls_back_source_transition` passes.

## 3. Explicit X capture provenance

- [x] 3.1 Add `crates/x-sync/tests/explicit_capture.rs::x_social_capture_command_preserves_explicit_provenance` and run it RED; the valid shared command has no X processing path.
- [x] 3.2 Implement the typed `social.capture.requested.v1` X adapter and source transition so `x_social_capture_command_preserves_explicit_provenance` passes, while non-X providers and invalid authority/acquisition pairs are refused.
- [x] 3.3 Add `crates/x-sync/tests/explicit_capture.rs::redelivered_command_does_not_duplicate_source_event` and run it RED; duplicate command handling is absent.
- [x] 3.4 Persist explicit-command application/inbox evidence with the source transition so `redelivered_command_does_not_duplicate_source_event` passes.

## 4. Linked-article capture and outcomes

- [x] 4.1 Add `crates/x-sync/tests/article_capture.rs::external_expanded_urls_are_selected_once_and_x_urls_are_excluded` and run it RED; link classification is absent.
- [x] 4.2 Implement conservative expanded-link classification and canonicalization so `external_expanded_urls_are_selected_once_and_x_urls_are_excluded` passes.
- [x] 4.3 Add `crates/x-sync/tests/article_capture.rs::equivalent_links_across_posts_create_one_capture_command_and_two_links` and run it RED; account URL capture deduplication is absent.
- [x] 4.4 Persist account URL captures, post links, and the existing extractor command envelope atomically so `equivalent_links_across_posts_create_one_capture_command_and_two_links` passes.
- [x] 4.5 Add `crates/x-sync/tests/article_capture.rs::correlated_document_ir_outcome_updates_every_linked_post_once` and run it RED; extractor outcomes are not consumed.
- [x] 4.6 Implement validated, idempotent extractor operation-report consumption so `correlated_document_ir_outcome_updates_every_linked_post_once` passes.
- [x] 4.7 Add `crates/x-sync/tests/article_capture.rs::foreign_or_malformed_outcomes_leave_capture_unapplied` and run it RED; foreign/malformed outcomes are not rejected.
- [x] 4.8 Reject foreign owners, unknown correlations, and invalid document results before inbox application so `foreign_or_malformed_outcomes_leave_capture_unapplied` passes.

## 5. Verification and delivery

- [x] 5.1 Run targeted crate tests and contract-fixture compatibility checks under `build-gate`, then run `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`.
- [x] 5.2 Run the repository's full documented gate, inspect the final diff and staged diff, and update task checkboxes only for observed completion.
- [ ] 5.3 Commit the scoped change, rebase/integrate it into current `main`, push `main`, confirm remote ancestry, then remove only this task worktree and branch.
