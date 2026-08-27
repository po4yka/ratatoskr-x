## Purpose

Defines durable, account-scoped X SocialSource events that downstream consumers can replay without
consulting X-owned storage or mistaking bookmark observations for provider timestamps.

## ADDED Requirements

### Requirement: Source events conform to one pinned shared contract
The service SHALL build captured and updated event envelopes with the exact pinned
`ratatoskr-social-contracts` revision and SHALL validate the corresponding committed contract
fixtures as part of its compatibility gate. It SHALL publish a full SocialSource snapshot rather
than a local projection or an event delta.

#### Scenario: captured source matches the shared fixture shape
- **WHEN** an account first preserves a normalized X post through a supported capture path
- **THEN** its durable event is `social.source.captured.v1` and its payload validates as the pinned
  SocialSourceCaptured contract fixture

#### Scenario: later material revision matches the shared fixture shape
- **WHEN** a preserved source's normalized snapshot materially changes
- **THEN** its durable event is `social.source.updated.v1` with the full current snapshot

### Requirement: Source identity and provenance remain account-scoped and truthful
The service SHALL assign a stable Ratatoskr SocialSource identity to each account's preserved post,
keep it distinct from the X provider post identity, and preserve the capture path's acquisition and
saved-state authority. Bookmark synchronization SHALL report official API acquisition with
authoritative platform state; an accepted X explicit-capture command SHALL report the command's
validated explicit acquisition and authority. A post publication time SHALL NOT be used as capture
time.

#### Scenario: a bookmark snapshot creates an authoritative source
- **WHEN** a bookmarked post first appears in a successful supported X API snapshot
- **THEN** the emitted source carries the account owner, a stable library identity, official API
  acquisition, authoritative-platform saved authority, and an observed capture instant

#### Scenario: an explicit X capture preserves its command provenance
- **WHEN** the service accepts a valid `social.capture.requested.v1` command for provider X
- **THEN** the emitted source carries the command's explicit acquisition and saved authority and
  does not claim provider bookmark membership

### Requirement: Publication is atomic and replay-safe
The source transition, its durable outbox event, and the source's current snapshot revision SHALL
commit together. Retrying an unchanged observation or redelivering the same explicit command SHALL
not emit another captured event; a material change SHALL emit one updated event.

#### Scenario: a repeated unchanged observation is silent
- **WHEN** the same account observes an unchanged preserved post again
- **THEN** the archive retains one source identity and has no additional captured or updated event

#### Scenario: an outbox failure rolls back the source transition
- **WHEN** persistence cannot write the source event's outbox record
- **THEN** the archive commits neither the corresponding source transition nor the event
