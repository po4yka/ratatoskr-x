## Why

The `ratatoskr_events` stream drops its oldest message after 7 days or at 1 GiB. Today the X relay marks a `social.source.removed.v1` row published at the broker acknowledgement and never looks at it again, so a removal that Knowledge does not consume in time is lost and the deleted post stays in Knowledge. Changeset XR-021 round 5 (CONTRACTS.md R5-11, rule 4) makes Knowledge acknowledge every applied or refused removal with `knowledge.deletion.acknowledged.v1` and makes the producer re-send the fact until that acknowledgement arrives.

## What Changes

- Every `ratatoskr-*` git dependency moves to the round 5 contracts revision `d98967b7b7ee80442c93ef4657140068955c8f61`, which registers `knowledge.deletion.acknowledged.v1`.
- `x_archive.outbox_events` gains `acknowledged_at`, `ack_outcome`, `resend_count` and `next_resend_at` (`schema.sql` in place). Only `social.source.removed.v1` rows use them.
- The relay arms `next_resend_at = published_at + resend_after` when it marks a removal published, republishes the stored bytes of a due unacknowledged removal with the same row id as `Nats-Msg-Id`, and doubles the resend delay up to a cap. The resend never stops by count and never touches `published_at` or relay health.
- A new supervised consumer of the Edge-provisioned durable `ratatoskr_x_deletion_acks` sets `acknowledged_at` and `ack_outcome` on the matching row. A `refused` outcome stops the resend, logs at ERROR without content and increments `x_tombstone_refused_total`. Two gauges, `x_unacknowledged_tombstones` and `x_oldest_unacknowledged_tombstone_age_seconds`, are served on the operator listener. Readiness is not tied to them.
- New keys `RATATOSKR__TOMBSTONE__RESEND_AFTER_SECONDS` (default 600, 30..=86400) and `RATATOSKR__TOMBSTONE__RESEND_CAP_SECONDS` (default 21600, at least `RESEND_AFTER`, at most 604800).
- `deploy/nats/identity.conf` gains the three permissions for the new durable, byte-equal to Platform's stanza after the usual normalization.
- No path deletes an unacknowledged removal row. The repository has no such path today, so the rule is enforced where every present and future path meets it: a `before delete` trigger on the outbox.

## Capabilities

### New Capabilities

- `removal-acknowledgement`: a removal is kept and re-sent until Knowledge acknowledges it.

### Modified Capabilities

- `x-archive-schema`: the four acknowledgement columns and the delete guard.

## Impact

- Affected code: `x-persistence` outbox transitions and schema, `x-capture` relay, `x-core` configuration, `services/x` consumer and wiring, the NATS stanza, the operator example, README and DEVELOPMENT.
- Breaking: `OutboxRelay::new` takes a `ResendPolicy`; the old two-argument constructor is gone. `schema.sql` changes in place (no migration, per the development status).
- Out of scope: the dormant bookmark and compliance lane, the 401 wording of a queued operation, a repeat capture that publishes no fact, GitHub, deletion acknowledgement for any fact but `social.source.removed.v1`, and erasing social facts from the events stream (CONTRACTS.md R5-15).

This change cites XR-021 CONTRACTS.md R5-11.
