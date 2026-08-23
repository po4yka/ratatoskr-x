# Developing Ratatoskr X

> Status: Proposed  
> Last reviewed: 2026-08-20

Architecture bootstrap: OAuth, X API client, schema, synchronization, and legacy importer are not implemented.

## Intended toolchain

Rust/Tokio, Reqwest/Rustls, OAuth 2.0 PKCE, SQLx/PostgreSQL, encrypted credentials, NATS JetStream, provider fixtures/WireMock, tracing, and testcontainers.

## Code size limits

There is no code here yet, so no limit is enforced yet. The commit that brings the first manifest brings the configuration that carries the limits with it: `clippy.toml` beside a `Cargo.toml`, `eslint.config.js` beside a `package.json`. `fleet.yml` fails the gate when a manifest arrives without one, so the rule has a check behind it and not only this paragraph.

`ratatoskr-workspace/docs/QUALITY_GATES.md` holds the numbers the repositories with code use today, the command that measured each one, and the limits that were rejected with the reason. Read it before you choose numbers, then measure this tree. Each limit is set at the worst case the tree already has, so that the check fails on a regression and not on work that has not been done yet.

## Current validation

This repository has no product manifest or `.github/workflows/ci.yml` yet. Run the current docs-only
gate locally:

```bash
git diff --check
openspec validate --all --strict
openspec validate --archived
```

`.github/workflows/openspec.yml` runs the two OpenSpec commands in CI. The first-manifest rule in
`.github/workflows/fleet.yml` requires the first product manifest to add a product `ci.yml` that
invokes a test. For a Rust or Node manifest, it also requires `clippy.toml` or
`eslint.config.js`, respectively; it does not prove that product CI invokes the linter. The
docs-only/OpenSpec gate remains in addition to product CI.

## Workflow

1. Confirm documented provider capability, scope, rate/credit cost, and read/write consent.
2. Keep post, bookmark, folder membership, and local Ratatoskr collection semantics separate.
3. Name timestamps as observations unless the provider supplies authoritative save time.
4. Never infer removal from a partial page scan.
5. Test pagination, interruption, redelivery, rate limits, compliance state, and write idempotency.

The first scaffold PR must document exact commands. Default CI uses synthetic fixtures and never personal X credentials.

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
