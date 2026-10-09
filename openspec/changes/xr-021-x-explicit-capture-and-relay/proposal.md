## Why

Changeset XR-021 fixes cross-repository integration defects. For X, an explicit browser capture that Platform accepts today only inserts an `x_archive.explicit_captures` row that has no owner and that nothing reads: the service resolves nothing, publishes nothing and reports nothing to Platform. The library `ExplicitCaptureService` published the shared per-provider-id `x_archive.posts` row to any tenant naming the permalink, a reproduced tenant-isolation leak. Bookmark, compliance and article outbox rows are bare payloads that nothing relays, and a stopped bus consumer is only logged while the service stays ready. This change cites XR-021 CONTRACTS.md sections S02, S03, S05, S10 (CD1 to CD7 and the X linked-article path) and S11.

## What Changes

- Move every `ratatoskr-*` contracts dependency to the XR-021 contracts commit `ad16855c4e7f3d52cd118274faa3b8f3ab4da576`.
- Move the operator listener default from `127.0.0.1:8080` (the Edge public port) to `127.0.0.1:9087` (S05). **BREAKING** for any deployment relying on the old default.
- Every `x_archive.outbox_events` row stores the complete canonical envelope, keyed by the envelope id: `EventEnvelope` for `platform.operation.reported.v1`, `social.source.captured|updated|removed.v1`, and `CommandEnvelope` carrying `ContentCaptureRequested` for `content.capture.requested.v1` (S02, S11). The table gains a closed `event_type` CHECK and publication attempt columns. **BREAKING**: `schema.sql` changes in place.
- Explicit capture becomes an owner-scoped lane: intake keeps the tenant as owner, the operation and the provider post id, queues the operation with a `queued` report and answers an unmappable permalink with a terminal `failed` report. A worker resolves the post through the public app-only X API, retries transient failures on a bounded schedule, publishes `explicit_sources` rows and owner-scoped social events, and reports exactly one terminal outcome (S10 CD1, CD2, CD5, CD6).
- `ExplicitCaptureService` is deleted. **BREAKING**: it is the tenant-isolation leak.
- One `OutboxRelay` publishes the five allowed event types to JetStream with `Nats-Msg-Id` equal to the row id, marks `published_at` only after the acknowledgement, and backs off a failing row without starving later rows (S02).
- The service consumes extractor operation reports through the `ratatoskr_x_extractor_reports` durable and completes linked-article captures (S04, S10).
- A stopped bus task flips readiness and ends the process with a failure code; the service refuses to start with a bus and no public-capture bearer token (S02, S10 CD5). **BREAKING**: a new required secret file.

## Capabilities

### New Capabilities

- `explicit-capture`: the owner-scoped explicit browser-capture lane: intake, public resolution, retry, publication and exactly-once reporting.
- `outbox-relay`: complete-envelope outbox rows, the relay to JetStream, extractor-report consumption and bus-task lifecycle.

### Modified Capabilities

- `x-archive-schema`: explicit captures become owner-scoped, `explicit_sources` is added, and outbox rows are keyed by envelope id with a closed type list.
- `social-source-publication`: an explicit capture publishes only what the public resolution returned for its owner.

## Impact

- Affected code: `x-core` configuration, `x-persistence` schema and outbox helper, `x-capture` intake and relay, `x-sync` publication, public resolver and worker, `x-http` readiness, `services/x` wiring.
- The Platform contract is unchanged: this service reads the existing `social.capture.requested.v1` command and answers with `platform.operation.reported.v1`.
- External dependency the owner must provision: an X developer app with read access and its bearer token file. Without it the service refuses to start, by design.
- Out of scope: mobile social capture routing, the `partially_succeeded` linked-article branch of the extension, Knowledge ingestion, account erasure for the new tables, the credential-free oEmbed fallback.
