# Bootstrap service scaffold

## Why

The repository is architecture bootstrap: every document describes intent and no code exists. Implementation plan item 1 of `docs/IMPLEMENTATION_PLAN.md` calls for the first runnable vertical slice so that OAuth, synchronization, and normalization work in later items lands on real workspace conventions, a working build-and-test gate, and an owned database schema instead of prose alone.

## What Changes

- Add a Rust/Tokio cargo workspace laid out like the sibling services: libraries under `crates/`, one deployable binary under `services/x`.
- Add typed finite configuration loaded through figment from `RATATOSKR__`-prefixed environment variables with unknown-key rejection and semantic validation.
- Add structured telemetry: tracing-subscriber registry with JSON or pretty stdout output and a Prometheus metrics recorder installed once behind a shutdown-aware guard.
- Add typed error types (thiserror) covering configuration, telemetry, persistence, and bootstrap failures, including exit code 78 for rejected configuration.
- Add admin HTTP endpoints on axum: `/health/live`, `/health/ready`, `/metrics`, `/version`, with lifecycle-aware readiness and no-store caching.
- Add the first-version `x_archive` PostgreSQL schema in a root `schema.sql`, applied in place under an advisory lock; there are no migrations by development-status rule.
- Add a disposable-database test harness behind a `test-support` feature so integration tests create isolated databases from `schema.sql`.
- Add product CI (`.github/workflows/ci.yml`) beside the unchanged OpenSpec checks, plus the lint/toolchain/deny configuration fleet policy requires (`rust-toolchain.toml`, `clippy.toml`, `rustfmt.toml`, `deny.toml`).
- Update `DEVELOPMENT.md` with the exact local gate commands and move the README status line from "no implementation exists" to "scaffold implemented".

Out of scope: OAuth, X API clients, bookmark synchronization, post normalization, NATS eventing, legacy import.

## Capabilities

### New Capabilities

- `service-bootstrap` - how the service process starts, accepts or rejects configuration, emits telemetry, exposes liveness/readiness/metrics/version, and reports failure classes.
- `x-archive-schema` - the owned `x_archive` database schema: what objects `apply_schema` creates, that application is idempotent and migration-free, and that tests can build disposable databases from it.

### Modified Capabilities

None. `openspec/specs/` is empty; this change creates the first local capabilities.

## Impact

- New code: workspace manifest, `crates/x-core`, `crates/x-telemetry`, `crates/x-persistence`, `crates/x-http`, `services/x`, root `schema.sql`.
- New configuration files required by fleet policy in the same change as the first `Cargo.toml`: `clippy.toml`, `.github/workflows/ci.yml`; plus `rust-toolchain.toml`, `rustfmt.toml`, `deny.toml`.
- `DEVELOPMENT.md` gains the product gate command list; README status text changes to match reality.
- New dependencies: tokio, axum (default features off), sqlx (postgres, no migrate feature), figment, serde, tracing/tracing-subscriber, metrics + metrics-exporter-prometheus, thiserror, uuid.
- No cross-repository contract changes; nothing is published to other repositories yet.
