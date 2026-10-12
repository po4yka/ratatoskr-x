## Context

The authoritative cross-repository text is XR-021 CONTRACTS.md R5-11. This document records only decisions internal to `ratatoskr-x`.

## Decisions

### State lives on the outbox row

A removal row moves through: unpublished, published and armed (`next_resend_at` set), then resent any number of times (`resend_count`, `next_resend_at` pushed out), then acknowledged (`acknowledged_at`, `ack_outcome`). No second table exists; the row id is the event id, so the acknowledgement's `acknowledged_event_id` finds the row by primary key, and the tenant check compares the acknowledgement's `owner` with the stored envelope's `tenant_id`.

### The relay owns publishing, the persistence crate owns transitions

`x-persistence::outbox` holds the SQL transitions (`mark_published`, `record_resend`, `acknowledge`, `unacknowledged_removals`) so the relay and the consumer share one definition of each. `mark_published` takes the resend delay and arms `next_resend_at` only for `social.source.removed.v1`; `record_resend` computes `now() + least(resend_after * 2^resend_count, cap)` from the pre-update `resend_count` and clamps the exponent so the arithmetic cannot overflow. The relay selects due resends with `published_at is not null and acknowledged_at is null and next_resend_at <= now() and next_attempt_at <= now()` and republishes `payload::text`, the same bytes the first publish sent. A failed republish records the existing failure backoff on `next_attempt_at` and leaves `published_at`, `next_resend_at` and `resend_count` alone, so it neither counts toward relay health (which only reads unpublished rows) nor advances the resend schedule.

### Resend runs after the fresh rows and stops at a refusing broker

A pass publishes due unpublished rows first. When none of those failed, it republishes due removals, oldest due first, and stops at the first failed republish, as the fresh loop does. A broker that refuses one publish will refuse the next, so the pass does not hammer it.

### Configuration is a section of its own

`tombstone.resend_after_seconds` and `tombstone.resend_cap_seconds` follow the strict figment tree. The minimum of 30 seconds is a configuration rule; tests construct `ResendPolicy` directly with millisecond delays. The 120 second duplicate window of the events stream makes a resend sooner than that a no-op at the broker, which is why the default is 600.

### The consumer mirrors the extractor-report consumer

`services/x/src/deletion_acks.rs` verifies the Edge-provisioned durable (name, filter `evt.knowledge.deletion.acknowledged.v1`, explicit acks, 30 s ack wait, not push based), never creates one, and loops under the same supervision. An envelope that does not decode, has another event type, another producer or an invalid payload is Termed: it cannot be attributed and redelivery cannot fix it. A valid acknowledgement for another family, another tenant or an unknown id is acknowledged and ignored. A database failure is Nak'd with a delay. A duplicate acknowledgement changes nothing because the update requires `acknowledged_at is null`.

### Gauges are refreshed by the relay pass

The relay already runs every second against the same database, so it refreshes `x_unacknowledged_tombstones` and `x_oldest_unacknowledged_tombstone_age_seconds` at the end of each pass from one aggregate query. They are exported through the existing Prometheus recorder on the operator listener. Readiness ignores them: a Knowledge outage must not make X unready.

### The delete guard is a trigger

The repository has no code that deletes outbox rows, so there is no existing path to patch with the predicate `not (event_type = 'social.source.removed.v1' and acknowledged_at is null)`. A future retention job or a privacy deletion would be exactly the kind of path that forgets the rule, so `schema.sql` carries a `before delete` trigger that refuses to delete an unacknowledged removal row. It is the same predicate, enforced where all paths meet.

## Risks

A removal Knowledge never acknowledges is re-sent forever with the delay doubling to the cap (6 hours by default); that is the R5-11 rule. The trigger makes a manual cleanup of such a row an explicit act: an operator must first set `acknowledged_at`.
