# X connector requirements

## Goals

1. Connect a user account with OAuth 2.0 PKCE and offline refresh.
2. Archive posts referenced by bookmarks and maintain native bookmark folders.
3. Use full successful snapshots as authority for bookmark/folder absence.
4. Preserve honest observation timestamps and upstream availability/compliance state.
5. Delegate linked articles to Extractor and interpretation/indexing to Knowledge.

## Non-goals

Generic web scraping of X, browser-cookie login, pretending local collections are native folders, or assuming partial scans prove removal.

## Requirements

- Read scopes and optional `bookmark.write` consent are separate.
- Posts and bookmark membership are different entities.
- Partial scans only add/update observations.
- Full bookmark/folder snapshots commit authority atomically.
- Rate limits and credit usage are budgeted per account/global policy.
- Provider writes are idempotent, audited, and serialize conflicting account mutations.
- Deleted/protected/suspended/unavailable states are represented, not silently erased.

First slice: OAuth test account -> full bookmarks snapshot -> normalized SocialSource -> Knowledge indexing -> operation result.
