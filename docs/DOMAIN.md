# X connector domain model

## Terms

- **Account connection:** X identity, encrypted credentials, scopes, and status.
- **Post:** provider content and relation graph independent of save state.
- **Bookmark observation:** evidence of membership seen during a scan.
- **Full snapshot:** complete successful enumeration authoritative for absence.
- **Bookmark folder:** native provider collection and membership.
- **Saved authority:** authoritative platform state for the completed snapshot.
- **Compliance state:** active, deleted, protected, suspended, or unavailable.

## Invariants

1. `created_at` of a post is not bookmark save time.
2. Use `first_observed_saved_at` and `observed_removed_at` unless authoritative timestamps exist.
3. Partial scans never remove bookmarks or memberships.
4. Native folders and local Ratatoskr collections are separate.
5. Read connection does not imply write consent.
6. Linked articles are separate source documents with separate provenance.
7. Provider content lifecycle is retained as explicit state/tombstones.
