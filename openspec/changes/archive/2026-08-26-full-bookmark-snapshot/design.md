## Context

See [proposal.md](proposal.md) for motivation. The workspace has a durable per-account `BudgetGate`, pure official-API envelope normalization, and a PostgreSQL schema applied in place to disposable test databases. No provider bookmark client or synchronization writer exists yet. The previous current-state tables cannot be used as a work area: changing them page by page would let a failed traversal leak a mixed authority set.

## Goals / Non-Goals

**Goals:**

- Compose a bounded, testable source seam with the existing budget, normalization, and persistence boundaries.
- Make checkpoint commits and full-snapshot authority separate, explicit operations.
- Keep all absence inference, current-authority replacement, bookmark reconciliation, and statistics in one explicit PostgreSQL transaction.
- Test exact durable state through the existing disposable-database harness, including a controlled mid-run provider failure.

**Non-Goals:**

- An HTTP implementation, credential loading, retries/backoff policy, partial scans, folders, event/outbox publication, and compliance revalidation.
- Deriving save/removal timestamps from provider post timestamps or deleting prior bookmark observations.
- Migration tooling or compatibility paths; `schema.sql` remains the single first-version schema definition.

## Decisions

### D1: A generic async provider seam with opaque continuation tokens

`x-sync` will define an async `BookmarkPageSource` trait returning one official API envelope plus an optional continuation token. `BookmarkSnapshotService` is generic over that trait, so the first application layer has no HTTP dependency and integration tests drive exact pagination and failure schedules with a hand-written harness. Tokens are stored and passed as opaque `Option<String>` values; the service never interprets, synthesizes, or compares their internal shape. The production official-API adapter can implement the seam in the next HTTP-focused slice.

An internal generic trait is preferred to a mocking framework or a `dyn` async trait: concrete harnesses remain simple, and no production dependency is required.

### D2: Per-page work is durable but non-authoritative

Each source response is normalized before it can affect persistence. One transaction upserts normalized users/posts/relations/media, inserts page membership into `snapshot_bookmark_items`, increments page/item counters, and writes the continuation token to the `full` sync run. It does not mutate `x_archive.bookmarks` or the account authority pointer. A crashed/resumed run can therefore reuse the same snapshot identity, skip duplicate staged membership through a unique key, and request only the checkpoint it committed.

Staging membership by normalized local post id makes bookmark records refer to normalized posts without copying the graph into snapshot tables. Provider content updates are safe to persist early because post content is not account-specific bookmark authority.

### D3: Current authority and membership stage separately

`bookmark_snapshot_authority(account_id PRIMARY KEY, snapshot_id)` points to the one complete snapshot that currently has absence authority. `snapshot_bookmark_items(snapshot_id, post_id)` is the durable candidate set. `snapshots` belongs to the run and records completion; `sync_runs` records the checkpoint, terminal state, and reconciliation counters. A `bookmarks.observed_removed_snapshot_id` foreign key preserves the particular complete snapshot that inferred the removal.

This separation is preferred to adding a mutable current-snapshot id to `bookmarks`: the latter would expose page-by-page state and cannot prove that all candidate pages existed before authority changed.

### D4: Final reconciliation uses one serializable transaction and an account lock

On a page with no continuation token, the service opens a serializable transaction, takes a transaction-scoped advisory lock derived from the account id, verifies the run/snapshot are still running and fully staged, then:

1. upserts staged memberships into `bookmarks`, restoring a previously removed row if it reappears and retaining `first_observed_saved_at`;
2. marks only previously active rows absent from this snapshot as observed removed with one completion timestamp and the new snapshot id;
3. records addition/retention/removal counts and marks the run/snapshot complete;
4. upserts `bookmark_snapshot_authority` to the completed snapshot; and
5. commits explicitly.

The authority pointer is the final write in the transaction. Post-commit code only returns the outcome; no external effect occurs inside it. The account advisory lock serializes competing full runs without holding a connection during provider I/O. Unknown commit outcomes are reported as unknown rather than replayed automatically.

### D5: Budget reservation precedes provider contact

The service reserves a cost of one from `BudgetGate` before every call to the page source. Budget refusal ends the run as failed/non-authoritative without invoking the source. A source error after an accepted reservation remains a failed snapshot; budget refund/retry is intentionally excluded until the provider error policy is designed, so recorded usage is conservative and truthful.

### D6: Time is supplied by a small `Clock` seam

The service depends on a `Clock` trait for UTC observation time. Production uses system time; the test harness has deterministic instants. This keeps `first_observed_saved_at`, `last_observed_saved_at`, and `observed_removed_at` exact and independently assertable without timing sleeps.

## Risks / Trade-offs

- [A source succeeds remotely but the page transaction fails] → its continuation is not committed, so a later retry requests the same opaque token and idempotent upserts/staging keys make replay safe.
- [A source fails after budget reservation] → conservatively retain the charge and preserve the checkpoint; a later error-classification slice can add a documented refund policy.
- [Concurrent full runs for one account] → advisory locking at finalization and run state validation prevent two authority swaps; they still independently stage content, which is idempotent.
- [A process loses connectivity during final commit] → return an unknown-finalization error and require reconciliation by run id; do not blindly rerun the swap.
- [Staging accumulates after repeated failures] → it is intentionally retained for resume; expiry/cleanup belongs to a later operations retention policy rather than this authority slice.

## Migration Plan

Development status forbids migrations. Edit `schema.sql` in place, update schema integration tests, and create each disposable test database from the current definition. Rolling deployment compatibility is not applicable while no persisted production data must survive the schema change.
