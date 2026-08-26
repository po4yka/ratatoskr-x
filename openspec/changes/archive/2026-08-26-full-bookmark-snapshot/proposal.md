## Why

Plan item 4 needs to turn the official X API's paginated bookmark listing into authoritative account state without turning an interrupted, rate-limited, or budget-stopped traversal into false removals. The existing OAuth, budget, normalization, and disposable-PostgreSQL foundations make this the first point at which the service can safely own bookmark truth rather than importing legacy observations.

## What Changes

- Add a full-bookmark snapshot application layer that pages only through the supported API seam, charges the durable budget before each request, normalizes each received envelope, and writes each validated page plus its opaque continuation checkpoint in one transaction.
- Persist a resumable full-sync run, snapshot identity, checkpoint, page/item statistics, and terminal outcome. A failed or budget-blocked run remains non-authoritative and resumes from its last committed continuation token.
- Persist normalized users, posts, relations, media metadata, and account-specific bookmark observations from each page; bookmark rows retain `first_observed_saved_at`, refresh `last_observed_saved_at`, and never derive native save times from post timestamps.
- Commit complete-snapshot authority in one transaction: publish the newly completed snapshot as current for its account, reconcile counts, and set `observed_removed_at` with authoritative run/snapshot evidence for previously active bookmarks that were absent from the complete run. No bookmark row is silently deleted.
- Add a fixture-driven provider seam and integration tests for checkpoint resume after a mid-run failure, atomic authority visibility, removal observations, and count reconciliation under multi-page traversal.
- Extend `schema.sql` in place, with no migration files or migration tooling.

Out of scope: frequent partial scans (plan item 5), native folder and folder-membership scans (item 6), provider write-back, event publication, and external article extraction.

## Capabilities

### New Capabilities

- `bookmark-snapshot`: complete, budget-bounded bookmark enumeration with opaque checkpoints, normalized record persistence, truthful observation timestamps, and atomic absence authority.

### Modified Capabilities

- `x-archive-schema`: the existing bookmark, sync-run, and snapshot tables gain the in-place fields and constraints needed to hold snapshot membership, resumable checkpoints, terminal statistics, and authoritative removal evidence.

## Impact

- Adds a synchronization crate/application seam that composes `x-budget`, `x-normalize`, and `x-persistence`; it does not add an HTTP endpoint or expose a cross-repository contract.
- Edits `schema.sql` and its real-PostgreSQL integration assertions in place; disposable test databases continue to be created from that same definition.
- Adds synthetic official-API pagination fixtures and harness-driven integration tests. No new production dependency, provider credential, browser session, or migration machinery is introduced.
