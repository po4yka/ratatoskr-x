## 1. Contracts pin

- [ ] 1.1 Move all five `ratatoskr-*` git dependencies to `ad16855c4e7f3d52cd118274faa3b8f3ab4da576` and refresh `Cargo.lock`; a dependency pin has no meaningful RED, so verify that `cargo metadata --locked` reports one source revision and the existing suite stays green.

## 2. Operator port

- [ ] 2.1 Change `crates/x-core/tests/config.rs::absent_variables_yield_documented_defaults` to expect `127.0.0.1:9087` and add `operator_listener_default_differs_from_the_edge_public_port`; run both RED, the assertion fails because the default is `127.0.0.1:8080`.
- [ ] 2.2 Move the default, its doc comment and its violation message in `crates/x-core/src/config.rs` and the README and DEVELOPMENT mentions so both tests pass.

## 3. Complete-envelope outbox rows

- [ ] 3.1 Add `crates/x-sync/tests/social_sources.rs::outbox_row_is_a_complete_event_envelope`, `compliance_removal_row_is_a_complete_event_envelope` and `article_capture_row_is_a_complete_command_envelope`; run them RED, the stored payload is a bare document and `EventEnvelope::from_json` or `CommandEnvelope::from_json` fails.
- [ ] 3.2 Add `crates/x-persistence/src/outbox.rs` (`enqueue_event`, `enqueue_command`), the `event_type` CHECK and the attempt columns in `schema.sql`, and use the helper at the bookmark, compliance and article sites so the three tests and the updated `social_source_events.rs` pass.

## 4. Intake

- [ ] 4.1 Add `crates/x-capture/tests/persistence.rs::persists_the_tenant_as_owner_and_queues_the_operation`, the six-host permalink test, `a_command_without_a_tenant_is_rejected_as_missing_tenant` and `a_permalink_without_a_status_id_is_reported_unavailable`; run them RED, the owner, queued report and typed refusal are absent.
- [ ] 4.2 Rewrite `explicit_captures`, add `explicit_sources` in `schema.sql`, require the tenant, add `status_id_from_permalink` and write the queued or terminal-failed report in the claim transaction so the tests pass.

## 5. Tenant isolation

- [ ] 5.1 Add `crates/x-sync/tests/explicit_capture.rs::explicit_capture_never_publishes_another_tenants_protected_post` and run it against the current `ExplicitCaptureService`; it is RED because tenant B receives the text only account A was allowed to see. Record the failing output.
- [ ] 5.2 Delete `ExplicitCaptureService` and its tests and factor the shared source tail so the bookmark and explicit paths write a complete envelope; the leak assertion moves onto the worker in 7.2.

## 6. Public post resolver

- [ ] 6.1 Add `crates/x-sync/tests/public_post.rs` (WireMock) and the `public_capture` cases in `crates/x-core/tests/config.rs`; run them RED, the resolver and the configuration section do not exist.
- [ ] 6.2 Implement `PublicPostResolver`, `PublicPostFailure`, `AppBearerResolver` and `PublicCaptureConfig` so the tests pass.

## 7. Public capture worker

- [ ] 7.1 Add `crates/x-sync/tests/public_capture.rs` (scripted resolver, injected clock, `TestDatabase`) covering preserved, re-capture, two tenants, deleted and inaccessible, retry schedule, racing workers and the NotAuthorized leak case, plus a source-grep test; run them RED against a signature-only worker.
- [ ] 7.2 Implement `PublicCaptureWorker` so every test passes.

## 8. Relay and service wiring

- [ ] 8.1 Add `crates/x-capture/tests/outbox_relay.rs`, the bearer-token refusal and `readiness_goes_false_when_a_bus_task_stops` in `services/x/tests/smoke.rs`; run them RED, nothing reads the outbox and a stopped consumer is only logged.
- [ ] 8.2 Implement `OutboxRelay`, `RuntimeState::set_bus_ready` and the service wiring so the tests pass.

## 9. Extractor reports

- [ ] 9.1 Add `services/x/tests/extractor_reports.rs`; run it RED, no subscription exists.
- [ ] 9.2 Implement `services/x/src/extractor_reports.rs` so the test passes.

## 10. Composed proof

- [ ] 10.1 Add `crates/x-capture/tests/e2e.rs` for one permalink through the consumer, the worker and the relay against a real broker, for the preserved and the not-authorized outcomes; run it RED until the tasks above are combined.
- [ ] 10.2 Make the composed test pass (no new production behaviour is expected; any gap it exposes is fixed in the owning module).

## 11. Documentation and gate

- [ ] 11.1 Update `README.md`, `docs/ARCHITECTURE.md` and `DEVELOPMENT.md`; documentation has no meaningful RED.
- [ ] 11.2 Run the full local gate (`cargo fmt`, `cargo clippy`, the 850-line check, `cargo test --workspace --locked`, `cargo deny --locked check`, `openspec validate --all --strict`, `openspec validate --archived`).
