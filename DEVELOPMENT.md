# Developing Ratatoskr X

> Status: Active. Last reviewed: 2026-08-25

The first scaffold is implemented: a Rust workspace with typed configuration, structured telemetry, process-state endpoints, and the first-version `x_archive` schema. The official OAuth 2.0 Authorization Code connection with PKCE — encrypted credential envelopes, rotation-aware refresh with reuse detection, revocation, scope auditing — and the durable per-account API budget gate are implemented. Official-payload post normalization, complete bookmark snapshots with durable checkpoints, atomic authority, and observed removals, and safe frequent partial bookmark scans with watermarks, bounded budget use, gap escalation, and full-snapshot drift repair evidence are implemented. The HTTP adapter, folder synchronization, and legacy importer are not.

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
cargo deny --locked check
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --locked
cargo test --workspace --locked
cargo build --workspace --locked --release
```

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
