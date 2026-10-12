## ADDED Requirements

### Requirement: Outbox rows carry a removal acknowledgement

`x_archive.outbox_events` SHALL carry `acknowledged_at timestamptz`, `ack_outcome text` constrained to `applied` or `refused`, `resend_count integer not null default 0` and `next_resend_at timestamptz`.

#### Scenario: the outcome is closed

- **WHEN** a row is given an `ack_outcome` other than `applied` or `refused`
- **THEN** the database refuses it

### Requirement: An unacknowledged removal cannot be deleted

A `social.source.removed.v1` outbox row with `acknowledged_at` null SHALL NOT be deletable. Any other row SHALL be deletable.

#### Scenario: deleting an unacknowledged removal fails

- **WHEN** a statement deletes a removal row that has no `acknowledged_at`
- **THEN** the database refuses it and the row remains

#### Scenario: an acknowledged removal may be deleted

- **WHEN** the removal row has `acknowledged_at` set
- **THEN** the delete succeeds
