# Tasks: bootstrap-service-scaffold

Every behaviour task is a pair: the first task adds a test that fails for the stated assertion (minimal compiling stubs are allowed so the failure is an assertion, not a compile error), the second makes it pass. A task that cannot start from a failing test states why in one line.

## 1. Workspace foundation

- [x] 1.1 Create the cargo workspace: root `Cargo.toml` (resolver `"3"`, `[workspace.package]` 0.1.0/edition 2024/rust-version 1.97/BSD-3-Clause/repository ratatoskr-x, exact-pinned `[workspace.dependencies]`, shared `[workspace.lints]` block, `[profile.release] debug = 1`), `rust-toolchain.toml` (1.97.0, minimal, clippy+rustfmt), `clippy.toml` (msrv 1.97, allow-unwrap/expect/indexing-slicing-in-tests, thresholds 100/7/5, disallowed `std::env::var`/`var_os` outside `x_core::config`), `rustfmt.toml` (2024/100/Unix), `deny.toml` (sibling skeleton), and member skeletons `crates/{x-core,x-telemetry,x-persistence,x-http}` + `services/x` each with `[lints] workspace = true`. Verify with `cargo metadata --no-deps --format-version 1` exiting 0 and listing five members. Cannot start from a failing test: project scaffolding and toolchain configuration files.
- [x] 1.2 Add `.github/workflows/ci.yml` with SHA-pinned actions, `permissions: contents: read`, `persist-credentials: false`, steps `cargo fetch --locked`, `cargo deny --locked check`, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo build --workspace --locked`, `cargo test --workspace --locked`, `cargo build --workspace --locked --release`, plus a digest-pinned `postgres:17` service container with ICU `initdb` args exporting `X_TEST_DATABASE_URL`. Verify the file contains a literal `cargo test` invocation and `openspec validate --all --strict` still passes. Cannot start from a failing test: CI workflow definition.

## 2. Typed finite configuration (`x-core`)

- [x] 2.1 RED: add `crates/x-core/tests/config.rs` with `unknown_environment_key_is_refused`: given `RATATOSKR__UNKNOWN__KEY=1`, `load()` must return `Err` whose operator report names the refused key. Stub `load()` to return defaults ignoring the environment; run `cargo nextest`-equivalent `cargo test -p ratatoskr-x-core --test config` and confirm the failure is the missing refusal (Ok returned), not a compile error.
- [x] 2.2 GREEN: wire `Figment::from(Serialized::defaults(...)).merge(Env::prefixed("RATATOSKR__").split("__"))` with `#[serde(deny_unknown_fields)]` config structs and `ConfigError::Source` value-free reporting; the test passes.
- [x] 2.3 RED: add test `absent_variables_yield_documented_defaults` asserting the loaded config equals the documented defaults (listen `127.0.0.1:8080`, JSON format, filter `info`, max connections 5); make it fail first by stubbing defaults away from the declared model.
- [x] 2.4 GREEN: complete the typed model and its `Default` so defaults flow through figment; the test passes.
- [x] 2.5 RED: add test `invalid_values_report_all_violations_together`: two violating overrides (empty database URL, zero max connections) must yield one `Err` whose violation list length is 2; make it fail with a validation stub that returns an empty violation list.
- [x] 2.6 GREEN: implement semantic `validate()` collecting every violation into `ConfigError::Invalid(Vec<Violation>)`; the test passes.
- [x] 2.7 RED: add test `config_rejection_maps_to_exit_code_78` asserting `ConfigError::exit_code() == 78`; make it fail with a stub returning 1.
- [x] 2.8 GREEN: implement `exit_code()` returning 78 for both `Source` and `Invalid`; the test passes.

## 3. Structured telemetry (`x-telemetry`)

- [x] 3.1 RED: add `crates/x-telemetry/tests/telemetry.rs` with `unparsable_filter_returns_typed_error`: installing with filter `"lvl^[["` must return `Err(TelemetryError::Filter(..))`; make it fail with a stub that accepts any filter and returns `Ok`.
- [x] 3.2 GREEN: parse the filter with `EnvFilter::try_new` mapped to the typed error; the test passes.
- [x] 3.3 RED: add `json_events_render_message_and_level_to_captured_writer`: build telemetry with JSON format over an injected shared writer, emit `info!("bootstrap smoke")` under a thread-local dispatcher (`with_default`), and assert the captured line parses as a JSON object whose message is `bootstrap smoke` and level is `INFO`; make it fail with a stub capturing nothing.
- [x] 3.4 GREEN: assemble the registry + filter + `fmt` JSON layer writing through the injected `MakeWriter`; the test passes.
- [x] 3.5 RED: add `prometheus_recorder_renders_described_counter`: after building telemetry, increment a described counter `ratatoskr_x_bootstrap_total` by 1 and assert the guard's metrics render output contains it; make it fail while the stub provides no recorder.
- [x] 3.6 GREEN: install the Prometheus recorder in the guard (`PrometheusBuilder::install_recorder`) and expose `render()`; the test passes.

## 4. `x_archive` schema and persistence (`x-persistence`)

Integration tests in this section require a reachable PostgreSQL at `X_TEST_DATABASE_URL` (CI service container; locally the documented docker command).

