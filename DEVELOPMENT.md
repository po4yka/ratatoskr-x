# Developing Ratatoskr X

> Status: Active. Last reviewed: 2026-08-25

The first scaffold is implemented: a Rust workspace with typed configuration, structured telemetry, process-state endpoints, and the first-version `x_archive` schema. The official OAuth 2.0 Authorization Code connection with PKCE — encrypted credential envelopes, rotation-aware refresh with reuse detection, revocation, scope auditing — and classed durable per-account API budget gates are implemented. Official-payload post normalization, complete bookmark snapshots with durable checkpoints, atomic authority, and observed removals, safe frequent partial bookmark scans with watermarks, bounded budget use, gap escalation, and full-snapshot drift repair evidence, native folder-membership snapshots, the owner-scoped explicit browser-capture lane (NATS consumer, public app-only resolution worker, exactly-once operation reports), complete-envelope outbox facts with one JetStream relay, linked-article report consumption, exact Knowledge completion linkage, the bounded compliance revalidation/takedown application service, and separately consented idempotent bookmark add/remove with an official HTTP adapter are implemented. No external runtime/API/UI surface invokes write-back; the compliance HTTP adapter, periodic scheduler, Knowledge completion consumption, and legacy importer are also not wired.

## Toolchain

Rust/Tokio pinned by `rust-toolchain.toml` (1.97.0), axum for the admin listener, SQLx/PostgreSQL without the migrate feature — `schema.sql` is edited in place while development status forbids migrations. Reqwest/Rustls carries the OAuth token adapter behind recorded provider fixtures served by WireMock in tests; AES-256-GCM encrypts credentials under an environment-provided key; pure normalization of official API payloads lives in `crates/x-normalize` with no I/O dependencies; NATS JetStream and testcontainers arrive with the changes that need them.

## Code size limits

`clippy.toml` beside the manifest carries the limits: msrv 1.97, function-length threshold 100, argument threshold 7, nesting threshold 5, and a disallowed-methods rule that reserves `std::env::var` for `x_core::config`. The one limit clippy cannot express — file length — is the 850-line ratchet step in `.github/workflows/ci.yml`. The numbers follow `ratatoskr-workspace/docs/QUALITY_GATES.md`; each sits at the worst case the sibling trees measured, so a regression fails and finished work does not.

## The gate

Run it locally exactly as `.github/workflows/ci.yml` runs it. Integration tests need a PostgreSQL 17 cluster with ICU collation; start one disposable container and export its URL:

```bash
docker run -d --name ratatoskr-x-pg -p 5432:5432 \
  -e POSTGRES_USER=x -e POSTGRES_PASSWORD=x -e POSTGRES_DB=x \
  -e POSTGRES_INITDB_ARGS="--locale-provider=icu --icu-locale=und-x-icu --encoding=UTF8" \
  postgres:17
export X_TEST_DATABASE_URL=postgres://x:x@127.0.0.1:5432/x
```

### Rust — also the CI gate

```bash
cargo fetch --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --locked
cargo test --workspace --locked
cargo build --workspace --locked --release
```

`cargo deny --locked check` runs in its own `deny` job in the same workflow, not in the gate above,
so a new RustSec advisory cannot hide a clippy or test failure behind it.

### Broker fixture and public-capture credential

The X service needs two Edge-preprovisioned durables: `ratatoskr_x_browser_capture` on `ratatoskr_commands`, filtered to `cmd.x.capture.requested.v1`, and `ratatoskr_x_extractor_reports` on `ratatoskr_events`, filtered to `evt.platform.operation.reported.v1` with explicit acks and a 30 second ack wait. In deployment configure `RATATOSKR__BUS__URL` and the absolute `RATATOSKR__BUS__NKEY_SEED_PATH`; do not put an NKey seed in an environment value or a URL. The service only opens and validates the durables, so a local fixture must create both streams and both durables before starting it; `services/x/tests/smoke.rs` shows the exact configuration.

The service also refuses to start (exit code 78) without `RATATOSKR__PUBLIC_CAPTURE__BEARER_TOKEN_PATH`, the absolute path of a file holding an app-only X API bearer token. The token is external: an X developer app with read access must be provisioned by the owner. Tests never need it; they serve the X API from `wiremock`.

The operator listener defaults to `127.0.0.1:9087` (host only). Port `8080` is the Edge public API.

The command list above and the `gate` job's `run:` steps are one list by rule; a step in `.github/workflows/ci.yml` diffs them and fails on drift, treating this document as the wrong side.

### OpenSpec checks, unchanged

```bash
git diff --check
openspec validate --all --strict
openspec validate --archived
```

`.github/workflows/openspec.yml` runs the two OpenSpec commands in CI alongside product CI.

## Workflow

1. Confirm documented provider capability, scope, rate/credit cost, and read/write consent.
2. Keep post, bookmark, folder membership, and local Ratatoskr collection semantics separate.
3. Name timestamps as observations unless the provider supplies authoritative save time.
4. Never infer removal from a partial page scan.
5. Test pagination, interruption, redelivery, rate limits, compliance state, and write idempotency.

Default CI uses synthetic fixtures and never personal X credentials.

## What a clone needs before you plan a change

A change is planned with OpenSpec, which is a CLI a clone installs for itself. Use the version
`.github/workflows/openspec.yml` pins, so your terminal and the gate answer the same:

```bash
npm install --global @fission-ai/openspec@1.10.0
```

Cross-repository behaviour lives in a store, and registering one is per-machine state that no
repository can turn on for you — the same kind of step as `git config core.hooksPath .githooks`:

```bash
git clone git@github.com:po4yka/ratatoskr-workspace.git <path>
openspec store register <path> --id ratatoskr-workspace
```

`openspec doctor` reports whether both are in place.

## The Rust skills in this repository

`.agents/skills/` holds eighteen Rust skills vendored from `po4yka/rust-skills`, and
`.claude/skills/` symlinks to them. Unlike the steps above this needs nothing from your machine: the
files are in the tree, so a fresh clone already has them.

Update them with the catalogue and never by hand:

```bash
npx skills update
```

That rewrites `.agents/skills/` and `skills-lock.json` from the catalogue. Run it in one repository,
read the diff, then apply the same change to every Ratatoskr repository whose stack is Rust.
`ratatoskr-workspace/.github/workflows/drift.yml` fails when one copy differs from the others.
