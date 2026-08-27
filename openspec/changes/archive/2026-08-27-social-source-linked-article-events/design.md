## Context

See proposal.md. X already persists normalized posts, bookmark observations, generic outbox/inbox
records, and snapshot page transactions, but it has no account owner column for shared contracts,
no SocialSource revision state, and no article-capture aggregate. The extractor accepts its existing
capture JSON envelope and preserves the caller's `correlation_id` in both document and operation
outcomes.

## Goals / Non-Goals

**Goals:**

- Consume one immutable contracts revision with nominally identical identifier and envelope types.
- Derive state-carried SocialSource events from both supported bookmark and explicit capture paths.
- Deduplicate linked-article work per owner and normalized URL, and project terminal IR results back
  to every source post.

**Non-Goals:**

- Publishing crates, changing extractor behavior or storage, fetching linked articles in X,
  analysis, provider bookmark writes, browser session automation, migration files, or a blob store.

## Decisions

### D1: Use one reviewed git revision for every direct contracts crate

The workspace pins social, envelope, identifiers, document, and operation crates to the same
reachable ratatoskr-contracts SHA. This preserves Rust nominal type identity and avoids crates.io
release coupling. A fixture-conformance test serializes the exact event types; the committed lock
records the source SHA. Local path dependencies are rejected because they would make a build depend
on a developer checkout.

### D2: Social sources are account-library records, not global post fields

`social_sources` is unique by account and post, has a generated SocialSourceId, and carries the
current semantic digest. `social_source_revisions` retains emitted revisions. This reflects that
the same X post may enter more than one user's library and permits captured-versus-updated decisions
without mutating provider identity.

### D3: Explicit capture uses the existing social command contract

The inbound adapter validates `social.capture.requested.v1`, accepts only provider X, and invokes
the same source transition with the command's acquisition and saved authority. Bookmark snapshots
always set official API / authoritative platform state themselves. This supplies a real explicit
capture lane without pretending a browser capture is an X bookmark.

### D4: Article capture is an X-owned aggregate correlated through Extractor

An `article_captures` row is unique on owner and canonical normalized URL. `post_article_links`
records every source post. The outbox command carries `article_capture:<id>` as correlation and its
stable operation/idempotency values. Extractor's documented JSON protocol and generic operation
report remain an adapter boundary; X validates only the shared envelope and result reference before
recording the returned DocumentId and BlobRef.

### D5: Commit state and outgoing work in one transaction

Every source revision and article capture is stored with its outbox row. Incoming terminal outcomes
use the inbox id as a deduplication key. This preserves at-least-once safety without inferring a
successful extraction from a queued command.

## Risks / Trade-offs

- [Pinned git revision is unreachable later] → lock the exact SHA, verify it is an origin/main
  ancestor before updating, and retain Cargo's resolved source in Cargo.lock.
- [Extractor JSON is not a typed shared command contract] → isolate serialization and decoding in
  one adapter, validate the existing envelope and report shape, and do not export it as X API.
- [URL equivalence differs from Extractor routing] → use the same conservative canonicalization
  rules for scheme, host, default ports, fragments, and documented tracking parameters; retain the
  original URL as evidence.
- [A partial snapshot causes duplicate event work] → source and capture semantic uniqueness live in
  the transaction; partial scans never use absence authority.

## Migration Plan

The current schema definition changes in place and disposable test databases are created from it.
Deploy the schema and service together; rollback is the previous application plus a freshly created
development database. No migration framework or compatibility path is introduced.
