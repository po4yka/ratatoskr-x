# Ratatoskr X Agent Instructions

## Scope

These instructions apply to the `ratatoskr-x` repository.

This repository owns the X account and bookmark bounded context: OAuth, posts, bookmarks, bookmark folders, media metadata, authoritative snapshots, rate/cost state, and upstream compliance status.

## Repository mission

The service provides a durable local projection of X content that the connected user is authorized to access, with special emphasis on bookmarks and bookmark folders obtained through the supported user API.

It must preserve the difference between:

- a post as provider content;
- a bookmark as account-specific saved state;
- a bookmark-folder membership;
- a Ratatoskr collection/tag;
- an external article linked by a post.

## Current phase

The repository is in architecture bootstrap. Do not assume Rust crates, OAuth routes, API clients, migrations, synchronization workers, or CI commands exist unless they are present in the checkout.

When creating initial implementation:

- model snapshot authority before optimizing requests;
- keep X SDK/HTTP types inside adapters;
- make read and write consent separate;
- preserve raw/provider evidence safely for diagnostics and migration;
- avoid browser-session scraping as a substitute for the supported API.

### Development status

Ratatoskr is in development. No database holds data that has to survive a schema change. While this
status holds, these rules are binding, and they override anything else in this repository that
plans otherwise, including the rest of this file:

- **One version only.** The API, the database, and the contracts keep their first version. Do not
  add a `v2` or a later major version, and do not add version negotiation, deprecation windows, or
  parallel-major routing.
- **No database migrations.** Do not add a migration file, and do not add migration tooling. A
  schema change edits the current schema definition in place, and a test database is created from
  that definition.
- **The product is `Ratatoskr`.** It is not "Ratatoskr Next". Do not write that name in code,
  documentation, identifiers, comments, or commit messages.

Only the repository owner changes this status. Ask before you write anything these rules forbid.

## Sources of truth

Use this order:

1. active task/changeset and accepted ADRs;
2. `README.md`;
3. social/event contracts from `ratatoskr-contracts`;
4. synchronization state and complete snapshot evidence;
5. redacted provider API fixtures and repository tests;
6. implementation details.

When X does not expose an exact fact, store an observation with honest naming rather than inventing provider authority.

## Hard bounded-context rules

### X service owns

- X account identity and encrypted OAuth credentials;
- granted scopes, expiry/refresh, and connection state;
- normalized X posts, authors, relations, and media metadata;
- account-specific bookmarks;
- native X bookmark folders and memberships;
- sync runs, cursors/tokens, snapshots, rate-limit and usage state;
- upstream unavailable/deleted/protected/suspended status;
- X-specific outbox/inbox records;
- references to linked article extraction and Knowledge analyses.

### X service does not own

- generic article HTTP extraction;
- LLM summaries, entities, embeddings, or search ranking;
- Ratatoskr collections/tags;
- Platform sessions or Telegram interactions;
- Instagram/Threads capture state;
- browser cookies or a userbot session;
- unrelated provider credentials.

External article URLs are delegated to `ratatoskr-extractor`; interpretation is delegated to `ratatoskr-knowledge`.

## OAuth and scopes

Use OAuth 2.0 Authorization Code with PKCE for user authorization.

Default read connection should request only the scopes required for the supported read workflow, conceptually:

```text
users.read
tweet.read
bookmark.read
offline.access
```

Write-back is a separate consent step, conceptually:

```text
bookmark.write
```

Rules:

- validate `state`, PKCE verifier/challenge, redirect binding, and authenticated internal user;
- record the exact granted scopes;
- detect missing/downgraded scopes;
- keep access/refresh tokens encrypted and versioned;
- rotate/revoke credentials explicitly;
- never send tokens to Platform, Knowledge, Extractor, clients, events, or logs;
- do not request write scope merely because a read connection exists;
- audit external bookmark mutations.

Provider authorization codes, refresh tokens, and bearer headers must never appear in ordinary diagnostics.

## Provider identity

- Use provider IDs for X users, posts, media, and folders as namespaced external identities.
- Keep Ratatoskr internal IDs separate.
- Usernames/display names and post URLs are mutable attributes, not primary identity.
- Preserve post relation types such as reply, quote, and repost explicitly.
- Do not collapse a quoted/replied post into the bookmark record.
- Keep account-specific bookmark state separate from globally normalized post content.

## Bookmark timestamp semantics

The provider post `created_at` is not the time the user bookmarked it.

Unless X supplies an authoritative bookmark timestamp, use observation fields such as:

```text
first_observed_saved_at
last_observed_saved_at
observed_removed_at
```

Do not expose these as exact native `saved_at`/`unsaved_at` values.

