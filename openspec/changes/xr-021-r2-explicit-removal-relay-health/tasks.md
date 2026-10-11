## 1. Provider deletion removes the owner source (R2-09)

- [x] 1.1 Add to `crates/x-sync/tests/public_capture/outcomes.rs` the tests `a_recapture_of_a_deleted_post_removes_the_owner_source_and_queues_removal`, `a_first_capture_of_a_deleted_post_removes_nothing`, `inaccessible_and_transient_outcomes_remove_nothing` and `a_restored_post_is_preserved_again_as_a_new_source`; run them RED, the first fails because the owner source row still exists and no `social.source.removed.v1` row is queued.
- [x] 1.2 Delete the owner source and queue the removal in `PublicCaptureWorker::terminate` for a `Deleted` failure so the four tests pass.

## 2. Relay health reaches readiness (R2-03, R2-04)

- [x] 2.1 Add `repeated_refusals_mark_the_relay_unhealthy_until_a_clean_pass` and `an_oversize_body_is_undeliverable_and_later_rows_publish` to `crates/x-capture/tests/outbox_relay.rs`, and `readiness_follows_the_relay` to `services/x/tests/smoke.rs`; run them RED, with a signature-only `RelayHealth` that always reports healthy and a relay that treats every failure as transient.
- [x] 2.2 Implement `RelayHealth`, the `payload_too_large` check in `JetStreamPublisher::send`, `undeliverable_at` in `schema.sql` and the readiness wiring in `services/x/src/main.rs` so the three tests pass.

## 3. Documentation and operator example (R2-10)

- [x] 3.1 Add `the_shipped_example_loads` to `services/x/tests/bootstrap.rs`; run it RED, `deploy/systemd/x.conf.example` does not exist.
- [x] 3.2 Add the example, rewrite the README section on the transport and the dormant bookmark and compliance lane, and point DEVELOPMENT.md at the example; the documentation has no meaningful RED.

## 4. Gate

- [ ] 4.1 Run the full local gate (`cargo fmt`, `cargo clippy`, the 850-line check, `cargo test --workspace --locked`, `cargo deny --locked check`, `openspec validate --all --strict`, `openspec validate --archived`).
