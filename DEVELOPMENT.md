# Developing Ratatoskr X

> Status: Proposed  
> Last reviewed: 2026-08-20

Architecture bootstrap: OAuth, X API client, schema, synchronization, and legacy importer are not implemented.

## Intended toolchain

Rust/Tokio, Reqwest/Rustls, OAuth 2.0 PKCE, SQLx/PostgreSQL, encrypted credentials, NATS JetStream, provider fixtures/WireMock, tracing, and testcontainers.

## Code size limits

There is no code here yet, so no limit is enforced yet. The commit that brings the first manifest brings the configuration that carries the limits with it: `clippy.toml` beside a `Cargo.toml`, `eslint.config.js` beside a `package.json`. `fleet.yml` fails the gate when a manifest arrives without one, so the rule has a check behind it and not only this paragraph.

`ratatoskr-workspace/docs/QUALITY_GATES.md` holds the numbers the repositories with code use today, the command that measured each one, and the limits that were rejected with the reason. Read it before you choose numbers, then measure this tree. Each limit is set at the worst case the tree already has, so that the check fails on a regression and not on work that has not been done yet.

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
