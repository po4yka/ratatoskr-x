## Purpose

Defines how provider API request cost is accounted per account in durable fixed windows and how the budget gate blocks work that would exceed the configured cap before any provider call is made.

## ADDED Requirements

### Requirement: Durable per-account window accounting

The service SHALL account provider API request cost per account inside fixed windows of configured length, SHALL persist the running usage of the current window in the owned schema, and SHALL make every accepted reservation visible to any later observer of the same window, including a freshly constructed gate after process restart.

#### Scenario: Reservation within cap is counted durably

- **WHEN** a reservation for request cost is accepted inside an open window and a newly constructed gate instance observes the same account afterwards
- **THEN** the accepted cost is included in the persisted usage of that window

#### Scenario: Usage survives a restart

- **WHEN** a gate instance accepts cost, is discarded, and a new instance is constructed over the same database
- **THEN** the new instance refuses further cost that would exceed what the previous instance had already charged against the cap

### Requirement: Hard cap enforced before provider contact

The budget gate SHALL decide each reservation by comparing accumulated plus requested cost against the configured cap, SHALL refuse any reservation exceeding the remaining allowance with an outcome naming when the window resets, and SHALL charge nothing for the refused reservation. Acceptance happens before any provider call is attempted; the gate has no knowledge of provider responses.

#### Scenario: Over-budget reservation is blocked before any call

- **WHEN** accumulated usage plus the requested cost exceeds the cap and the caller attempts the reservation
- **THEN** the gate refuses with the exhausted outcome carrying the reset time, and the persisted usage is unchanged by the attempt

#### Scenario: Refused reservation performs no provider work

- **WHEN** the gate refuses a reservation and the caller honors the refusal
- **THEN** no provider request attributable to that reservation occurs, because refusal precedes any call decision made by the caller

### Requirement: Window rollover on expiry

When the current window for an account has expired or none exists, the next reservation SHALL open a fresh window starting at that moment with zeroed usage, and prior windows SHALL remain as historical rows rather than being mutated.

#### Scenario: Expired window yields a fresh allowance

- **WHEN** a reservation arrives after the previous window's expiry has passed
- **THEN** a new window begins at the reservation time with zero prior usage and the reservation is evaluated against the full cap

#### Scenario: Superseded windows stay immutable

- **WHEN** a fresh window has been opened for an account
- **THEN** the usage figures of all earlier windows of that account read exactly as they were last written

### Requirement: Serialized concurrency

Concurrent reservations for the same account SHALL be serialized so that the sum of accepted costs never exceeds the cap, even when reservations race; reservations for different accounts remain independent.

#### Scenario: Racing reservations never exceed the cap

- **WHEN** more concurrent reservation attempts than the remaining allowance arrive for one account simultaneously
- **THEN** exactly the remaining allowance is accepted in total, the rest are refused, and the persisted usage equals the accepted sum
