## Context

Bookmark snapshots already stage and atomically reconcile account bookmark state. The embedded schema has no folder authority or membership observation records, so the same absence discipline cannot yet apply to the native folder state described in `specs/folder-snapshot/spec.md`.

## Goals / Non-Goals

**Goals:**

- Give folder discovery and each folder's membership list separate complete-snapshot authority.
- Preserve membership history as observations rather than destructive diffs.
- Preserve a provider capability limit as explicit state without pretending the API returned an empty folder set.
- Make the consistency boundary with bookmark and local organization explicit in code-level documentation.

**Non-Goals:**

- Folder creation, rename, delete, or membership mutation.
- Inferring folder state from tags, collections, bookmark ordering, or legacy metadata.
- Changing bookmark snapshot authority or publishing a new cross-repository event contract.

## Decisions

### Separate folder and per-folder membership authorities

The schema will keep a folder-list snapshot authority for native folder entities and a membership snapshot authority per `(account, folder)` identity. This avoids treating a successful folder listing as authority over a membership list the provider did not return. A shared authority table was considered, but would make the subject of absence ambiguous and invite accidental cross-reconciliation.

### Stage first, reconcile in the completion transaction

Folder entities and memberships will be written to snapshot-scoped staging rows while pages are accepted. Completion will update the authority pointer, current projection, membership observations, terminal run state, and statistics in one transaction. Directly applying page changes was rejected because an interrupted page traversal would expose false absences.

### Capability limits are terminal non-authoritative outcomes

The provider adapter will expose a typed capability-limit result distinct from an empty page or transport failure. The sync run records the limit and retains every current authority pointer. Treating the limit as an empty result was rejected because it fabricates removals and folder absence.

### Reuse normalized posts without coupling bookmark status

Membership staging references existing normalized post identities, following bookmark snapshots' post persistence path. It never requires an active bookmark row, so native membership and saved state remain independently observable.

## Risks / Trade-offs

- [Provider support differs by endpoint or account] → Persist the specific operation and capability-limit outcome, and make no authority claim.
- [A folder disappears while membership runs are in flight] → Completion is scoped to the folder identity observed at run start; a later authoritative folder-list snapshot is the only source for folder absence.
- [Large membership sets increase staging cost] → Reuse bounded pagination and snapshot-scoped keys; do not make a page authoritative.

## Migration Plan

The repository is in development status: the current embedded schema is edited in place and disposable test databases are created from it. No migration file or migration tooling is introduced. Rollback is the normal application rollback to the prior schema definition before data exists.
