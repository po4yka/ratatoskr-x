# Design: bootstrap-service-scaffold

## Context

The repository holds intent documents only; the first code must arrive together with its own product CI and lint configuration (fleet policy ties them to the first manifest commit). Sibling services (`ratatoskr-platform`, `ratatoskr-extractor`, `ratatoskr-knowledge`) already encode the workspace conventions this repository is expected to match. Development status is binding: one API/database version, no migrations, product name Ratatoskr.

## Goals / Non-Goals

Goals: a workspace indistinguishable in conventions from the newest siblings (extractor, knowledge); every spec scenario above backed by a named test; a local gate identical to CI.

Non-Goals: OAuth, X API clients, sync workers, normalization, NATS eventing, OTLP trace export (deferred until eventing exists to propagate contexts), legacy import.

## Decisions

### D1. Workspace shape

`crates/{x-core, x-telemetry, x-persistence, x-http}` plus `services/x`, resolver `"3"`, `[workspace.package]` version 0.1.0 / edition 2024 / rust-version 1.97 / BSD-3-Clause / repository `https://github.com/po4yka/ratatoskr-x`. Members consume shared deps via `<dep>.workspace = true` and inherit `[lints] workspace = true`. Alternative considered: fewer crates (single lib + bin) rejected because platform/extractor separate config/telemetry/persistence/http concerns and later plan items grow persistence and protocol code heavily.

Crate responsibilities:

- `x-core`: identity constants (`SERVICE_NAME = "ratatoskr-x"`, `VERSION`, `GIT_SHA` via `option_env!("RATATOSKR_GIT_SHA")`, `RUST_VERSION`), typed config, violation reporting, `ConfigError`.
- `x-telemetry`: subscriber installation behind a guard that owns the Prometheus recorder handle; explicit `shutdown()`, never Drop-driven.
- `x-persistence`: pool construction, `apply_schema` under `pg_advisory_xact_lock`, `PersistenceError`, and the `#[cfg(feature = "test-support")]` disposable-database harness.
- `x-http`: admin router, lifecycle state, response DTOs; receives the metrics renderer as a closure so it does not depend on the exporter.
- `services/x`: bootstrap order config -> telemetry -> database -> listener -> readiness -> signal-driven drain.

### D2. Exact-pinned dependencies

All external dependencies pinned `=x.y.z` (extractor/knowledge style, the current majority). Initial set: tokio 1.53.1, axum 0.8.9 (`default-features = false`, features `http1`,`tokio`), tower 0.5.3 (`util`; dev-only `ServiceExt::oneshot` testing), sqlx 0.8.6 (`runtime-tokio`,`tls-rustls-ring`,`postgres`,`uuid`; deliberately NO `migrate` and no `macros`, so builds need no live `DATABASE_URL`), figment 0.10.19 (`env`), serde 1.0.229, tracing 0.1.44, tracing-subscriber 0.3.23 (`fmt`,`json`,`registry`,`env-filter`,`ansi`,`std`), metrics 0.24.6, metrics-exporter-prometheus 0.18.3 (`default-features = false`), thiserror 2.0.20, uuid 1.24.1 (`v7`,`serde`). No `anyhow` anywhere; contracts dependency intentionally absent until events exist (D7).

### D3. Configuration model (figment)

`Figment::from(Serialized::defaults(XConfig::default())).merge(Env::prefixed("RATATOSKR__").split("__"))` then `extract::<XConfig>()`. Struct tree `#[serde(deny_unknown_fields)]`: `admin.listen_addr` (default `127.0.0.1:8080`), `database.url` (`secrecy`-free plain `String`, never logged; secrets handling arrives with credentials storage in plan item 2), `telemetry.log_format` (`Json | Pretty`, default Json), `telemetry.log_filter` (default `info`), `database.max_connections` (default 5). Semantic validation returns `Vec<Violation>` (empty URL, zero max connections, unparseable listen address); `ConfigError::{Source, Invalid}` renders value-free operator text. Alternatives: hand-rolled parser (knowledge) rejected - figment is the majority convention and gives unknown-key refusal for free.

### D4. Telemetry

Registry + parsed `EnvFilter` + one `fmt` layer (`.json()` or `.pretty()`) writing stdout. Installation function takes a generic writer so tests capture output into a shared buffer and assert JSON structure; the production wrapper fixes stdout. `PrometheusBuilder` installs a recorder once, returning a handle whose `render()` feeds `/metrics`. Guard holds the handle; double installation returns `TelemetryError::AlreadyInstalled`. No OpenTelemetry layer yet (see Non-Goals).

### D5. Errors and exit codes

thiserror everywhere, `#[non_exhaustive]`, static messages, `#[source]` chains, no value interpolation. Mapping: configuration rejection -> 78 (`EX_CONFIG`); telemetry/persistence/bootstrap failures -> 1. `BootstrapError` in `services/x` aggregates subsystem errors and exposes `exit_code()`. No stringly-typed failures: handlers convert typed errors once at the edge.

### D6. Schema and persistence

Root `schema.sql` embedded via `include_str!("../../../schema.sql")` in `x-persistence`, applied inside one transaction guarded by a constant advisory lock key, statements executed through `sqlx::Executor::execute` on the transaction (no migration runner, honoring development status). Table inventory follows the README planned model exactly: `accounts`, `credentials`, `users`, `posts`, `post_relations`, `media`, `bookmarks`, `bookmark_folders`, `bookmark_folder_items`, `sync_runs`, `snapshots`, `rate_limit_state`, `tombstones`, `outbox_events`, `inbox_events` - resolving the AGENTS.md conceptual `x_outbox`/`x_inbox` wording in favor of the concrete names both README and DATA_MODEL agree on. All columns are honest placeholders (uuid v7 primary keys via `gen_random_uuid()` where the model does not dictate otherwise, `timestamptz` audit columns, observation timestamp names `first_observed_saved_at` / `last_observed_saved_at` / `observed_removed_at`, availability state enforced by CHECK). Uniqueness declared where provider identity is known (provider user/post/folder IDs). Test harness: `TestDatabase::create()` connects to `X_TEST_DATABASE_URL` (default `postgres://x:x@127.0.0.1:5432/x`), creates `<prefix>_test_<uuid>` from `template0` with ICU locale matching CI `initdb` args, applies the schema, and `cleanup()` drops with `FORCE`.

### D7. Cross-repo contracts

No `ratatoskr-contracts` dependency yet. Events begin in plan item 7; adding the git-rev pin then keeps this scaffold free of unused graph edges.

## Risks / Trade-offs

- [Placeholder columns may not match eventual normalization needs] -> Acceptance explicitly allows placeholders; later items edit `schema.sql` in place while development status holds.
- [Integration tests need live PostgreSQL] -> CI runs a digest-pinned `postgres:17` service container with ICU init args; locally the gate documents the docker command. Harness failure produces a clear connect error, not a silent pass.
- [Exact pins make routine bumps manual] -> Matches sibling policy; Dependabot already configured in-repo manages updates.
- [Global subscriber state makes telemetry tests order-sensitive] -> Writer-injection tests avoid installing global state; only the production wrapper touches the process-global subscriber.

## Migration Plan

Not applicable: no data exists, no migrations exist, nothing is deployed. Rollback is branch deletion.

## Open Questions

None.
