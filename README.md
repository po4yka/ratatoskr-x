# Ratatoskr X

`ratatoskr-x` is the X account and bookmark archive bounded context for Ratatoskr. It authenticates a user through the official X OAuth flow, synchronizes bookmarks and bookmark folders, preserves normalized post content and media metadata, and publishes authoritative social-source events for indexing and analysis.

> **Status:** the service scaffold, official OAuth connection, post normalization, complete and frequent bookmark scans, native-folder authority, explicit browser-capture command consumer, normalized social-source and linked-article outbox facts, Knowledge completion linkage, and the due-driven compliance/takedown application service are implemented. Knowledge owns analysis, embeddings, and search documents; X stores only exact `(social_source_id, content_digest)` completion linkage. Knowledge completion/removal transport and timed compliance invocation are not wired into the runtime, so they are not live deployment claims. An official provider adapter for compliance classification, write-back, and legacy import remain unimplemented.

> [!IMPORTANT]
> **Ratatoskr is in development.** No database holds data that has to survive a schema change.
> While this status holds, these two rules replace what the documents below plan:
>
> - the API and the database keep their first version. There is no `v2` and no later major
>   version.
> - the database has no migrations. No schema exists yet. The first persistence change creates one
>   schema definition, and later schema changes edit it in place.
>
> Only the repository owner changes this status.

## Role in Ratatoskr

Unlike Instagram and Threads, X exposes an official user-authorized bookmark surface. Ratatoskr can therefore distinguish upstream bookmark state from a local explicit capture.

This service owns:

- X account identity;
- OAuth 2.0 Authorization Code with PKCE;
- encrypted access and refresh tokens;
- read-only bookmark synchronization;
- bookmark-folder synchronization;
- post, author, media, reply, quote, and repost representations;
- full bookmark snapshots and observed removals;
- provider rate-limit and cost budgets;
- optional bookmark write-back under separate consent;
- compliance revalidation and tombstones;
- migration from the legacy Field Theory bookmark pipeline.

It does not fetch linked articles, run LLM analyses, or own general Ratatoskr collections. Linked public URLs are delegated to `ratatoskr-extractor`; normalized source material is interpreted by `ratatoskr-knowledge`.

## Authorization model

The default connection is read-only and requests only the scopes required to identify the user and read posts/bookmarks, including offline access for background synchronization.

Write authority is separate. Adding or removing bookmarks through Ratatoskr requires a later explicit consent step and a dedicated write scope. A user who only wants local backup never needs to grant write access.

Credentials remain inside this service:

- access and refresh tokens are encrypted at rest;
- granted scopes and expiry are recorded explicitly;
- refresh and reauthorization state are auditable;
- token values never appear in events, logs, traces, or public responses;
- Platform, Telegram, Knowledge, and clients receive no plaintext X credential.

## Owned data model

The service owns the `x_archive.*` PostgreSQL schema defined in [`schema.sql`](schema.sql), applied in place with no migrations while development status forbids them. The first-version tables:

```text
accounts
credentials
users
posts
post_relations
media
bookmarks
bookmark_folders
bookmark_folder_items
sync_runs
snapshots
rate_limit_state
social_sources
social_source_revisions
knowledge_analysis_links
compliance_revalidation_ledger
tombstones
explicit_captures
outbox_events
inbox_events
```

Post and bookmark are separate entities. A post may exist without being bookmarked, and one bookmark may belong to several native folders.

### Observed bookmark time

X post creation time is not the same as bookmark creation time. Where the provider does not expose the exact save or removal timestamp, Ratatoskr uses honest observation fields:

```text
first_observed_saved_at
last_observed_saved_at
observed_removed_at
```

It never invents precise `saved_at` or `unsaved_at` values.

## Bookmark synchronization

The sync model combines frequent partial scans and periodic full snapshots.

### Frequent scan

- enumerate the newest bookmark pages within a configured budget;
- upsert new posts, bookmarks, media, and changed metadata;
- update last-observed timestamps;
- never infer removal from an incomplete traversal.

### Full snapshot

- enumerate every bookmark page successfully;
- record a snapshot identity and completion boundary;
- only after complete success, mark previously present but now absent bookmarks as removed;
- reconcile native folder membership through its own complete snapshot authority;
- preserve warnings if one optional enrichment fails.

Invariant:

> A partial bookmark scan can add or update state but cannot prove that a bookmark was removed.

This protects local history from pagination errors, cost limits, rate limits, and interrupted jobs.

## Native folders and Ratatoskr collections

X bookmark folders are upstream state. Ratatoskr collections and tags are local organization. They remain separate many-to-many relationships.