- [x] 4.1 RED (test lives at `services/x/tests/harness.rs` where the dev-dependency enables the feature): add the harness test first drafted for `crates/x-persistence/tests/schema.rs`: `test_database_harness_creates_and_removes_isolated_database`: `TestDatabase::create()` must succeed, expose a unique `<prefix>_test_` name, serve a trivial query against the applied schema, and after `cleanup()` a catalog probe must show the database gone; make it fail with a stub whose `create()` returns a connect error.
- [x] 4.2 GREEN: implement the `#[cfg(feature = "test-support")]` harness (admin URL from `X_TEST_DATABASE_URL`, unique database from `template0` with ICU options, own pool, explicit `cleanup()` dropping with `FORCE`); the test passes with an intentionally empty schema application.
- [x] 4.3 RED: add `fresh_database_receives_full_owned_inventory` asserting the `x_archive` table set queried from `information_schema` equals the fifteen owned tables exactly; make it fail because the stub applies nothing.
- [x] 4.4 GREEN: add root `schema.sql` creating the fifteen owned tables with placeholder-honest columns and embed it in `x-persistence` via `include_str!`, applying it in one transaction guarded by a constant `pg_advisory_xact_lock`; both harness tests pass.
- [x] 4.5 RED: add `reapplication_is_idempotent` applying the schema twice and asserting the second application succeeds with a byte-identical catalog-visible table set; make it fail because plain `CREATE TABLE` rejects the second pass. Exact-set equality in these tests also enforces that no migration bookkeeping table ever appears; a separate failing test is impossible because the `sqlx` migrate feature is absent by construction.
- [x] 4.6 GREEN: make application idempotent (`IF NOT EXISTS` objects, advisory lock retained); the test passes.
- [x] 4.7 GUARD, cannot start from a failing test because the schema written in 4.4 already enforces the boundary by construction: add `constraints_stay_within_x_archive_boundary` asserting every foreign key in `x_archive` references a table in `x_archive` and provider identity columns carry their declared uniqueness; make it fail because the placeholder schema declares neither yet.
- [x] 4.8 VERIFIED instead of implemented: the intra-schema foreign keys and provider-identity uniqueness constraints already exist in `schema.sql`; the guard passes.

## 5. Process-state endpoints (`x-http`)

- [x] 5.1 RED: add `crates/x-http/tests/admin.rs` with `live_reports_running_state`: `oneshot(GET /health/live)` must return 200 with body state `live`; make it fail with an empty router returning 404.
- [x] 5.2 GREEN: implement `admin_router` with the `/health/live` handler and lifecycle state; the test passes.
- [x] 5.3 RED: add `ready_tracks_initialization_and_names_checks`: before granting readiness `/health/ready` returns 503 naming the unmet `database` check, after granting it returns 200 reporting ready; make it fail against the current router.
- [x] 5.4 GREEN: implement named readiness checks and the atomic lifecycle transition; the test passes.
- [x] 5.5 RED: add `metrics_serves_prometheus_exposition`: with one recorded counter, `GET /metrics` responds with Prometheus text exposition content type and a body containing the counter; make it fail with a stub renderer emitting an empty body.
- [x] 5.6 GREEN: inject the real renderer closure from the telemetry handle; the test passes.
- [x] 5.7 RED: add `version_identifies_service_build`: `GET /version` reports service `ratatoskr-x`, the crate version, git sha fallback `unknown`, and the compiler version; make it fail with a stub returning wrong fields.
- [x] 5.8 GREEN: implement identity constants in `x-core` and the version handler; the test passes.
- [x] 5.9 RED: add `state_endpoints_disable_caching` asserting every one of the four endpoints answers with `Cache-Control: no-store`; make it fail while headers are unset.
- [x] 5.10 GREEN: add the `map_response` no-store middleware; the test passes.

## 6. Service binary bootstrap (`services/x`)

- [x] 6.1 RED: add `services/x/tests/bootstrap.rs` with `subsystem_errors_map_to_distinct_exit_codes`: configuration failure maps to 78 while telemetry, persistence, and listener failures map to a nonzero code different from 78, each rendering a static subsystem message; make it fail with a `BootstrapError` stub that has no variants.
- [x] 6.2 GREEN: implement the typed `BootstrapError` aggregation with `From` conversions and `exit_code()`; the test passes.
- [x] 6.3 RED: add `services/x/tests/smoke.rs` with `service_serves_health_endpoints_until_sigterm`: launch the compiled binary (`env!("CARGO_BIN_EXE_ratatoskr-x")`) with valid config pointing at a harness database, wait for the startup event carrying the bound address, request `/health/live` (expect 200) and `/version` (expect service `ratatoskr-x`), send SIGTERM, and expect exit status success; make it fail because the stubbed binary exits immediately without serving.
- [x] 6.4 GREEN: implement `main` bootstrap order (config -> exit 78 on rejection -> telemetry guard -> database connect + schema apply -> bind listener -> publish startup event -> grant readiness -> SIGINT/SIGTERM graceful drain -> clean shutdown exit); the test passes.

## 7. Gate, documentation, archive

- [ ] 7.1 Update `DEVELOPMENT.md`: document the product gate commands identical to `ci.yml`, the local disposable Postgres docker command exporting `X_TEST_DATABASE_URL`, and keep the unchanged OpenSpec checks listed beside them. Cannot start from a failing test: developer documentation.
- [ ] 7.2 Update the README status paragraph to state that the service scaffold, health endpoints, typed config, telemetry, and first-version `x_archive` schema exist while OAuth, synchronization, and normalization remain unimplemented. Cannot start from a failing test: documentation.
- [ ] 7.3 Run the full local gate with a disposable Postgres container and capture evidence: `git diff --check`, `openspec validate --all --strict`, `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo build --workspace --locked`, `cargo test --workspace --locked`, `cargo deny --locked check`, `cargo build --workspace --locked --release`.
- [ ] 7.4 With every box above ticked, commit the branch (Conventional Commits), archive the change via OpenSpec, and verify `openspec validate --archived`.
- [ ] 7.5 Integrate: merge the branch into `main`, push `main` to the remote, delete the worktree and the task branch.
