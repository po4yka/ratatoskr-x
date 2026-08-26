# Design: post-normalization

## Context

Plan items 1–2 delivered the service scaffold, OAuth connection, and budget gates. `schema.sql` already defines `users`, `posts`, `post_relations`, and `media`, but `posts` lacks conversation linkage and public-metric counts, and none of the four tables stamps which parser produced its rows. Nothing reads or writes those tables yet; snapshot synchronization arrives in items 4–6 and will consume whatever records this change defines. The legacy Field Theory monolith derived tweets from GraphQL interception and articles from DOM scraping — both are excluded inputs here by repository policy. Payload shapes therefore come from the official X API v2 documentation, pinned by committed synthetic fixtures under `fixtures/x-api/`.

## Goals / Non-Goals

**Goals:**

- Pure, deterministic normalizers in a dedicated crate: payload envelope in, row-shaped records out, no I/O of any kind.
- An explicit, written unknown-field tolerance policy so provider evolution never silently drops evidence and never surprises a contributor.
- A parser-version stamp carried by every normalized record and persisted through the schema, making future re-parse campaigns selectable.
- Behavior pinned first by failing tests against committed fixtures, including the deleted-author edge.
- Property-based confidence over payload shape variation beyond hand-picked fixtures.

**Non-Goals:**

- Fetching, pagination, checkpointing, and snapshot authority (items 4–5).
- Database writers for the target tables; item 4 wires persistence onto these records.
- Event publishing and the SocialSource contract (item 7).
- Compliance/tombstone workflows beyond the typed deleted-author refusal (item 8).
- Media-byte download or storage anywhere in this crate.
- Bookmark observation timestamps; bookmark rows are not produced here.

## Decisions

### D1: New pure crate `crates/x-normalize`

The normalizers live in their own eighth workspace member depending only on `serde`, `serde_json`, and `chrono` (plus `proptest` as a dev-dependency). No HTTP client, no sqlx, no telemetry. Alternatives: modules inside `x-oauth` or `x-core` were rejected because the OAuth crate owns token transport concerns and `x-core` owns configuration; normalization is a distinct trust boundary — untrusted provider payloads in, trusted archive records out — and deserves its own dependency set.

### D2: Envelope DTOs mirror the official API v2 response envelope

The input type mirrors the documented response envelope: a data array of post objects, includes collections (users, tweets, media), and meta/errors containers as documented. Every known object deserializes through serde with an attached extension map capturing unrecognized members — see D12 for the policy statement. `deny_unknown_fields` was rejected deliberately: the provider evolves payloads continuously, AGENTS.md forbids discarding unknown provider fields without a retention decision, and a strict DTO would turn every provider addition into a production incident. Refusal is reserved for structural violations of documented required members.

### D3: Outputs are row-shaped records keyed by provider identity

Normalization emits `NormalizedUser`, `NormalizedPost`, `NormalizedRelation`, and `NormalizedMedia` whose fields map one-to-one onto the target table columns minus database-owned columns (uuid surrogate keys, `created_at`/`updated_at` defaults). All cross-record links use provider ids, never uuids: relation targets may dangle because `post_relations.related_post_provider_id` is intentionally an unconstrained text column, and media links to posts by provider id within the emitted set.

### D4: Parser versioning

A single crate constant (`PARSER_VERSION`, integer, starting at 1) is stamped into every record at construction. The schema gains `parser_version integer NOT NULL` on `users`, `posts`, `post_relations`, and `media`. Any future change to mapping semantics bumps the constant; rows retain the version that produced them, so retained raw payloads can be re-parsed selectively under a newer parser. A round-trip test pins that serialization preserves the stamp.

### D5: Author resolution is batch-wide and refuses atomically

Authors resolve exclusively against the user objects carried by the same envelope, keyed by provider id. A post whose author id has no match fails the whole envelope with `NormalizeError::UnresolvedAuthor` naming the affected post's provider id; no placeholder author is synthesized and no partial output escapes. The alternative — skipping bad posts and emitting the rest — was rejected because silently thinning an archive batch invites exactly the quiet gaps this bounded context exists to prevent; the sync layer of item 4 sees the typed refusal and decides tombstone versus retry policy above this layer. The committed deleted-author fixture pins this edge.

### D6: Relations map the closed vocabulary and tolerate dangling targets

`referenced_tweets` entries map by type: replied-to → `reply`, quoted → `quote`, retweeted → `repost`. Relations emit regardless of whether the referenced post appears in the same envelope. A reference type outside the vocabulary is preserved through the extension policy on the owning post and emits no relation row, since the schema CHECK admits only the three documented values. Emitted relations sort deterministically by `(related_post_provider_id, relation)`.

### D7: Media is metadata only

