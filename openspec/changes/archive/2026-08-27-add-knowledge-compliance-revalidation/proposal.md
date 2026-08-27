## Why

X social-source facts can now start real Knowledge analysis, but the X archive does not yet retain
completion linkage or enforce upstream takedown obligations after capture. Plan item 8 closes that
loop without moving Knowledge-owned analyses, embeddings, or search documents into this bounded
context.

## What Changes

- Treat the existing replay-safe `social.source.captured.v1` and `social.source.updated.v1` outbox
  facts as Knowledge requests and preserve one request per source content digest.
- Consume `knowledge.analysis.completed.v1` idempotently and link it to the exact locally retained
  `(social_source_id, content_digest)` revision without storing Knowledge-private identifiers.
- Add bounded compliance revalidation that records every provider observation in an append-only
  ledger and distinguishes authoritative takedowns from indeterminate failures.
- On an authoritative takedown, atomically mark the source unavailable, retain a scoped tombstone,
  and enqueue one `social.source.removed.v1` deletion request for Knowledge-derived analyses,
  embeddings, and search projections.
- Document the cross-repository contract, periodic scheduling boundary, retention behavior, and
  takedown path. No new shared event type or Knowledge-internal behavior is introduced.

## Capabilities

### New Capabilities

- `knowledge-analysis-linkage`: Replay-safe Knowledge request and completion linkage for X social
  source revisions under the workspace `social-analysis-intake` contract.
- `compliance-revalidation`: Bounded upstream authorization checks, durable ledger evidence, and
  atomic tombstone-to-Knowledge-deletion propagation.

### Modified Capabilities

- `x-archive-schema`: Add the owned analysis-link and compliance-ledger state plus source-removal
  fields required by the new flows, editing the first-version schema in place.

## Impact

- Affects `crates/x-sync`, `schema.sql`, persistence/schema integration tests, README and
  architecture documentation.
- Uses the already pinned `ratatoskr-social-contracts` completion and removal payloads and the
  workspace `social-analysis-intake` specification; there is no contract version change and no
  new production dependency.
- Knowledge remains the owner of analysis output, embeddings, and search documents. X stores only
  privacy-safe completion linkage and publishes the existing removal fact as the deletion request.
- Revalidation consumes bounded official-provider requests and therefore shares the existing
  account/global rate and cost budgets when wired into runtime scheduling.
