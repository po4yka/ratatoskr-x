# Developing Ratatoskr X

> Status: Proposed  
> Last reviewed: 2026-08-17

Architecture bootstrap: OAuth, X API client, schema, synchronization, and legacy importer are not implemented.

## Intended toolchain

Rust/Tokio, Reqwest/Rustls, OAuth 2.0 PKCE, SQLx/PostgreSQL, encrypted credentials, NATS JetStream, provider fixtures/WireMock, tracing, and testcontainers.

## Workflow

1. Confirm documented provider capability, scope, rate/credit cost, and read/write consent.
2. Keep post, bookmark, folder membership, and local Ratatoskr collection semantics separate.
3. Name timestamps as observations unless the provider supplies authoritative save time.
4. Never infer removal from a partial page scan.
5. Test pagination, interruption, redelivery, rate limits, compliance state, and write idempotency.

The first scaffold PR must document exact commands. Default CI uses synthetic fixtures and never personal X credentials.