A post can simultaneously:

- be bookmarked in X;
- belong to one or more X folders;
- carry local Ratatoskr tags;
- belong to local collections;
- have a user note;
- be linked to an extracted external article.

Folder reconciliation must never overwrite local organization. A folder entity is observed only from a supported provider operation; bookmark presence, a local tag, or a local collection never creates one. Each native folder's membership has a separate staged snapshot and authority pointer: a complete traversal atomically replaces that folder's current membership, while a failed, truncated, cancelled, or rate-limited traversal leaves its previous authority intact. Added and absent memberships are retained as observations linked to the completing snapshot.

If the provider does not expose folder listing or membership reading for the connected account, Ratatoskr records that operation's capability limit and makes no empty-folder, membership-removal, or successful-snapshot claim. This is different from an observed empty complete result.

Bookmark saved state and native folder membership are therefore consistent in their authority discipline but independent in meaning: a normalized post can be bookmarked, belong to one or more native folders, and belong to local collections or tags in any combination.

## Post normalization

The normalized representation preserves:

- canonical post identity and URL;
- author identity and handle;
- regular and long-form text when available;
- publication and edit metadata;
- reply, quote, and repost relationships;
- media metadata and blob references where storage is permitted;
- resolved URL entities;
- provider-specific raw response references;
- upstream availability state.

The service publishes a common `SocialSource` contract with:

```text
platform = X
acquisition = OfficialApi
saved_authority = AuthoritativePlatformState
```

Provider-specific fields that cannot be represented in the common contract remain available through raw blob references and typed extension metadata.

## Linked articles

A post and an external article remain distinct sources:

```text
X post
├── normalized social source -> Knowledge
└── expanded external URL    -> Extractor -> Knowledge
```

Knowledge may produce a composite analysis that explicitly separates the post's claims from the article's content and preserves provenance for both.

`social.source.captured.v1` and `social.source.updated.v1` are the agreed Knowledge analysis
requests. X deduplicates them by source revision, accepts
`knowledge.analysis.completed.v1`, and links the result to that exact retained digest. Search
documents remain Knowledge-owned; X does not persist Knowledge-private analysis or search IDs.

## Compliance revalidation

`ComplianceRevalidationService` is a bounded, due-driven application seam. For each unremoved
account source whose last ledger entry predates the caller's due instant, it reserves one unit from
the existing provider-request budget and asks an official-provider adapter for a closed availability
classification. Every attempted check appends non-sensitive evidence to
`compliance_revalidation_ledger`; ambiguous, failed, scope-lost, and rate-limited checks are
`indeterminate` and have no takedown authority.

An authoritative deleted, protected, suspended-author, or unavailable result atomically updates
provider availability, marks the account source removed, records one source tombstone, and enqueues
the shared `social.source.removed.v1` fact with `reason = retention_policy`. Knowledge consumes that
fact to delete or tombstone its analysis, embedding, and search projection. Normal source
observations and delayed Knowledge completions cannot reactivate a removed source.

The future runtime scheduler must call this service periodically, and the outbox/inbox transport
must deliver the facts. Neither runtime facility exists in this bootstrap repository yet. Retention
of normalized or raw provider bytes remains governed by separate provider/legal policy; a tombstone
does not assert an unconditional right to retain those bytes.

## Write-back

Optional write-back operations include adding and removing bookmarks. Requirements:

- separate write consent;
- authenticated user principal;
- idempotency key;
- explicit target post;
- audit record;
- provider response classification;
- no implicit write caused by viewing or importing a URL.

Native folder mutations are added only if the official API and granted scopes support the required operation at implementation time.

## Compliance and upstream availability

Provider content may later become deleted, protected, suspended, or otherwise unavailable. The local archive records upstream state separately from local preservation:

```text
active
deleted
protected
author_suspended
unavailable
unknown
```

A revalidation worker updates authoritative API-derived projections according to provider policy while preserving local audit and snapshot records under the configured retention rules.

## Rate limits and cost budgets

The connector stores account-level provider limits and uses bounded concurrency. Synchronization policy accounts for:

- endpoint-specific rate limits;
- reset timestamps and `Retry-After` behavior;
- per-run request budgets;
- full-snapshot age;
- folder-enrichment cost;
- retry backoff;
- one account's failure not blocking another account.

The service reports when a snapshot is delayed by budget or provider limits rather than silently presenting stale state as current.

## Commands and events

Expected contracts include:

