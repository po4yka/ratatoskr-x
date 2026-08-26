## MODIFIED Requirements

### Requirement: Complete snapshot authority swaps atomically

The service SHALL keep the previously authoritative bookmark set visible until every page of a new full snapshot has been received, validated, and staged. It SHALL then atomically mark the snapshot complete, make it the account's authoritative snapshot, reconcile the current bookmark set, record terminal statistics, clear any requirement for a complete snapshot, and record each discovered incremental-drift repair once. No observer using the archive's current-authority projection SHALL be able to observe a mixed old/new set or an authority pointer to an incomplete snapshot.

#### Scenario: Observers see either the old authority or the completed new authority

- **WHEN** a completed snapshot replaces an existing account authority
- **THEN** a transaction before the completion commit observes the old snapshot and its current bookmark set, while a transaction after the commit observes the new snapshot and its fully reconciled set, with no intermediate authority state

#### Scenario: Reconciliation records an incremental-drift repair once

- **WHEN** a complete snapshot corrects bookmark state that differs from the preceding incremental observation state
- **THEN** it records one repair linked to that complete snapshot, and repeating reconciliation for the same completed snapshot records no duplicate repair
