## Why

Complete snapshots establish absence authority but are too costly to run at the cadence needed to discover newly saved bookmarks promptly. A bounded incremental path must improve freshness without ever turning a partial view into removal evidence or letting undetected pagination drift persist.

## What Changes

- Add a safe incremental bookmark scan that reads recent official-API pages until a durable watermark is reached, upserts only observations, and advances its watermark only after a successful bounded run.
- Detect a gap in the recent window and require a complete snapshot instead of continuing incremental scans.
- Apply a stricter per-run request budget than a complete snapshot before provider contact.
- Persist explicit repair records when a later complete snapshot finds incremental drift; record each repair once so retrying reconciliation is idempotent.
- Consume the existing platform scheduler command type `x.bookmarks.scan_requested.v1` so incremental and required full scans can be selected without an HTTP or provider adapter.

## Capabilities

### New Capabilities

- `bookmark-incremental-scan`: bounded, observation-only bookmark refreshes with watermarks, gap escalation, a tighter budget, scheduler-command execution, and complete-snapshot repair evidence.

### Modified Capabilities

- `bookmark-snapshot`: completed full snapshots reconcile and durably record repairs for drift previously left by incremental observation-only scans.
- `x-archive-schema`: the owned first-version schema gains in-place state for incremental watermarks, scan outcomes, and idempotent repair evidence.

## Impact

- Extends `x-sync` and the current `schema.sql`/disposable PostgreSQL assertions; no migration files, HTTP routes, provider credentials, folders, write-back, or cross-repository contract are added.
- Reuses the existing official-envelope normalization and durable `BudgetGate`; incremental caps are supplied by the caller and are lower than the full-snapshot cap.