If a future API begins supplying authoritative timestamps, add explicit authority/version fields and migrate through contracts rather than silently changing existing meaning.

## Incremental and full snapshot invariant

This is non-negotiable:

> Absence from a partial bookmark scan does not prove removal.

Correct behavior:

1. frequent scans may inspect the first N pages or otherwise bounded provider output;
2. observed bookmarks/posts/folders are upserted idempotently;
3. partial scans never mark unobserved bookmarks as removed;
4. periodic full snapshots enumerate the complete accessible bookmark set;
5. only a complete, successful full snapshot may mark missing bookmarks as removed;
6. rate-limited, truncated, failed, cancelled, or schema-invalid snapshots have no absence authority;
7. each inferred removal records the authoritative snapshot/run evidence;
8. folder membership uses equivalent conservative reconciliation.

A pagination optimization must preserve this invariant.

## Bookmark folder semantics

Native X folders and Ratatoskr collections are distinct.

- Persist X folder identity and account ownership.
- Reconcile folder listing and membership separately.
- Do not infer folder membership from local tags.
- Do not remove membership from a partial folder scan.
- Provider folder mutations require write scope, explicit intent, idempotency, and audit.
- A post may be in multiple local collections independent of X folder state; that local organization belongs outside this service.

## Synchronization and checkpoints

- Persist sync-run type: incremental, full, folder listing, folder membership, or compliance revalidation.
- Persist pagination tokens/checkpoints only after the corresponding batch is durably committed.
- Make reruns idempotent.
- Bound page count, items, time, concurrency, and usage/credit cost.
- Do not declare a full snapshot successful until every expected page is processed and validated.
- Preserve enough metadata to diagnose API response shape changes.
- Separate post content refresh from bookmark-state reconciliation when practical.

Provider pagination tokens are opaque. Never parse or synthesize them.

## Rate limits and usage/cost budgets

- Respect endpoint-specific and user/app limits.
- Honor reset and `Retry-After` signals.
- Bound concurrent requests per account and globally.
- Record provider request IDs and rate metadata required for diagnostics.
- Apply backoff and circuit breaking by failure class.
- Track usage/credit budget where the provider charging model requires it.
- Avoid refetching unchanged referenced posts/media without need.
- Do not convert rate-limit exhaustion into a successful partial snapshot.

A sync plan must be cost-aware and observable.

## Post normalization

Build a normalized social source from supported provider fields:

- author identity and display attributes;
- text, including long-form/note/article forms when available;
- publication timestamp;
- reply/quote/repost relations;
- entities and resolved URLs;
- media metadata;
- language and provider metadata where useful;
- upstream availability state;
- raw payload/blob reference when retained.

Rules:

- preserve the original provider payload separately from normalized projections when policy permits;
- do not treat rendered HTML from x.com as the canonical API source;
- do not discard unknown provider fields needed for future schema migration without a retention decision;
- do not download media or external article bodies without an explicit product/storage policy;
- never execute embedded links or content instructions.

## External links

A bookmarked X post and a linked article are separate sources.

Publish or request extraction with:

- expanded canonical URL;
- originating post ID;
- correlation/operation ID;
- user ownership;
- capture/sync provenance.

The X service does not fetch the article itself. Knowledge may later create a composite analysis that distinguishes claims made by the post from content in the linked article.

## Write-back operations

Supported writes, if implemented, must remain narrower than read sync.

- Require explicit user intent and separately granted write scope.
- Require an idempotency key and persist provider request/result evidence.
- Reconcile provider state after mutation.
- Return truthful partial success if local classification/indexing fails after the provider mutation succeeds.
- Do not automatically delete a provider bookmark because a local Ratatoskr item is removed.
- Do not retry an uncertain mutation blindly; resolve provider state first.
- Audit who/what requested the external write.

## Upstream compliance and availability

Model provider content state explicitly, for example:

```text
active
deleted
protected
author_suspended
unavailable
unknown
```

Rules:

- perform revalidation according to policy and provider requirements;
- preserve tombstone metadata needed to explain local disappearance;
- stop serving provider content when required by policy while retaining permitted audit/backup evidence;
- distinguish provider deletion from temporary API failure or lost scope;
- do not use a missing partial response as deletion evidence;
- publish availability/tombstone events for downstream projection/index cleanup.

## Field Theory legacy migration

Legacy SQLite imports are observations, not authoritative X API snapshots.

