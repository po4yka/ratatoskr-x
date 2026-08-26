## MODIFIED Requirements

### Requirement: The archive schema keeps account ownership and trustworthy bookmark observations

The owned `x_archive` schema SHALL keep provider identifiers namespaced by provider and account-scoped bookmark observations separate from normalized posts. It SHALL store first and last observation instants without representing either as an authoritative provider save time, retain observed removals rather than deleting records, and include in-place durable state for each account's incremental watermark, complete-snapshot requirement, partial-scan outcome, and idempotent complete-snapshot repair evidence.

#### Scenario: A bookmark observation never exposes the post publication time as saved time

- **WHEN** a normalized post has a publication timestamp and becomes an account bookmark observation
- **THEN** its bookmark record persists distinct first/last observation fields and has no column or API field that claims the post timestamp was a provider saved time

#### Scenario: A repair record is unique to its complete snapshot and bookmark

- **WHEN** reconciliation attempts to record the same bookmark repair more than once for one complete snapshot
- **THEN** the schema retains exactly one repair record for that snapshot and bookmark
