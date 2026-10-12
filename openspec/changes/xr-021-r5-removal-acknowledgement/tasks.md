## 0. Pin

- [x] 0.1 Move every `ratatoskr-*` git dependency to `d98967b7b7ee80442c93ef4657140068955c8f61` and refresh `Cargo.lock`. A dependency pin cannot start from a failing test; the full gate is the check.

## 1. State machine and relay (R5-11 rule 4)

- [x] 1.1 Add `crates/x-persistence/tests/removal_ack.rs` and the resend cases of `crates/x-capture/tests/outbox_relay.rs`, with the four columns in `schema.sql` and signature-only stubs of the transitions; run them RED, each fails on an assertion (no `next_resend_at` armed, no resend selected, no acknowledgement recorded).
- [x] 1.2 Implement the transitions in `x-persistence/src/outbox.rs`, the resend selection and bookkeeping in `relay.rs`, the `ResendPolicy` constructor argument and the `tombstone` configuration keys so the tests pass.

## 2. The consumer (R5-11 rules 3 and 4)

- [x] 2.1 Add `services/x/tests/deletion_acks.rs` (real broker, PostgreSQL) and the durable cases of `services/x/tests/bus_identity.rs`; run them RED against a signature-only `deletion_acks` module.
- [x] 2.2 Implement `services/x/src/deletion_acks.rs`, the gauges and the counter, the wiring in `main.rs`, the stanza in `deploy/nats/identity.conf`, and the README, DEVELOPMENT and conf-example text.

## 3. A removal is never dropped while unacknowledged

- [x] 3.1 Add the delete-guard tests to `crates/x-persistence/tests/schema.rs`; run them RED, the delete succeeds today. The repository has no code that deletes outbox rows, so the guard lives in the database.
- [x] 3.2 Add the `before delete` trigger to `schema.sql` so the tests pass.

## 4. Gate

- [x] 4.1 Run the full local gate (`cargo fmt`, `cargo clippy`, the 850-line check, `cargo test --workspace --locked`, `cargo deny --locked check`, `openspec validate --all --strict`, `openspec validate --archived`) and record one real-broker transcript: publish, no acknowledgement, resend with the same `Nats-Msg-Id`, acknowledgement, stop.
