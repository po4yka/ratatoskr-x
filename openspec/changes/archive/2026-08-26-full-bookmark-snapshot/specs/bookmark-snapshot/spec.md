## Purpose

Defines how Ratatoskr derives authoritative per-account bookmark state from a complete, successful traversal of the official X API without treating partial results as absence evidence.

## ADDED Requirements

### Requirement: Full snapshot enumeration is budget-bounded and resumable

The service SHALL enumerate a full bookmark snapshot through a supported-provider pagination boundary, reserving configured request cost before every page request. It SHALL persist each successfully normalized page, its membership staging records, and the opaque continuation token in one durable batch before requesting another page. A failed, refused, malformed, cancelled, or otherwise incomplete run SHALL retain its committed checkpoint and SHALL NOT receive absence authority; a later invocation for the same unfinished run SHALL continue from that exact opaque token rather than restart or parse it.

#### Scenario: A mid-run provider failure resumes at the committed continuation token

- **WHEN** a multi-page full snapshot commits its first page and the provider then fails before returning the next page
- **THEN** the run remains incomplete with the first page's opaque continuation token and a resumed invocation requests that token first, without duplicating its staged membership or treating any unobserved bookmark as removed

#### Scenario: Budget exhaustion leaves the snapshot non-authoritative before provider contact

- **WHEN** the next page cannot reserve sufficient remaining provider budget
- **THEN** the service makes no request for that page, retains the prior checkpoint and staged records, records the non-completed terminal outcome, and changes no active bookmark into a removal observation

### Requirement: Complete snapshot authority swaps atomically

The service SHALL keep the previously authoritative bookmark set visible until every page of a new full snapshot has been received, validated, and staged. It SHALL then atomically mark the snapshot complete, make it the account's authoritative snapshot, reconcile the current bookmark set, and record terminal statistics. No observer using the archive's current-authority projection SHALL be able to observe a mixed old/new set or an authority pointer to an incomplete snapshot.

#### Scenario: Observers see either the old authority or the completed new authority

- **WHEN** a completed snapshot replaces an existing account authority
- **THEN** a transaction before the completion commit observes the old snapshot and its current bookmark set, while a transaction after the commit observes the new snapshot and its fully reconciled set, with no intermediate authority state

### Requirement: Bookmark observations remain truthful and linked to normalized posts

For every staged bookmark page, the service SHALL persist its normalized post graph and stage membership by the resulting post identity. On an authoritative completion, each active bookmark SHALL refer to a normalized post, preserve its original `first_observed_saved_at`, and refresh `last_observed_saved_at` from the provider-observation time. The service SHALL NOT derive a bookmark save time from the post publication time.

#### Scenario: A seen bookmark keeps its first observation but refreshes last observation

- **WHEN** an already active bookmark appears in a later complete snapshot
- **THEN** it remains linked to the same normalized post, retains its original first-observed timestamp, and has its last-observed timestamp updated to the later observation time

### Requirement: Authoritative absence becomes an observed removal, never a delete

Only a complete successful snapshot SHALL infer that an active bookmark absent from its staged membership was removed. The service SHALL retain that bookmark row, set `observed_removed_at` to the completion observation time, and attach the complete snapshot/run evidence. An incomplete snapshot SHALL neither set an observation-removal timestamp nor delete bookmark state.

#### Scenario: Complete reconciliation records an unbookmark observation

- **WHEN** a bookmark active under the previous authority is absent from a newly completed full snapshot
- **THEN** its bookmark row remains present with a non-null observed-removal timestamp and reference to the completing snapshot, and the completion statistics count exactly one removal

#### Scenario: Reconciliation counts additions, retained bookmarks, and removals

- **WHEN** a complete snapshot contains both a newly seen bookmark and one retained bookmark while omitting one previously active bookmark
- **THEN** the persisted run statistics report one addition, one retained update, and one removal, and the current authoritative projection contains exactly the new and retained bookmarks
