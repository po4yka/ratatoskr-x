## Purpose

Defines how X keeps a `social.source.removed.v1` fact and re-sends it until Knowledge acknowledges it, so that the drop-oldest events stream cannot lose a deletion.

## ADDED Requirements

### Requirement: A published removal is armed for resending

When the relay marks a `social.source.removed.v1` row published, it SHALL set `next_resend_at` to the publish time plus the configured resend delay. A row of any other event type SHALL get no `next_resend_at` and SHALL never be resent.

#### Scenario: a removal is armed

- **WHEN** the relay marks a removal published
- **THEN** its `next_resend_at` equals `published_at` plus the resend delay

#### Scenario: a captured fact is not armed

- **WHEN** the relay marks a captured, updated or report row published
- **THEN** its `next_resend_at` stays null and no pass selects it for resending

### Requirement: An unacknowledged removal is re-published unchanged

A pass SHALL select every removal with `published_at` set, `acknowledged_at` null, `next_resend_at` and `next_attempt_at` due, and SHALL publish its stored bytes again with the row id as `Nats-Msg-Id`. After the broker acknowledged the republish the row SHALL have `resend_count` increased by one and `next_resend_at` set to now plus the least of the resend delay times two to the previous `resend_count` and the cap. A failed republish SHALL use the existing failure backoff and SHALL NOT change `published_at`, `resend_count` or `next_resend_at`. An unacknowledged removal SHALL NOT affect relay health.

#### Scenario: a due removal is republished with the same message id

- **WHEN** a removal is published, no acknowledgement arrives and the resend delay passes
- **THEN** the relay publishes the same bytes with the same `Nats-Msg-Id` and the row's `resend_count` becomes 1

#### Scenario: the resend delay doubles up to the cap

- **WHEN** a removal stays unacknowledged across several resends
- **THEN** each next delay is twice the previous one until it reaches the cap, and the resend never stops by count

#### Scenario: a failed republish leaves the schedule alone

- **WHEN** the broker refuses a republish
- **THEN** the row's `published_at`, `resend_count` and `next_resend_at` are unchanged, its `next_attempt_at` is backed off, and the relay stays healthy

### Requirement: An acknowledgement ends the resend

The service SHALL consume `knowledge.deletion.acknowledged.v1` through the Edge-provisioned durable `ratatoskr_x_deletion_acks`. For a valid acknowledgement whose `acknowledged_event_id` is the id of a removal row of the same tenant, it SHALL set `acknowledged_at` and `ack_outcome` once and the row SHALL no longer be resent. An acknowledgement that names no row of this service, a row of another tenant, or an already acknowledged row SHALL be acknowledged to the broker and change nothing. A message that is undecodable, has another event type or producer, or carries an invalid payload SHALL be terminated.

#### Scenario: an applied acknowledgement stops the resend

- **WHEN** Knowledge publishes an `applied` acknowledgement for a published removal
- **THEN** the row has `acknowledged_at` and `ack_outcome = 'applied'` and no later pass republishes it

#### Scenario: a refused acknowledgement stops the resend and is surfaced

- **WHEN** Knowledge publishes a `refused` acknowledgement
- **THEN** the resend stops, an ERROR log line without content is written and `x_tombstone_refused_total` increases by one

#### Scenario: an acknowledgement for no row is ignored

- **WHEN** an acknowledgement names an unknown id or another tenant's row
- **THEN** no row changes and the message is acknowledged to the broker

#### Scenario: a mismatched durable refuses startup

- **WHEN** the durable has another filter or ack wait
- **THEN** the consumer refuses to start

### Requirement: Readiness and gauges

Readiness SHALL NOT depend on acknowledgements. The operator listener SHALL expose `x_unacknowledged_tombstones` and `x_oldest_unacknowledged_tombstone_age_seconds`.

#### Scenario: Knowledge never answers

- **WHEN** a removal stays unacknowledged for many resend periods
- **THEN** `/health/ready` stays ready and the two gauges show one unacknowledged tombstone and its growing age

### Requirement: Resend timing is configured strictly

`RATATOSKR__TOMBSTONE__RESEND_AFTER_SECONDS` SHALL default to 600 and accept 30 to 86400. `RATATOSKR__TOMBSTONE__RESEND_CAP_SECONDS` SHALL default to 21600, be at least the resend delay and at most 604800.

#### Scenario: out-of-range values are refused

- **WHEN** the delay is 10, or the cap is below the delay or above 604800
- **THEN** configuration loading fails with every violation reported
