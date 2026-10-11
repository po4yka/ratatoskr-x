## MODIFIED Requirements

### Requirement: Outbox rows may be marked undeliverable

`x_archive.outbox_events` SHALL carry a nullable `undeliverable_at timestamptz`. A row with it set SHALL NOT be selected for publication.

#### Scenario: an undeliverable row is not selected

- **WHEN** a row has `undeliverable_at` set
- **THEN** a relay pass does not publish it and does not count it toward relay health