- Mark acquisition as `LegacyImport`/equivalent.
- Preserve original legacy IDs, timestamps, categories, and raw row evidence where safe.
- Do not equate legacy categories with native X bookmark folders.
- Deduplicate against provider post IDs and canonical URLs using documented precedence.
- Run a first successful full API snapshot before using authoritative removal semantics.
- Produce migration counts, conflicts, and unmapped records.
- Keep migration idempotent and restartable.

Do not delete the legacy source as part of import.

## Persistence and migrations

X writes only its owned schema.

Conceptual data includes:

```text
x_accounts
x_credentials
x_posts
x_post_relations
x_media
x_bookmarks
x_bookmark_folders
x_bookmark_folder_items
x_sync_runs
x_snapshots
x_tombstones
x_outbox
x_inbox
```

Rules:

- no cross-schema writes or foreign keys;
- uniqueness enforces account/provider object identity;
- post content revisions and account bookmark state are separable;
- snapshot completion and absence authority are transactional;
- migrations preserve observation/compliance history;
- secrets and large raw payloads are stored/referenced using protected mechanisms.

## Commands and events

Representative messages include:

```text
x.bookmarks.snapshot_requested.v1
x.bookmark.observed.v1
x.bookmark.removed.v1
x.folder.membership.changed.v1
social.source.upserted.v1
social.source.unavailable.v1
social.connection.reauth_required.v1
```

Use canonical contracts, transactional outbox, inbox deduplication, correlation/causation IDs, and at-least-once-safe handlers.

Never publish a removal event from an incomplete snapshot.

## Security and privacy

- Keep OAuth credentials inside this service and encrypted.
- Do not use browser-session cookies, password automation, or hidden consumer endpoints.
- Apply internal-user ownership to all account/bookmark/folder operations.
- Do not log private post bodies or raw responses by default.
- Treat posts, links, media metadata, and archived content as untrusted input.
- Do not let source content invoke tools or external writes.
- Redact provider errors before user display.
- Use least-privilege database and network access.
- Audit credential changes and provider mutations.

## Observability

Required telemetry should cover:

- sync type, duration, pages, and item counts;
- snapshot completeness and authority;
- additions/updates/removals;
- folder/list reconciliation;
- rate-limit and usage/credit state;
- provider latency/failure class;
- OAuth refresh/reauth state without token values;
- compliance revalidation/tombstones;
- linked-article requests;
- outbox/inbox lag and duplicates;
- correlation, account, sync-run, and operation IDs in non-sensitive form.

Avoid usernames/post text/URLs as ordinary metric labels.

## Testing expectations

When implementation exists, include applicable tests for:

- OAuth PKCE/state/scope/refresh/revoke behavior;
- token encryption and log redaction;
- provider ID normalization and post relations;
- observation timestamp semantics;
- partial scan never causing removal;
- complete snapshot reconciliation;
- failed/truncated/rate-limited snapshot preserving state;
- folder listing and membership reconciliation;
- pagination checkpoint restart/idempotency;
- rate-limit/backoff/credit budgets;
- write mutation idempotency and uncertain-result reconciliation;
- upstream deletion/protection/suspension states;
- linked article delegation;
- legacy Field Theory import and deduplication;
- outbox/inbox replay and migrations.

Use synthetic/redacted fixtures, not live personal accounts in normal tests.

## Cross-repository change rules

Use a workspace changeset when changing:

- social/event contracts;
- capture/result APIs consumed by Platform, web, mobile, extension, or Telegram;
- linked-article requests consumed by Extractor;
- analysis inputs consumed by Knowledge;
- auth/callback behavior;
- deployment secrets/scopes;
- Field Theory migration/cutover.

List producer/consumer compatibility, rollout, rollback, cost, re-sync/reindexing, and privacy impact.

## Git and PR workflow

- Separate sync correctness changes from unrelated client refactors.
- State whether a change affects partial or full snapshot authority.
- Include pagination/rate-limit fixtures and state-machine tests.
- Document scope and external-write changes.
- Do not add generic scraping, browser cookies, LLM calls, or local collections.
- Do not commit OAuth credentials or personal bookmark exports.
- Do not claim exact saved/removal timestamps without provider evidence.
- Update README/ADRs when provider capability or authority semantics change.

## Completion criteria

A task is complete only when:

- responsibility belongs to the X bounded context;
- read/write scopes and credentials remain minimal and isolated;
- posts, bookmarks, folders, and local collections remain distinct;
- observation timestamps are named truthfully;
- partial scans cannot cause false removals;
- full snapshot completion and authority are explicit;
- rate, retry, cost, checkpoint, and idempotency behavior is safe;
- external article extraction and Knowledge analysis remain delegated;
- upstream compliance states are handled explicitly;
- relevant repository and workspace tests pass;
- contracts, migrations, telemetry, and rollout are documented.
