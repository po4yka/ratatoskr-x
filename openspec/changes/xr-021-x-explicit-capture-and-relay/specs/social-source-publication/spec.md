## MODIFIED Requirements

### Requirement: Source identity and provenance remain account-scoped and truthful

The service SHALL assign a stable Ratatoskr SocialSource identity to each account's preserved post,
keep it distinct from the X provider post identity, and preserve the capture path's acquisition and
saved-state authority. Bookmark synchronization SHALL report official API acquisition with
authoritative platform state; an explicit X capture SHALL report the command's validated explicit acquisition and authority
and SHALL publish only what the public resolution returned for the capturing owner, never a row
synced for another account. A post publication time SHALL NOT be used as capture
time.

#### Scenario: a bookmark snapshot creates an authoritative source

- **WHEN** a bookmarked post first appears in a successful supported X API snapshot
- **THEN** the emitted source carries the account owner, a stable library identity, official API
  acquisition, authoritative-platform saved authority, and an observed capture instant

#### Scenario: an explicit X capture preserves its command provenance

- **WHEN** the service resolves a valid `social.capture.requested.v1` command for provider X
- **THEN** the emitted source carries the command's explicit acquisition and saved authority and
  does not claim provider bookmark membership
