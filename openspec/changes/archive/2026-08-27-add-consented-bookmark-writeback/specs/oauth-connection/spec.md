## ADDED Requirements

### Requirement: Bookmark-write authorization uses a separate consent flow

Requesting bookmark-write authority SHALL issue a new PKCE authorization intent bound to an
existing connected account and its authenticated internal owner. The authorization request SHALL be
distinguishable from a default read intent and request the configured read scopes plus exactly
`bookmark.write`; it SHALL NOT request posting, liking, reposting, folder, direct-message, follow,
or any other write scope. The default read authorization flow and its scope set SHALL remain
unchanged.

#### Scenario: Write extension requests only bookmark mutation authority

- **WHEN** the owner of a read-connected account starts the bookmark-write authorization flow
- **THEN** the authorization URL requests the configured bookmark-write scope set including `bookmark.write` and contains no unrelated write scope

#### Scenario: Default connection remains read-only after write support exists

- **WHEN** a new account starts the normal connection flow
- **THEN** it receives the existing read authorization intent and no bookmark-write or other write scope is requested

### Requirement: Write callback remains bound to the existing account

The write-authorization callback SHALL pass the same state, expiry, replay, redirect, PKCE,
encrypted-storage, and diagnostic-secrecy protections as the read flow, SHALL require the
authenticated internal owner to match the intent, and SHALL verify that the provider identity belongs
to the existing account before activation. A complete matching grant SHALL atomically replace the
account's encrypted credential with the newly granted credential and activate a separate local
bookmark-write authorization record. A grant missing any configured read scope or `bookmark.write`
SHALL be recorded as downgraded and SHALL NOT replace the prior read credential or activate write
authority.

#### Scenario: Matching write grant activates separate authority

- **WHEN** the existing account owner completes the write PKCE callback and the provider returns the required scopes for the same provider identity
- **THEN** the newly encrypted credential and its exact combined scope set replace the prior credential atomically and the separate local bookmark-write authorization becomes active

#### Scenario: Foreign provider identity cannot be attached

- **WHEN** a write callback resolves to a provider identity different from the existing connected account
- **THEN** write activation is refused, the observed mismatch is audited without secrets, and the existing credential is not replaced

#### Scenario: Downgraded write grant leaves read access intact

- **WHEN** the provider grant omits `bookmark.write` or another required bookmark-mutation prerequisite
- **THEN** no active write authorization exists, the observed granted scopes remain auditable, and the read credential continues unchanged
