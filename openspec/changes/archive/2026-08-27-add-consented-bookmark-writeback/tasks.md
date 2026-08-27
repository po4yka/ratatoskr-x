## 1. Owned schema and isolated budget class

- [x] 1.1 Extend `crates/x-persistence/tests/schema.rs` with `bookmark_writeback_inventory_is_owned_scoped_and_secret_free`, run it through `build-gate -- cargo nextest run --locked -p ratatoskr-x-persistence --test schema`, and confirm RED because the four write-back tables, OAuth intent purpose/account binding, classed budget key, and bookmark write-evidence columns are absent.
- [x] 1.2 Edit the current `schema.sql` in place (no migration) with checked first-version write authorization/consent/operation/audit state and classed budget identity, update the exact owned-table inventory assertion, and run the task 1.1 test GREEN.
- [x] 1.3 Add `crates/x-budget/tests/gate.rs::bookmark_write_budget_is_isolated_and_inspection_is_non_consuming` against a minimal compileable class/inspection seam, run it through `build-gate -- cargo nextest run --locked -p ratatoskr-x-budget --test gate`, and confirm RED because write usage still shares the read row or inspection mutates usage.
- [x] 1.4 Implement the closed `BudgetClass`, class-bound gate construction, class-aware charge/refund/peek SQL, and read-only `inspect`; update every existing caller to its explicit read class and run the task 1.3 test plus all `ratatoskr-x-budget` tests GREEN.

## 2. Separately authorized bookmark-write OAuth grant

- [x] 2.1 Add `crates/x-oauth/tests/pkce.rs::bookmark_write_intent_adds_only_bookmark_write_while_default_stays_read_only` against a minimal compileable intent-purpose API, run it through `build-gate -- cargo nextest run --locked -p ratatoskr-x-oauth --test pkce`, and confirm RED because both flows still build the same read-only URL.
- [x] 2.2 Implement read versus bookmark-write intent purpose/account binding and the exact read-plus-`bookmark.write` scope construction without changing the default URL; run the task 2.1 test GREEN.
- [x] 2.3 Add `crates/x-oauth/tests/exchange.rs::write_callback_replaces_credential_only_for_matching_account_and_complete_scope` using redacted provider fixtures, run it through `build-gate -- cargo nextest run --locked -p ratatoskr-x-oauth --test exchange`, and confirm RED because the callback cannot activate local write authority or preserve the prior credential on identity/scope mismatch.
- [x] 2.4 Implement matching-provider identity lookup, full-scope validation, atomic encrypted credential replacement, local write-authorization activation, and downgraded/mismatched audit evidence; run the task 2.3 test and the complete `ratatoskr-x-oauth` suite GREEN.

## 3. One-action consent gate

- [x] 3.1 Add `crates/x-sync/tests/bookmark_writeback.rs::consent_gate_blocks_unconsented_writes_before_budget_and_provider` with a minimal compileable write-back service and hand-written provider fake, run it through `build-gate -- cargo nextest run --locked -p ratatoskr-x-sync --test bookmark_writeback`, and confirm RED because an unconsented live add reaches the fake or consumes write budget.
- [x] 3.2 Implement owner/account/write-authorization gate ordering plus immutable consent recording bound to action, target, approval time, expiry, and validated surface; run the task 3.1 test GREEN with zero provider calls and zero budget use.
- [x] 3.3 Add `crates/x-sync/tests/bookmark_writeback.rs::consent_is_bound_to_exact_action_and_consumed_once` and run it RED because wrong-target/action requests or concurrent consumers are currently admitted, while the retained row assertion must show who approved what, when, and via which surface.
- [x] 3.4 Implement transactional consent matching and at-most-once live consumption while leaving consent untouched on pre-provider refusal; run the task 3.3 test GREEN.

## 4. Idempotent operation claim

- [x] 4.1 Add `crates/x-sync/tests/bookmark_writeback.rs::idempotent_retry_returns_stored_result_without_second_provider_call` and run it RED because replay currently calls the fake twice or consumes a second budget unit.
- [x] 4.2 Implement account-scoped idempotency digests, canonical request fingerprints, one executor claim, stored result replay, and replay audit evidence; run the task 4.1 test GREEN with one provider call and one write-budget charge across restart-safe retries.
- [x] 4.3 Add `crates/x-sync/tests/bookmark_writeback.rs::idempotency_key_conflict_and_concurrent_duplicates_do_not_duplicate_side_effects` and run it RED because changed content is not rejected or racing exact requests can both execute.
- [x] 4.4 Serialize operation claims, reject fingerprint conflicts without changing the original operation, and converge in-flight duplicate callers on one identity/result; run the task 4.3 test GREEN.

## 5. Dry-run fidelity

- [x] 5.1 Add `crates/x-sync/tests/bookmark_writeback.rs::dry_run_fidelity_matches_live_admission_without_side_effects` covering eligible, already-satisfied, missing-consent, missing-scope, ownership, and exhausted-write-budget cases, then run it RED because no shared decision engine or non-consuming budget inspection drives both modes.
- [x] 5.2 Implement the shared typed admission evaluator and advisory dry-run result with evaluation/observation/reset instants; run the task 5.1 test GREEN while asserting no consent consumption, provider contact, budget row mutation, or bookmark-state change and a complete dry-run audit record.

