# Post normalization

## Why

Implementation plan item 3 of `docs/IMPLEMENTATION_PLAN.md` calls for post/author/relation/media normalization. The legacy Field Theory monolith built its records by intercepting GraphQL traffic and scraping X Articles out of rendered DOM pages; this service must instead derive every stored fact from official API payloads so the archive is lawful, stable, and re-derivable. Snapshot synchronization (items 4–6) consumes normalized records as its input format, so the mapping must exist and be pinned by tests before any fetching lands.

## What Changes

- Add a new crate `crates/x-normalize` holding typed DTOs for the official X API v2 post/user/media payload shapes plus pure normalizers that map them onto row-shaped records for `x_archive.users`, `x_archive.posts`, `x_archive.post_relations`, and `x_archive.media`.
- Normalize post content: text, long-form note text into `long_text`, language, publication timestamp, edit metadata, public metric counts, and conversation linkage.
- Resolve authors exclusively from user objects carried by the same payload envelope; a post whose author cannot be resolved yields a typed error (the deleted-author edge) instead of an invented author.
- Map `referenced_tweets` to the closed relation vocabulary `reply`/`quote`/`repost`; relations are recorded even when the related post is not part of the batch, keyed by provider id.
- Normalize media metadata only — provider id, kind, dimensions, alt text, duration — never media bytes; blob references stay untouched at this layer.
- Stamp every normalized record with a parser version, and persist that stamp through new `parser_version` columns on the four target tables, so retained payloads can be re-parsed cleanly when the parser evolves.
- Fix the unknown-field tolerance policy explicitly as record-and-preserve: unrecognized payload fields are captured into extension maps that travel with the normalized records rather than rejected or silently dropped; rejection is reserved for structural violations of known required fields.
- Extend `schema.sql` in place: `posts` gains `conversation_provider_id` and the public-metric count columns; `posts`, `users`, `media`, and `post_relations` gain `parser_version`.
- Commit synthetic fixture payloads under `fixtures/x-api/`: a single tweet, a thread, a quote, an article-backed post, and the deleted-author edge.
- Add property-based tests over payload shape variations proving normalization never panics, is deterministic, and preserves unknown fields.

Out of scope: bookmark pagination and snapshot runs (plan items 4–5), event publishing (item 7), persistence writers that fill these tables during sync, folder handling, compliance revalidation, write-back.

## Capabilities

### New Capabilities

- `post-normalization`: how official API payload DTOs become normalized author, post, relation, and media metadata records — determinism, the unknown-field tolerance policy, parser-version stamping, author resolution failure, relation fidelity including dangling references, media-metadata-without-bytes, and honest timestamp semantics.

### Modified Capabilities

- `x-archive-schema`: the owned table inventory keeps seventeen tables but `posts` grows conversation and public-metric columns, and the four normalization-target tables carry a `parser_version` column stamping which parser produced each row.

## Impact

- New crate `crates/x-normalize` becomes the eighth workspace member; it depends only on `serde`/`serde_json`/`chrono` and adds `proptest` as a dev-only dependency.
- `schema.sql` edited in place; the inventory and constraint tests in `crates/x-persistence/tests/schema.rs` extend to cover the new columns.
- No HTTP surface, OAuth, budget, or service-binary behavior changes. Nothing is published to other repositories; the SocialSource contract remains item 7 work.
