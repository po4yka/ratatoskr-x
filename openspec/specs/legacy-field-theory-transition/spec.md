# legacy-field-theory-transition

## Purpose

Defines how Ratatoskr preserves Field Theory and retired-monolith bookmark history as bounded,
credential-free legacy observations, compares it with official snapshots, and records an
owner-approved transition without fabricating identity or provider authority.

## Requirements

### Requirement: Legacy sources are allow-listed, read-only, and credential-free
The importer SHALL accept only documented retired-monolith bookmark metadata, Field Theory JSONL
v1, or Field Theory SQLite schema v6 through an explicit operator invocation. It SHALL validate the
complete source shape before target writes, open SQLite without write authority, and SHALL NOT read,
select, copy, decrypt, persist, report, or log cookies, OAuth tokens, browser sessions,
authorization headers, or other credential material. The source SHALL remain unchanged after every
outcome.

#### Scenario: Credential-bearing source is rejected before target writes
- **WHEN** a candidate source exposes a credential or session field outside the allow-listed bookmark shape
- **THEN** preflight refuses the import, records no target rows, and identifies only the rejected field name without its value

#### Scenario: Read-only source survives a failed import unchanged
- **WHEN** validation fails after a source has been opened for import
- **THEN** the source bytes remain unchanged and the target contains no partial import batch

### Requirement: Target ownership is explicit and verified through current OAuth identity
A real import SHALL require an operator-selected existing X account and owner approval bound to the
source digest. Before target writes, the service SHALL resolve the authenticated X user through the
account's current OAuth connection and require that provider user ID to equal the account's stored
provider identity. It SHALL NOT infer archive ownership from post authors, handles, URLs, legacy
request identifiers, filesystem ownership, source credentials, or the number of configured
accounts.

#### Scenario: Current OAuth identity authorizes the selected account mapping
- **WHEN** the approved target account, source digest, and current authenticated provider user ID all match
- **THEN** the import run is bound to that account and records the non-secret mapping evidence

#### Scenario: Ambiguous or mismatched account fails closed
- **WHEN** approval is absent, OAuth identity is unavailable, or the authenticated provider user ID differs from the selected account
- **THEN** the import is refused before target writes and no alternative owner is inferred

### Requirement: Import preserves legacy observations and stable post identity truthfully
For each accepted row, the importer SHALL prefer a valid X post provider ID, report any conflict
with a status URL, and use a canonical status URL only as a secondary deduplication hint. It SHALL
retain source kind and schema version, original legacy record identity, safe source-row digest,
observation timestamps, legacy category/folder metadata, import parser version, and
`legacy-import` provenance. Publication time SHALL remain distinct from first/last observed saved
time; opaque ordering keys and historical Field Theory bookmark dates SHALL NOT be exposed as exact
native save times. Missing author provider identity SHALL remain explicitly unresolved rather than
being synthesized from a handle.

#### Scenario: Stable provider identity creates an imported observation
- **WHEN** an allow-listed row has a valid post provider ID and an approved target account
- **THEN** the resulting account bookmark observation refers to that provider post identity, carries legacy-import evidence, and has no exact provider saved timestamp

#### Scenario: Mutable author handle does not become provider identity
- **WHEN** a legacy row contains an author handle but no provider user ID or existing normalized author match
- **THEN** the report marks author identity unresolved and no provider user record is fabricated from the handle

#### Scenario: Conflicting URL identity is retained for review
- **WHEN** the explicit post ID and the post ID parsed from the legacy URL differ
- **THEN** neither value is silently discarded, the row is classified as an identity conflict, and authoritative bookmark state is unchanged

### Requirement: Identical imports are restartable and idempotent
The importer SHALL validate a batch before applying it and SHALL make the accepted batch atomic. A
retry after interruption SHALL reuse durable source identity, and repeating an identical source for
the same account and importer parser version SHALL create no duplicate post, bookmark observation,
legacy evidence, or event. Each completed report SHALL deterministically count inserted, matched,
updated, conflicted, rejected, and unmapped rows.

#### Scenario: Repeating the same fixture import is a no-op
- **WHEN** the same approved synthetic source is imported twice for the same target account
- **THEN** the second result reports no new target or evidence rows and the persisted counts remain unchanged

#### Scenario: Invalid row rolls back the batch
- **WHEN** one row fails batch validation before the import transaction completes
- **THEN** no row from that batch becomes visible and a corrected retry can complete without cleanup

### Requirement: Shadow comparison is deterministic, redacted, and non-authoritative
Shadow mode SHALL compare one completed import with one complete successful official bookmark
snapshot for the same account. It SHALL classify stable provider-ID matches, secondary URL matches,
legacy-only records, official-only records, identity conflicts, unmapped records, and content,
category, or folder differences. The report SHALL be deterministically ordered and digest-bound to
both inputs, SHALL omit post bodies, handles, raw URLs, credentials, and source paths, and SHALL NOT
change bookmark state or assign absence authority to legacy data.

#### Scenario: Shadow report classifies each difference without mutation
- **WHEN** imported and official fixture sets contain matching, legacy-only, official-only, and conflicting records
- **THEN** the report contains deterministic counts and stable non-secret references for every class while the current bookmark projection is unchanged

#### Scenario: Incomplete official scan cannot produce a cutover verdict
- **WHEN** shadow comparison is requested with a partial, failed, cancelled, truncated, rate-limited, or schema-invalid official run
- **THEN** the request is refused as non-reviewable and no clean or approved verdict is recorded

### Requirement: Cutover and rollback remain owner-gated and evidence-bound
The service SHALL generate a cutover checklist bound to the selected import run, complete official
snapshot, shadow report digest, target account, privacy review, rollback steps, and workspace
changeset. It SHALL record local transition approval only from explicit owner evidence matching
that checklist and SHALL reject stale or mismatched approval. Imported history SHALL remain retained
through cutover and rollback. Activating or reverting cross-repository routing or schedules SHALL
occur only through the separately approved workspace changeset.

#### Scenario: Matching approval makes the local transition reviewable
- **WHEN** the owner approves the exact current checklist and every required evidence item is complete
- **THEN** the service records a reviewable local transition decision without deleting legacy evidence or changing external routing

#### Scenario: Stale approval cannot authorize cutover
- **WHEN** the import run, official snapshot, shadow digest, target account, or checklist changes after approval
- **THEN** the approval no longer matches and the transition returns to owner-review-required

#### Scenario: Rollback preserves both evidence sets
- **WHEN** an approved workspace cutover is rolled back during its stability window
- **THEN** the rollback notes restore the prior routing plan while official and imported observations, reports, and approval evidence remain available for diagnosis
