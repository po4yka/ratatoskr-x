## ADDED Requirements

### Requirement: Relay health reaches readiness

The relay SHALL expose a health flag that is unhealthy when a pass attempted a publish, at least one publish failed, and the oldest unpublished publishable row has `attempt_count >= 3` or is older than 300 seconds. The flag SHALL be healthy again after a pass that fails no publish or finds no due row. A row classed undeliverable SHALL NOT count. The service SHALL report `/health/ready` as 503 while the flag is unhealthy and SHALL NOT stop the relay or exit the process.

#### Scenario: repeated refusals mark the relay unhealthy until a clean pass

- **WHEN** the publisher refuses the head row on three passes and then accepts it
- **THEN** the health flag is unhealthy after the third refusal and healthy after the clean pass

#### Scenario: readiness follows the relay

- **WHEN** the broker's X stanza omits one published subject and three attempts fail, and the stanza is later restored
- **THEN** `/health/ready` answers 503 after the third attempt and 200 after the row is published

### Requirement: An oversize body is undeliverable

The publisher SHALL refuse a body larger than the connected server's `max_payload` minus 1 KiB with class `payload_too_large` without publishing. The relay SHALL mark such a row undeliverable at once, increment its attempt count, and continue with the next row in the same pass.

#### Scenario: an oversize body does not block later rows

- **WHEN** the publisher reports `payload_too_large` for the head row
- **THEN** that row has `undeliverable_at` set, is never selected again, and the next row is published in the same pass

## MODIFIED Requirements

### Requirement: Operator example is shipped and loadable

The repository SHALL ship `deploy/systemd/x.conf.example` that the X configuration loader accepts and that sets the operator address, the bus URL and NKey seed path, and the public-capture credential path, API base URL, attempts, batch size and poll interval.

#### Scenario: the shipped example loads

- **WHEN** the example is loaded with temporary files for its absolute secret paths
- **THEN** the configuration carries `127.0.0.1:9087`, the bus URL, the seed path, `https://api.x.com`, 5 attempts, batch 8 and a 2 second poll
