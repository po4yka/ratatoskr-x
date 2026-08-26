## Why

Bookmark-folder state from the legacy monolith is account-specific upstream state, but the service currently cannot take a truthful snapshot of folders or their post memberships. It needs the same complete-snapshot authority boundary that prevents bookmark scans from fabricating removals.

## What Changes

- Add read-only native folder entities and independent folder-membership snapshot runs.
- Stage folder and membership observations, then atomically replace each folder's authoritative membership only after a complete successful traversal.
- Record membership additions and removals as observations linked to the completing snapshot; incomplete scans retain prior authority.
- Persist provider capability limits explicitly when folders or folder memberships cannot be read, without creating synthetic folders or inferred memberships.
- Document the independent but consistent authority rules for bookmarks, folders, and local Ratatoskr collections.

## Capabilities

### New Capabilities

- `folder-snapshot`: Native X folder discovery and per-folder membership snapshots with truthful capability reporting and atomic absence authority.

### Modified Capabilities

- None.

## Impact

- Affects the embedded `x_archive` schema, snapshot persistence, synchronization logic, redacted X API fixtures, tests, and bookmark/folder consistency documentation.
- Uses only supported read capability; it adds no folder mutation, local collection behavior, or cross-repository contract change.
