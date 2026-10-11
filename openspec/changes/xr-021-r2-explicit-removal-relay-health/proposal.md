## Why

Changeset XR-021 round 2 fixes three defects that independent verifiers traced through the pushed code. A provider deletion found while re-capturing a post the owner already preserved only fails the new operation, so the owner's `explicit_sources` row and Knowledge's derived state stay (R2-09). A relay that keeps running but cannot publish, because of an ACL drift or a body above the broker limit, leaves the service ready (R2-03, R2-04). The README promises a transport that already exists and no operator example ships (R2-10). This change cites XR-021 CONTRACTS.md sections R2-09, R2-03, R2-04 and R2-10 of the Round 2 amendments.

## What Changes

- When the public resolution answers deleted for a capture whose `(owner, provider_post_id)` already has an `explicit_sources` row, the terminating transaction deletes the row and queues one `social.source.removed.v1` envelope (reason `retention_policy`, because the closed `RemovalReason` has no provider-deleted variant and adding one is a contract change), and still reports `failed social.source.deleted`. Inaccessible and transient outcomes remove nothing.
- `RelayHealth` in `x-capture` becomes unhealthy once the oldest publishable unpublished row has `attempt_count >= 3` (or is older than 300 seconds) after a failed publish, and healthy again after a clean or empty pass. The service feeds it into `/health/ready` without stopping the relay. The relay classes a body larger than the connected server's `max_payload` as undeliverable (`undeliverable_at`, class `payload_too_large`), counts it, and continues with the next row. `x_archive.outbox_events` gains `undeliverable_at timestamptz` in `schema.sql`, edited in place.
- `deploy/systemd/x.conf.example` ships the operator example, a test loads it through the configuration loader, and the README says what runs and that the bookmark and compliance lane is dormant.

## Capabilities

### Modified Capabilities

- `explicit-capture`: a provider deletion removes the owner's source and tells Knowledge.
- `outbox-relay`: relay health reaches readiness and an oversize body is undeliverable.
- `x-archive-schema`: `outbox_events.undeliverable_at`.

## Impact

- Affected code: `x-sync` public capture worker, `x-capture` relay, `x-persistence` schema, `services/x` wiring and example, README and DEVELOPMENT.
- Breaking: none for a consumer; `schema.sql` changes in place (no migration, per the development status).
- Out of scope: the bookmark, OAuth and compliance lane, live X API calls, other providers, a provider-deleted removal reason (a contracts change) and periodic revalidation of explicit sources.