```text
x.account.connected.v1
x.account.reauth_required.v1
x.bookmarks.sync_requested.v1
x.bookmarks.snapshot_started.v1
x.bookmarks.snapshot_completed.v1
x.bookmark.observed.v1
x.bookmark.removed.v1
x.folder.membership.changed.v1
x.post.upserted.v1
x.post.unavailable.v1
social.source.upserted.v1
social.source.unavailable.v1
```

Events are idempotent under at-least-once delivery. Duplicate page or snapshot processing converges on the same post and bookmark records.

### Explicit browser-capture command

Platform routes an explicit X permalink through the provider-only JetStream subject
`cmd.x.capture.requested.v1`. The X service accepts only the canonical
`SocialCaptureRequested` command with `provider = x`, `acquisition = browser_extension`, and
`saved_authority = explicit_user_capture`; it stores the original permalink and action timestamp
in its durable inbox before acknowledging delivery. This is local user intent, never an X native
bookmark or Saved claim.

Platform pre-provisions the fixed `ratatoskr_x_browser_capture` pull durable. X connects with its
own NKey seed path (`RATATOSKR__BUS__NKEY_SEED_PATH`) and requires a credential-free
`RATATOSKR__BUS__URL`; its identity may read consumer information, pull only that durable, and
acknowledge only its deliveries. It has no `$JS.API.>` permission and cannot create a consumer with
another provider's filter. A missing or mismatched durable prevents readiness rather than silently
dropping browser captures.

## Legacy migration

The legacy Field Theory system reads a SQLite database and Markdown library without authenticating to X. Migration treats those records as historical observations:

```text
acquisition = LegacyImport
saved_authority = LegacyObservation
```

Field Theory categories become local tags or migration metadata, not native X folders. The migration process:

1. imports legacy rows and preserves external IDs;
2. deduplicates by canonical post identity or URL;
3. records provenance and original category;
4. runs the official connector in shadow mode;
5. performs the first complete authoritative snapshot;
6. reconciles overlaps without erasing legacy evidence;
7. retires the SQLite dependency after validation.

## Security invariants

1. X tokens remain inside this service.
2. Read-only connection does not imply write consent.
3. Partial scans never confirm removal.
4. Native folders never overwrite local collections.
5. Linked articles are extracted by the generic extractor, not by undocumented X scraping.
6. Raw provider data is treated as untrusted input.
7. Private or protected content access is never bypassed.
8. Every provider mutation is explicit, idempotent, and audited.
9. Search and analysis consumers receive normalized data, not provider credentials.

## Observability

Core metrics include:

```text
x_sync_duration
x_bookmarks_seen
x_full_snapshot_age
x_partial_scan_pages
x_rate_limit_remaining
x_rate_limit_waits
x_api_cost_budget_used
x_folder_reconciliation_failures
x_posts_changed
x_bookmarks_removed
x_reauth_required
x_compliance_state_changes
```

Every sync run records mode, pages, cursors, request budget, completeness, warnings, and state transitions.

## Non-goals

- Generic public-web scraping.
- Article extraction or browser automation.
- LLM analysis, embeddings, or search ownership.
- Treating local collections as X folders.
- Automatic write access during initial connection.
- Inferring exact save/removal times unavailable from the provider.
- Keeping the legacy Field Theory database as a permanent runtime dependency.

## Initial milestones

1. Define account, credential, post, bookmark, folder, and snapshot schemas.
2. Implement OAuth PKCE and encrypted refresh-token storage. *(done)*
3. Implement read-only bookmark pagination and post normalization. *(normalization done)*
4. Add periodic complete snapshots and removal reconciliation.
5. Add native folder synchronization.
6. Publish normalized social-source events. *(done)*
7. Delegate linked articles to Extractor. *(durable request/result linkage done)*
8. Integrate Knowledge analysis linkage and compliance revalidation. *(application services done;
   runtime scheduling/transport pending)*
9. Add separately consented idempotent bookmark write-back.
10. Import Field Theory data and run shadow comparison.

## Workspace integration

Planned: `ratatoskr-workspace` will pin this service with compatible social contracts, Platform, Knowledge, Extractor, Web, Mobile, Browser Extension, and Telegram commits. No workspace pin or integration profile exists for this service today. The service will remain independently testable using recorded provider fixtures and mock OAuth/API servers.

## Project status

Plan items 1–8 in `docs/IMPLEMENTATION_PLAN.md` now have durable application behavior and tests.
For item 8, this means revision-deduplicated SocialSource requests, exact Knowledge completion
linkage, the revalidation ledger, and replay-safe takedown outbox state. It does not mean live
periodic execution or Knowledge/compliance broker delivery: those require the future runtime
scheduler, transport, and official-provider compliance adapter. Write-back and legacy migration
remain planned.
