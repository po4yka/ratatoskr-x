## Context

See `proposal.md` for motivation. X already commits normalized source revisions and their
`social.source.captured.v1` or `social.source.updated.v1` outbox facts atomically. The workspace
`social-analysis-intake` spec makes those facts the Knowledge requests and defines completion
linkage as `(social_source_id, content_digest)`. The pinned social contracts already contain both
`knowledge.analysis.completed.v1` and `social.source.removed.v1`.

The repository has no message-broker runtime yet, so this change extends the durable producer and
consumer application services used by that future runtime rather than inventing a second transport.
Database schema changes edit `schema.sql` in place; there are no migrations while development status
holds.

## Goals / Non-Goals

**Goals:**

- Close the X-owned half of the analysis request/completion loop with exact revision linkage.
- Provide a callable due-driven compliance worker seam whose upstream adapter uses official API
  authorization and whose calls are bounded by the existing request budget.
- Make a compliance takedown one database transaction from observation evidence through tombstone
  and Knowledge deletion outbox fact.
- Preserve enough non-sensitive evidence to audit why and when derived data was removed.

**Non-Goals:**

- Running analysis, embedding content, ranking search results, or reading Knowledge storage.
- Implementing NATS transport or a process scheduler before the repository's runtime owns those
  facilities.
- Adding a new shared contract, contract version, database migration, UI, or provider write scope.
- Defining provider-specific HTTP response parsing; the adapter seam returns classified official
  observations and bounded failures.

## Decisions

### D1: Existing social-source facts remain the only analysis request

No `knowledge.analysis.requested` command is added. The source revision row and its state-carried
outbox fact are the durable request record. Concurrent publication is serialized on the account and
post identity before the existing revision/digest check, so concurrent retries converge instead of
racing unique constraints. Alternative: a separate request table or command, rejected because it
duplicates the agreed cross-repository identity and ordering contract.

### D2: Completion linkage is an X-owned revision foreign key, not a Knowledge identifier

`knowledge_analysis_links` stores the inbound event identity, exact source identity and digest, and
completion instant. A composite foreign key targets `social_source_revisions`; currentness is derived
by comparing the linked digest with `social_sources.current_content_digest`. Both current and
historical links survive source edits. Alternative: store Knowledge run/search-document IDs,
rejected because those are private implementation identities and absent from the agreed payload.

The consumer decodes a complete event envelope, requires producer `ratatoskr-knowledge`, checks the
typed payload, owner, source, digest, and non-removed state before it claims the common inbox row.
The inbox receipt and link commit together. This makes malformed or cross-tenant events retryable
after correction rather than permanently acknowledged.

### D3: Due selection and provider classification are separate seams

The compliance service selects at most the caller's bound of unremoved account sources whose latest
ledger time precedes `due_before`. It reserves one unit from `BudgetGate` immediately before each
provider call. The provider seam returns either a classified official observation with an optional
request ID, or one closed indeterminate failure class. It never exposes credentials or raw bodies to
the domain service.

This is a due-driven worker rather than an internal timer. The eventual runtime scheduler can invoke
it periodically without putting detached task ownership, credentials, or HTTP concerns into the
domain module. Alternative: spawn a timer inside `x-sync`, rejected because service lifecycle and
message scheduling do not exist in this bootstrap phase.

### D4: Every attempted check is ledgered; only authoritative states remove

Available and authoritative unavailable observations append an entry to
`compliance_revalidation_ledger`. Rate-limit, authorization-loss, transient-provider, and invalid-
evidence errors append an `indeterminate` entry with a bounded failure token and never remove data.
This prevents absence or transport failure from becoming takedown authority.

For deleted, protected, suspended-author, or unavailable observations, one transaction inserts the
ledger entry, changes the provider availability, marks the account source removed, inserts its
tombstone, and enqueues `social.source.removed.v1` with `reason = retention_policy`. The tombstone
retains the provider-specific reason; the shared event truthfully states only why the local library
let go. A conditional source-state update makes repeated checks append-only in the ledger while
tombstone and deletion request remain singletons.

### D5: Takedowns fail closed against delayed work

Normal source publication ignores a source already marked removed, and the completion consumer
rejects links for removed sources. This prevents delayed bookmark pages or Knowledge results from
recreating active downstream state. Reactivation, if provider policy later allows it, requires an
explicit future policy and is not inferred from an ordinary observation.

## Risks / Trade-offs

- [The runtime does not yet schedule or transport these services] -> Keep one durable, fully tested
  application seam and document the exact scheduler/transport wiring still required; do not claim
  live periodic execution or broker delivery.
- [A late completion can race a takedown] -> Lock the source row and perform each state transition in
  one transaction; either the link commits before the retained tombstone audit or the removed state
  rejects it.
- [Provider status classification can be wrong] -> Only the official adapter may produce an
  authoritative observation; all ambiguous and transport failures map to `indeterminate` and have
  no takedown authority.
- [Retaining normalized provider text may violate a future stricter deletion policy] -> Removed
  sources are no longer published or linked, while raw/normalized retention remains controlled by
  the separate provider-policy retention configuration; this change does not fabricate a universal
  right to retain bytes.

## Migration Plan

There is no database migration. A fresh development database is created from the edited
first-version `schema.sql`. Deploy contracts and the already-green Knowledge social consumer first,
then deploy this X producer/consumer code, then wire the existing outbox/inbox transport and periodic
scheduler. Rollback stops scheduling/consumption and restores the previous application/schema
definition only in disposable development environments; committed takedown events must not be
retracted or replayed as active facts.
