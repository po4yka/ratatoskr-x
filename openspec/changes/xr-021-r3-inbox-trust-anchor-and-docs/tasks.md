## 1. Per-service inbox prefix (R3-01)

- [x] 1.1 Move the inline connection of `services/x/src/main.rs` into `connect_bus` (signature-only behaviour, no prefix), add `deploy/nats/identity.conf` and `services/x/tests/bus_identity.rs::the_service_completes_jetstream_requests_under_its_inbox_prefix`; run it RED, the durable verification times out because no reply reaches the client.
- [x] 1.2 Apply `custom_inbox_prefix(INBOX_PREFIX)` in `connect_bus` and switch the smoke scenario broker to the shipped stanza so the test passes.

## 2. A configured CA is trusted (R3-15 rule 1)

- [x] 2.1 Add `the_extra_ca_file_must_be_absolute_readable_and_hold_a_certificate`, `a_private_ca_is_trusted_only_when_configured`, `the_extra_ca_does_not_replace_the_platform_roots` and `the_success_leg_runs_over_https_with_a_private_ca`; run them RED, the key does not exist.
- [x] 2.2 Add `public_capture.extra_ca_file`, the resolver options and the wiring in `main` so they pass.

## 3. A terminal capture is terminal (R3-15 rule 2)

- [x] 3.1 Add `a_terminal_capture_leaves_no_future_next_attempt_at` to `crates/x-sync/tests/public_capture/outcomes.rs`; run it RED, the claim lease remains.
- [x] 3.2 Set `next_attempt_at = reported_at` in `terminate` so it passes.

## 4. The example names the shared database (R3-11)

- [x] 4.1 Add `the_example_names_the_shared_database_and_the_role_file_exists` to `services/x/tests/bootstrap.rs`; run it RED, the example names `ratatoskr_x` and no role file exists.
- [x] 4.2 Edit the example, add `deploy/postgres/role.sql` and the README deployment paragraph.

## 5. Documentation (R3-19)

- [x] 5.1 Rewrite the README paragraph that says no workspace pin exists from the code and the workspace as they are; documentation has no meaningful RED.

## 6. Gate

- [x] 6.1 Run the full local gate (`cargo fmt`, `cargo clippy`, the 850-line check, `cargo test --workspace --locked`, `cargo deny --locked check`, `openspec validate --all --strict`, `openspec validate --archived`).
