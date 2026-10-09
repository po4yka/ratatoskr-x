## Purpose

Defines how X outbox rows reach the broker and how the service reacts when a bus task stops: complete envelopes, a closed set of event types, an acknowledged relay, extractor-report consumption and honest readiness.

## ADDED Requirements

### Requirement: Outbox rows are complete envelopes keyed by their id

Every outbox row SHALL store the complete canonical envelope: `EventEnvelope` for operation reports and social source facts, and `CommandEnvelope` carrying `ContentCaptureRequested` for linked-article capture with producer `ratatoskr-x`, aggregate `operation:<capture id>` and an idempotency key equal to the SHA-256 of the capture id. The row id SHALL equal the envelope id and the tenant SHALL be `user:<owner>`. The schema SHALL accept only the five publishable event types.

#### Scenario: a bookmark source row is a complete event envelope

- **WHEN** a bookmark snapshot publishes a source
- **THEN** the stored payload parses as an event envelope whose id equals the row id, whose producer is `ratatoskr-x` and whose tenant is the account owner

#### Scenario: an unknown event type cannot be stored

- **WHEN** a row with an event type outside the five publishable types is inserted
- **THEN** the database refuses it

### Requirement: The relay publishes acknowledged rows in order and never starves

The relay SHALL publish `evt.<event type>` for events and `cmd.content.capture.requested.v1` for the command with `Nats-Msg-Id` equal to the row id, in creation order, and SHALL set `published_at` only after the broker acknowledgement. A failed publish SHALL record the attempt, a safe error class and a backoff and SHALL NOT block later rows. An event type with no subject SHALL stop the relay and make readiness false.

#### Scenario: a row is marked only after the acknowledgement

- **WHEN** the broker rejects a publish
- **THEN** the row stays unpublished with its attempt recorded and later rows are still published

### Requirement: Extractor reports complete linked-article captures

The service SHALL consume `platform.operation.reported.v1` through its fixed durable, complete the matching linked-article capture for an extractor report, acknowledge and ignore reports for unknown operations or other producers, and negatively acknowledge on a database error.

#### Scenario: an extractor report completes the capture

- **WHEN** the extractor reports success for an article capture the service relayed
- **THEN** the capture is completed and the delivery is acknowledged

### Requirement: A stopped bus task is not a ready service

When the consumer, relay, worker or report-consumer task returns before an orderly shutdown, the service SHALL report readiness false and exit with a failure code.

#### Scenario: readiness goes false when a bus task stops

- **WHEN** a bus task stops while the service is running
- **THEN** `/health/ready` answers 503 and the process returns an error
