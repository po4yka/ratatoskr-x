# bookmark-incremental-scan

## Purpose

Defines frequent bookmark observation scans that improve freshness while preserving complete snapshots as the sole authority for absence and repair evidence.

## Requirements

### Requirement: Incremental scans advance only a durable observation watermark

The service SHALL scan recently returned bookmarks only until it reaches the account's committed observation watermark, SHALL upsert bookmarks it observes without inferring removal, and SHALL advance that watermark only after the bounded scan completes successfully.

#### Scenario: A completed incremental scan advances its watermark

- **WHEN** a bounded incremental scan observes newer bookmarks and reaches its prior watermark without error
- **THEN** it records its newest completed observation as the account watermark and leaves every unobserved active bookmark unchanged

#### Scenario: A failed incremental scan retains its prior watermark

- **WHEN** an incremental scan fails, is cancelled, or is refused before completion
- **THEN** its prior watermark remains committed and it records no inferred bookmark removal

### Requirement: A detected recent-window gap requires a complete snapshot

The service SHALL mark an account as requiring a complete snapshot when its bounded recent scan cannot prove it reached the prior watermark, and SHALL refuse later incremental scans until a complete snapshot clears that requirement.

#### Scenario: A scan that exhausts its page bound escalates to a complete snapshot

- **WHEN** an incremental scan reaches its configured page limit before observing its committed watermark
- **THEN** it records a gap outcome, advances no watermark, and requires a complete snapshot before the next incremental scan

### Requirement: Incremental scans use a tighter request budget

The service SHALL reserve request cost before every incremental provider call, SHALL enforce an incremental per-run request cap lower than the corresponding complete-snapshot cap, and SHALL make no provider call after either budget refuses work or that cap is reached.

#### Scenario: Budget refusal blocks provider contact

- **WHEN** the next incremental page cannot reserve its required budget
- **THEN** the scan ends without contacting the provider for that page, advances no watermark, and records a non-success outcome

### Requirement: Scheduler commands select safe scan work

The service SHALL consume the platform scheduler command type `x.bookmarks.scan_requested.v1` for an account-targeted incremental scan and execute it as an observation-only scan; a command for an account requiring a complete snapshot SHALL select the complete-snapshot operation instead.

#### Scenario: Scheduler command escalates a drifted account

- **WHEN** the scheduler submits a valid incremental command for an account marked as requiring a complete snapshot
- **THEN** it schedules the complete-snapshot operation and does not run another partial scan
