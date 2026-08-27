# X connector data model

## Planned owned schema: `x_archive.*`

- `accounts`, encrypted `credentials`, scopes, expiry/status, budgets/limits.
- `posts`, authors, relations, media, URL entities, raw revision blob references.
- `bookmarks`, observations, current state, full snapshots/pages/checkpoints.
- `bookmark_write_authorizations`, separately activated current write-scope evidence.
- immutable `bookmark_write_consents`, account-scoped `bookmark_write_operations`, and append-only
  `bookmark_write_audit_events`; raw idempotency keys, credentials, provider bodies, and post text
  are excluded.
- `bookmark_folders`, memberships, folder snapshots.
- `social_sources` and append-only `social_source_revisions`; the current digest is a projection,
  while every retained digest remains eligible for an exact Knowledge completion link.
- `knowledge_analysis_links`; X owns the inbox event identity and
  `(social_source_id, content_digest)` linkage only. Knowledge owns analyses, embeddings, search
  documents, and its private run identifiers.
- append-only `compliance_revalidation_ledger`, singleton source-scoped `tombstones`, write audits,
  legacy mappings, outbox/inbox.

## Constraints

Provider IDs are stable unique identities. Post revision and bookmark membership are separate. Observation timestamp names are honest. A snapshot is authoritative only after atomic completion. Credentials and private text are excluded from logs/events. Owner scope is mandatory. Cross-schema writes/foreign keys are forbidden.

Retention follows provider policy and user privacy while preserving permitted audit/tombstone evidence and local user-captured derivatives under explicit policy.

## Compliance evidence and removal

Each ledger row is account/source scoped and records `checked_at`, the closed observed `outcome`, an
optional non-sensitive provider request ID, and a closed `failure_class` only for
`indeterminate`. It stores no credential, authorization header, raw provider body, post text,
username, or URL. An indeterminate row cannot remove a source.

An authoritative unavailable result writes its ledger row in the same transaction that changes the
post availability, sets `social_sources.removed_at` and `removal_reason = retention_policy`, creates
the one tombstone tied to the authorizing ledger row, and adds one `social.source.removed.v1` outbox
fact. The tombstone retains the X-specific reason; the shared event says only why Ratatoskr removed
the library item. Development status still forbids migrations, so all definitions live in the one
current `schema.sql`.

## Bookmark write evidence

One consent is bound to exactly one owner/account/add-or-remove/target/surface and can be consumed by
at most one live operation. Operations store only SHA-256 idempotency identity, a closed state and
outcome, bounded provider request/rate evidence, and projection observation references. Confirmed
known-target writes use `last_write_operation_id`, `last_write_observed_at`, or
`observed_removed_write_operation_id`; these fields are observations and do not claim a native X
save/removal timestamp. Unknown confirmed targets remain `projection_pending` without fabricated
post rows. `uncertain` operations advance only from complete-snapshot evidence; partial scans never
have that authority.
