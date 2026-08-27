## Why

Normalized X posts currently remain only in the X archive. Downstream consumers cannot observe a
durable SocialSource projection, and a linked public article cannot be tied safely back to every
post that referenced it.

## What Changes

- Pin `ratatoskr-social-contracts` to a reviewed ratatoskr-contracts revision and validate its
  committed event fixtures without requiring a crates.io release.
- Publish exactly one durable `social.source.captured.v1` or `social.source.updated.v1` event for
  an account-scoped source transition, using the contract snapshot and truthful bookmark-snapshot
  or explicit-capture acquisition provenance.
- Persist normalized eligible external links as account-owned article captures, deduplicate one
  capture request across all posts with the same canonical URL, and link every originating post.
- Publish a capture command only after the capture and all originating links commit atomically;
  consume correlated extractor outcomes idempotently and retain the returned Document IR BlobRef
  against every linked post.

## Capabilities

### New Capabilities

- `social-source-publication`: durable account-scoped SocialSource transitions that conform to the
  pinned shared contract.
- `linked-article-capture`: conservative external-link capture, cross-post deduplication, and
  extractor outcome linkage.

### Modified Capabilities

- `x-archive-schema`: adds the current-schema records and constraints needed for source and
  article-capture provenance, without migrations.

## Impact

- Affected code: workspace dependency pinning, X persistence/schema, synchronization composition,
  transactional outbox/inbox handling, and focused integration tests.
- Affected systems: ratatoskr-contracts is consumed by exact revision; ratatoskr-extractor receives
  the existing capture command and its terminal events are consumed by X.
- No provider write scope, article fetching, extractor internals, analysis, collections, or schema
  migration mechanism is added.
