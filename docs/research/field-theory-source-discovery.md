# Field Theory legacy source discovery

Research date: 2026-08-28
Primary public source snapshot: `afar1/fieldtheory-cli@4b4a0f677e7077ac76848408b22091fc34330391`

## Result

The public source is sufficient to define a deterministic synthetic import fixture and the exact
Field Theory SQLite index shape. It is not sufficient to import the owner's real archive or infer
which Ratatoskr/X account owns it:

- Field Theory stores its raw bookmark cache in `bookmarks.jsonl` and builds `bookmarks.db` as a
  derived SQLite FTS5 index. The canonical directory is `~/.fieldtheory/bookmarks`; older installs
  may use `~/.ft-bookmarks`, and `FT_DATA_DIR` may point anywhere else
  ([README data layout](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/README.md#L205-L235),
  [path resolution](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/src/paths.ts#L5-L28),
  [artifact paths](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/src/paths.ts#L99-L125)).
- Neither the JSONL record nor the SQLite `bookmarks` table contains a bookmark owner, authenticated
  X user ID, Ratatoskr user ID, or tenant ID. Author identity is the author of the saved post, not
  the account that saved it
  ([record type](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/src/types.ts#L55-L102),
  [SQLite schema](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/src/bookmarks-db.ts#L234-L294)).
- Therefore owner mapping must be an explicit, owner-approved import input selecting an existing
  Ratatoskr account, and that account's `provider_user_id` must be verified against the current
  OAuth user-context identity returned by `GET /2/users/me`. X documents `/me` as the way to obtain
  the authenticated user's ID, and the bookmarks path requires the path ID to equal that
  authenticated user
  ([authenticated-user lookup](https://docs.x.com/x-api/users/lookup/quickstart/authenticated-lookup),
  [Get Bookmarks reference](https://docs.x.com/x-api/users/get-bookmarks)).
- No public repository contains the owner's real bookmark rows. Public Field Theory fixtures are
  synthetic/redacted source examples, not an archive. On this machine both documented default data
  directories were absent during this research. An `FT_DATA_DIR` override or an owner-provided
  read-only copy remains possible, but cannot be discovered from public source.

Real cutover therefore remains owner-gated. Research closes the schema/semantics question, not the
private-data or approval prerequisites.

## SQLite dependency decision

The importer reuses the workspace-pinned `sqlx 0.8.6` dependency and enables its existing `sqlite`
feature; it does not add a second database library or an unpinned production dependency. SQLx is
dual MIT/Apache-2.0 licensed, and its `libsqlite3-sys 0.30.1` dependency is MIT licensed, compatible
with this BSD-3-Clause repository. The bundled SQLite build adds the existing `cc` build dependency
to `Cargo.lock`, so Linux and macOS release builds remain part of the mandatory full gate. The
maintenance and security surface is the pinned SQLx/SQLite stack already visible to Cargo tooling;
`cargo deny --locked check` is required before delivery. Runtime access uses one immutable,
read-only, no-create connection and never opens a source with write authority.

## Public Field Theory storage contract

### Artifact authority

The repository describes `bookmarks.jsonl` as the raw cache and `bookmarks.db` as the SQLite FTS5
search index. The implementation confirms that `buildIndex` reads every JSONL `BookmarkRecord`,
initializes or updates SQLite, inserts/replaces records, and rebuilds FTS
([README](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/README.md#L205-L225),
[index build](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/src/bookmarks-db.ts#L385-L470)).

Implications for import:

1. Prefer a supplied `bookmarks.jsonl` as source evidence when both artifacts exist.
2. Accept `bookmarks.db` read-only as the retired monolith did, but identify it as a derived index.
3. Never read or import `oauth-token.json`, browser cookies, session state, or authorization
   material. The public project lists the OAuth token next to the archive and explicitly treats it
   as a password
   ([security notes](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/README.md#L293-L301)).

### SQLite schema at the pinned commit

`bookmarks.db` has application schema version `6`, stored in `meta(key, value)`, plus one content
table and its FTS5 external-content table
([schema definition](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/src/bookmarks-db.ts#L10-L10),
[tables and indexes](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/src/bookmarks-db.ts#L234-L294)).

The 37 `bookmarks` columns, in insertion order, are:

```text
id                      TEXT PRIMARY KEY
tweet_id                TEXT NOT NULL
url                     TEXT NOT NULL
text                    TEXT NOT NULL
author_handle           TEXT
author_name             TEXT
author_profile_image_url TEXT
posted_at               TEXT
bookmarked_at           TEXT
synced_at               TEXT NOT NULL
conversation_id         TEXT
in_reply_to_status_id   TEXT
quoted_status_id        TEXT
language                TEXT
like_count              INTEGER
repost_count            INTEGER
reply_count             INTEGER
quote_count             INTEGER
bookmark_count          INTEGER
view_count              INTEGER
media_count             INTEGER DEFAULT 0
link_count              INTEGER DEFAULT 0
links_json              TEXT
tags_json               TEXT
ingested_via            TEXT
categories              TEXT
primary_category        TEXT
github_urls             TEXT
domains                 TEXT
primary_domain          TEXT
quoted_tweet_json       TEXT
article_title           TEXT
article_text            TEXT
article_site            TEXT
enriched_at             TEXT
folder_ids              TEXT
folder_names            TEXT
```

Indexes exist on `author_handle`, `posted_at`, `language`, `primary_category`, and
`primary_domain`. `bookmarks_fts` indexes `text`, `author_handle`, `author_name`, and
`article_text` using `porter unicode61`, with `bookmarks` as its external content table
([exact DDL](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/src/bookmarks-db.ts#L234-L293)).

The retired monolith only depended on this seven-column subset:

```sql
SELECT id, url, text, author_handle, primary_category, posted_at, synced_at
FROM bookmarks
ORDER BY synced_at;
```

That subset is confirmed by the archived retired-monolith checkout at local commit
`5cdc911b9d33a426613eed79b1ef36db041693e3`, path
`app/adapters/ingestors/x_bookmarks_ingestor.py:31-55`. The former public URL
`https://github.com/po4yka/ratatoskr` and fixed-commit blob URLs returned HTTP 404 on the research
date, so this monolith evidence is locally reproducible but is **not** claimed as a currently public
web source.

## Record identity and field semantics

### Stable identity

For both supported ingestion paths, Field Theory assigns the provider post ID to both `id` and
`tweetId`; the canonical post URL is derived from the author handle plus that ID
([official API normalization](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/src/bookmarks.ts#L103-L124),
[GraphQL normalization](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/src/graphql-bookmarks.ts#L345-L378)).

Import identity precedence should consequently be:

1. valid `tweet_id`/`id` as the X provider post ID;
2. a post ID parsed from an `x.com` or `twitter.com` status URL when the explicit ID is absent or
   malformed, recording the conflict rather than silently replacing evidence;
3. canonical normalized URL only as a fallback/deduplication hint, never as proof of account owner.

The archived monolith used `bookmark_external_id` for update identity, then normalized URL/dedupe
hash to attach metadata to an existing request. Its stored sidecar was:

```text
request_id             integer primary key, foreign key to requests
bookmark_external_id   text, unique index
x_category             text
tweet_text             text nullable
tweet_text_tsv         generated English tsvector
tweet_author           text nullable
tweet_url              text
posted_at              timestamptz nullable
synced_at              timestamptz
```

Local evidence: retired-monolith commit `5cdc911b9d33a426613eed79b1ef36db041693e3`,
`app/db/alembic/versions/0022_add_x_bookmark_metadata.py:47-92` and
`app/adapters/ingestors/x_bookmarks_ingestor.py:220-301`. This metadata schema contains no normalized
post relations, media, provider account ID, or owner identity.

### Timestamps and snapshot authority

- `postedAt`/`posted_at` is post publication time.
- `syncedAt`/`synced_at` is the Field Theory observation/index refresh time.
- `sortIndex` is an opaque ordering key, explicitly not a timestamp.
- `bookmarkedAt` is nullable and cannot be treated as an authoritative native saved time. The
  official API normalizer deliberately sets it to `null` because the endpoint exposes post
  creation, not bookmark creation
  ([record comment](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/src/types.ts#L55-L68),
  [API normalizer](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/src/bookmarks.ts#L109-L123)).

This is a corrected contract, not merely a conservative interpretation. Commit
[`315b5b8dbce6da11a5616860dc470b7bcfb1f096`](https://github.com/afar1/fieldtheory-cli/commit/315b5b8dbce6da11a5616860dc470b7bcfb1f096)
removed the conversion of GraphQL `sortIndex` into a fabricated `bookmarkedAt`, preserves the key
as opaque ordering evidence, and clears `bookmarkedAt` on GraphQL records. Imported Field Theory
rows must therefore become legacy observations. They cannot authorize removal, prove exact save
time, or serve as a complete current snapshot.

### Categories, folders, and post normalization

`BookmarkRecord` carries post/author snapshots, relations, engagement counters, media, links,
enrichment, `ingestedVia`, and parallel X folder ID/name arrays
([full type](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/src/types.ts#L1-L102)).
The SQLite builder additionally preserves local classification/enrichment fields when rebuilding
from JSONL
([preservation and mapping](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/src/bookmarks-db.ts#L365-L433)).

Consequently:

- `primary_category`, `categories`, `domains`, `tags_json`, and enrichment fields are Field Theory
  projections, not native X folder authority;
- only `folder_ids` plus matching `folder_names` represent Field Theory's mirror of native X folder
  membership, and even those remain legacy observations without full-snapshot authority;
- quoted posts remain separate normalized posts/relations; `quoted_tweet_json` is evidence for that
  projection, not bookmark identity;
- linked article content must not be collapsed into the bookmarked post.

### Version and provenance

Public Field Theory exposes three different concepts:

- JSONL cache metadata `schemaVersion: 1`
  ([OAuth writer](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/src/bookmarks.ts#L229-L237),
  [GraphQL writer](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/src/graphql-bookmarks.ts#L806-L816));
- SQLite index `meta.schema_version = 6`
  ([index schema](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/src/bookmarks-db.ts#L10-L10),
  [meta write](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/src/bookmarks-db.ts#L283-L293));
- per-record `ingestedVia` in the closed vocabulary `api | browser | graphql`
  ([record type](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/src/types.ts#L81-L89)).

There is **no per-record parser-version or legacy provenance field**. `ingestedVia` identifies an
acquisition route, not a parser release. A truthful Ratatoskr import should therefore record all of:

1. Ratatoskr acquisition/provenance = `legacy-import`;
2. Ratatoskr import parser version = an importer-owned constant tested against fixtures;
3. source format = `field-theory-jsonl-v1` or `field-theory-sqlite-v6` when verified;
4. source implementation evidence = pinned Field Theory commit when known;
5. original legacy ID, timestamps, category/folder values, and a safe raw-row digest/reference.

It must not relabel `ingestedVia=graphql` as provider-authoritative or invent an absent Field Theory
parser version.

## Owner/account mapping

### What can be proven

The current Ratatoskr schema intentionally separates `internal_user_id` from X
`provider_user_id`; both live on `x_archive.accounts`
([current schema at the source-discovery base](https://github.com/po4yka/ratatoskr-x/blob/26d29decaa6940d3b86d32fb31ddaf29cfe961b7/schema.sql#L8-L19)).
Field Theory's OAuth implementation resolves its current API user via `/2/users/me`, but it does not
persist that user ID into each bookmark row
([Field Theory user resolution](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/src/bookmarks.ts#L80-L100),
[record type](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/src/types.ts#L55-L102)).

The archived monolith does not close the gap. Its bookmark importer created or updated requests and
`x_bookmark_metadata` without assigning `Request.user_id` or `Request.chat_id`. Those nullable
columns therefore do not establish ownership for imported rows. Local evidence:
`app/adapters/ingestors/x_bookmarks_ingestor.py:277-301` and
`app/db/models/core.py:429-443` at retired-monolith commit `5cdc911b9d33a426613eed79b1ef36db041693e3`.

### Safe mapping procedure

The only non-fabricated procedure supported by the evidence is:

1. Require `target_account_id` as explicit import input and record owner approval; do not choose the
   only account automatically.
2. Load that account's `internal_user_id` and stable `provider_user_id`.
3. Through the existing current OAuth connection, resolve `GET /2/users/me` and require its `id` to
   equal the stored `provider_user_id`. Do not read or import a legacy token to perform this check.
4. Bind every imported observation and shadow comparison to that account and owner; retain the
   approval/mapping evidence separately from source rows.
5. Refuse before writes on no approval, no active current OAuth identity, mismatch, multiple
   candidate accounts, or any attempt to infer owner from post authors, handles, URLs, legacy
   request IDs, filesystem ownership, cookies, or tokens.

This validates the current target account. It cannot prove historically which account produced an
unlabelled archive. That remaining historical assertion requires the owner's explicit approval.

## Public fixtures and source availability

Publicly available, safe fixture material at the pinned Field Theory commit:

- three inline synthetic `BookmarkRecord` rows covering authors, timestamps, engagement, media,
  links, and `ingestedVia`, plus a refresh/idempotence test
  ([SQLite index tests](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/tests/bookmarks-db.test.ts#L10-L82));
- a redacted GraphQL bookmark-feed payload with a note tweet
  ([fixture](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/tests/fixtures/bookmark-feed-note-tweet.json));
- a redacted tweet-by-ID payload used for quoted/long-form parsing
  ([fixture](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/tests/fixtures/tweet-result-by-rest-id-note-tweet.json));
- parser tests that construct synthetic GraphQL responses and verify provider post identity,
  relations, media, links, and opaque `sortIndex`
  ([tests](https://github.com/afar1/fieldtheory-cli/blob/4b4a0f677e7077ac76848408b22091fc34330391/tests/graphql-bookmarks.test.ts#L28-L140)).

No `.db`, complete `bookmarks.jsonl`, or personal bookmark export is committed in the public tree.
The tree contains only the two JSON payload files above under `tests/fixtures`. Therefore a test
SQLite must be generated from the public DDL and synthetic rows; it must not be presented as a real
owner archive.

The archived monolith additionally contains a minimal seven-column SQLite DDL and two synthetic
rows specifically testing idempotent Field Theory ingestion at
`tests/adapters/ingestors/test_x_bookmarks_ingestor.py:322-412` on local commit
`5cdc911b9d33a426613eed79b1ef36db041693e3`. It is useful corroborating fixture design, but is not a
currently accessible public source.

## Implementation consequences for plan item 10

The discovered contract supports implementation without private data:

- import both the seven-column retired-monolith subset and the full Field Theory SQLite v6 table;
- optionally prefer JSONL v1 when an owner supplies it;
- key normalized posts by provider post ID, retain URL conflicts and unmapped rows in the report;
- preserve legacy classification/folder evidence without upgrading its authority;
- stamp importer-owned parser version and `legacy-import` provenance;
- make a second identical fixture import a no-op;
- compare shadow sets primarily by provider post ID, with canonical URL as a reported secondary
  match, and report content/category/folder differences separately;
- never let an imported absence remove a bookmark; only the first complete successful official API
  snapshot can carry absence authority;
- require the explicit verified account mapping above before any real import;
- require owner approval of the generated cutover checklist before switching reads or stopping the
  legacy job.

Still required from the owner for a real run:

1. a read-only `bookmarks.db` or `bookmarks.jsonl`/redacted export (possibly from an `FT_DATA_DIR`
   override), and/or a retired-monolith data extract containing actual `x_bookmark_metadata` rows;
2. the exact target Ratatoskr account ID and approval that the unlabelled archive belongs to it;
3. approval of the completed shadow diff and cutover/rollback checklist.
