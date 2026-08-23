# X connector data model

## Planned owned schema: `x_archive.*`

- `accounts`, encrypted `credentials`, scopes, expiry/status, budgets/limits.
- `posts`, authors, relations, media, URL entities, raw revision blob references.
- `bookmarks`, observations, current state, full snapshots/pages/checkpoints.
- `bookmark_folders`, memberships, folder snapshots.
- compliance/tombstone records, write audits, legacy mappings, outbox/inbox.

## Constraints

Provider IDs are stable unique identities. Post revision and bookmark membership are separate. Observation timestamp names are honest. A snapshot is authoritative only after atomic completion. Credentials and private text are excluded from logs/events. Owner scope is mandatory. Cross-schema writes/foreign keys are forbidden.

Retention follows provider policy and user privacy while preserving permitted audit/tombstone evidence and local user-captured derivatives under explicit policy.