## 6. Official bookmark mutation adapter

- [x] 6.1 Add `crates/x-sync/tests/bookmark_provider.rs::add_uses_authenticated_account_post_endpoint_and_expected_body` with WireMock and marker credentials, run it through `build-gate -- cargo nextest run --locked -p ratatoskr-x-sync --test bookmark_provider`, and confirm RED because no adapter sends POST `/2/users/{id}/bookmarks` with only `tweet_id`.
- [x] 6.2 Implement the bounded Rustls/Reqwest add adapter, loading the account-bound encrypted credential internally and parsing only `data.bookmarked = true`; run the task 6.1 test GREEN and verify no token appears in Debug/error output.
- [x] 6.3 Add `crates/x-sync/tests/bookmark_provider.rs::remove_uses_authenticated_account_and_target_path` and run it RED because no adapter sends DELETE `/2/users/{id}/bookmarks/{tweet_id}` or validates `data.bookmarked = false`.
- [x] 6.4 Implement the remove request and response validation with no unrelated X action method in the public adapter surface; run the task 6.3 test GREEN.
- [x] 6.5 Add `crates/x-sync/tests/bookmark_provider.rs::provider_failures_are_bounded_redacted_and_not_retried` for authorization loss, 429/reset metadata, definite refusal, oversized/malformed success, connect failure, timeout, and ambiguous server response; run it RED because the closed classifications and body/timeout bounds do not exist.
- [x] 6.6 Implement bounded response reading, end-to-end timeout, non-sensitive request/rate evidence, closed failure classification, and zero automatic retries; run the full `bookmark_provider` test binary GREEN.

## 7. Budgeted live execution and truthful projection

- [x] 7.1 Add `crates/x-sync/tests/bookmark_writeback.rs::write_budget_refusal_leaves_consent_and_read_allowance_untouched` and run it RED because live execution does not yet reserve the isolated class immediately before provider contact or refund a failed pre-contact commit.
- [x] 7.2 Implement elected-executor budget reservation, refusal/reset result, consent preservation, exact-reservation refund before contact, and conservative charge retention after contact; run the task 7.1 test GREEN.
- [x] 7.3 Add `crates/x-sync/tests/bookmark_writeback.rs::confirmed_add_remove_update_known_projection_with_write_evidence` and run it RED because confirmed results do not atomically update operation/bookmark state with distinct honest write-observation evidence.
- [x] 7.4 Implement confirmed add/remove/already-satisfied projection updates for normalized targets and `projection_pending` for unknown targets without creating placeholder posts; run the task 7.3 test GREEN.
- [x] 7.5 Add `crates/x-sync/tests/bookmark_writeback.rs::uncertain_result_is_not_retried_and_only_complete_snapshot_reconciles_it` and run it RED because an exact retry can repeat the fake mutation or partial/complete scans do not preserve the required distinction.
- [x] 7.6 Persist uncertain outcomes before returning, suppress retry provider calls, and extend complete-snapshot finalization to resolve uncertain add/remove operations with append-only evidence while partial scans remain non-authoritative; run task 7.5 plus `bookmark_snapshot` and `incremental_scan` test binaries GREEN.

## 8. Complete append-only audit evidence

- [x] 8.1 Add `crates/x-sync/tests/bookmark_writeback.rs::audit_trail_reconstructs_consent_gate_budget_provider_and_result` and run it RED because a successful request cannot yet reconstruct ordered approval, admission, budget, provider, projection, and terminal stages with actor/action/target/time/surface.
- [x] 8.2 Implement closed append-only audit events and bounded evidence serialization for every reached stage, including refusal, conflict, replay, dry run, uncertain, and reconciliation paths; run the task 8.1 test GREEN.
- [x] 8.3 Add `crates/x-sync/tests/bookmark_writeback.rs::audit_and_diagnostics_exclude_tokens_headers_content_and_raw_bodies` with marker secrets/private content, run it RED if any marker is persisted or rendered, then remove the leak at its typed boundary and run the complete `bookmark_writeback` test binary GREEN.
- [x] 8.4 Refactor the now-green write-back implementation into size-compliant modules without changing behavior or adding tests; verify `cargo fmt --all -- --check` and strict Clippy remain GREEN through `build-gate` where compiler-backed.

## 9. Documentation, archive, and full gate

- [x] 9.1 Update README, architecture/data-model/testing/threat-model/implementation-plan documentation with the implemented add/remove-only capability, separate OAuth and per-action consent, dry-run limits, classed budget, provider adapter, audit/uncertainty behavior, and lack of external runtime/UI wiring; no failing test applies because these are documentation artifacts, verify with `git diff --check` and `openspec validate add-consented-bookmark-writeback --strict`.
- [x] 9.2 Run format, strict workspace Clippy, affected crate/doc tests, and every command in the `DEVELOPMENT.md` full gate, acquiring `build-gate` once per top-level compiler-backed command; record exact GREEN outcomes and review the final diff for scope, secrets, unrelated edits, migration/version violations, and unfinished markers.
- [x] 9.3 Mark only observed-complete tasks checked, archive `add-consented-bookmark-writeback`, and run `openspec validate --all --strict` plus `openspec validate --archived` GREEN; no failing product test applies because archiving moves validated planning deltas into the current specs after implementation is already green.