A media attachment normalizes provider key, kind (`photo` | `video` | `animated_gif`), dimensions where supplied, alt text where supplied, video duration, and preview/display URLs into its metadata record. The blob reference stays unset; nothing in the crate performs or implies a byte download. Post records carry their media keys so item 4 can join without re-parsing payloads.

### D8: Counts are nullable, absence is not zero

Documented public-metric members (`like_count`, `retweet_count`, `reply_count`, `quote_count`, `bookmark_count`, `impression_count`) become nullable bigint columns on `posts`; an absent member stays NULL. Zero would assert an observed value the provider never stated. Consent-gated metric variants (non-public, organic) ride the preservation policy like any other member.

### D9: Conversation linkage copies verbatim

The post's conversation id copies unchanged into `posts.conversation_provider_id`. It is linkage evidence, not a foreign key; thread reconstruction joins on it without trusting referential integrity.

### D10: Timestamps stay honest

`published_at` parses strictly from the documented publication timestamp format. `edited_at` stays unset unless the provider documents an exact edit timestamp in the payload; edit-history identifiers are preserved instead of being converted into invented times. Long-form note text lands in `long_text` while the canonical short text remains untouched, matching AGENTS.md's truthful-naming rule.

### D11: Article-backed posts normalize only documented structure

An article-backed post normalizes from officially documented members only. Any article wrapper beyond the currently documented surface rides the record-and-preserve path rather than receiving invented structure; the committed fixture encodes exactly the documented surface so the test proves the honest route. When the provider later documents richer article members, extending the DTO is a normal change with a parser-version bump.

### D12: Unknown-field tolerance policy — record-and-preserve, stated explicitly

Unrecognized members of known payload objects are captured, name and value intact, into the extension material of the corresponding normalized record. They are never dropped silently and never cause rejection. Rejection happens only when a documented required member is absent or unparseable — a post without a provider id, an unparseable timestamp — and takes the form of a typed error carrying the offending object and member. This policy is a spec requirement, not an implementation accident, so future contributors extend DTOs deliberately: add a field when semantics exist, otherwise let preservation hold the evidence.

### D13: Property-based tests over payload shape variation

`proptest` drives generated shape variations over the committed-fixture grammar: optional members omitted, arrays reordered or emptied, unicode text, injected unknown keys. Invariants: normalization never panics; repeated normalization is identical; injected unknown keys always survive into extension material; failures are always typed errors. Hand-written fixtures keep semantic truthfulness; properties cover the combinatorial space around them.

### D14: Included tweets are relation evidence, not records

Only `data[]` posts become normalized post records; `includes.tweets` entries (embedded quoted or retweeted posts) stay evidence that justifies relation targets, and their authors are never demanded from `includes.users`. This keeps batch refusal semantics clean — an envelope is never held hostage by a third party's missing author — and matches the fetcher's job (item 4) to request full hydration when the archive wants the quoted content itself. Included users and media participate normally: `includes.users` feeds the author map, and `includes.media` joins to owning posts through `attachments.media_keys`; attachment keys with no matching media object are tolerated and skipped because their evidence survives raw-payload retention.

### D15: Where preserved material persists

The normalized records carry extension material in memory, and the schema intentionally grows no extension columns for it: durable preservation happens through raw-payload retention behind `posts.raw_blob_ref` when item 4 wires persistence, per AGENTS.md's preserve-provider-evidence rule. Media is the exception that folds inward — its preserved unknown members merge under a reserved `extension` key inside the existing `metadata` jsonb, next to the documented dimensions, alt text, duration, URLs, and variants. Relations whose reference type falls outside the closed vocabulary are copied verbatim into the owning post's extension under the reserved key `ratatoskr.x/unresolved_references` so an unmapped type is never silently dropped.

## Risks / Trade-offs

- [Official documentation drifts from provider reality] → Fixtures are committed and reviewable; the parser-version bump path makes future remapping selectable instead of destructive.
- [Whole-envelope refusal holds good posts hostage to one bad post] → Deliberate: partial emission hides defects; the sync layer can split envelopes once it exists.
- [Extension maps could grow large] → Capture applies to the documented top-level objects now; nested recursive capture is a deliberate future extension, noted here so its absence is a decision, not an oversight.
- [Nullable counts invite null-versus-zero confusion downstream] → The spec fixes absence-is-not-zero; upsert semantics arrive with item 4 under the same rule.

## Migration Plan

None. Development status forbids migrations: `schema.sql` is edited in place and disposable test databases rebuild from it. Rollback is reverting the branch; no deployed data exists.

## Open Questions

None blocking. Exact payload member names are confirmed against the official documentation when fixtures are recorded, and the task ordering places fixture recording ahead of every assertion that consumes them.
