## MODIFIED Requirements

### Requirement: Owned schema inventory

Applying the schema to an empty database SHALL create exactly the service's owned tables inside the `x_archive` schema: accounts, credentials, users, posts, post relations, media, bookmarks, bookmark folders, bookmark folder items, sync runs, snapshots, rate limit state, oauth intents, api budget windows, tombstones, outbox events, and inbox events. No table owned by another bounded context SHALL be created. Account rows SHALL carry a closed connection-state vocabulary (`connected`, `refresh_required`, `reauth_required`, `revoked`, `suspended`, `paused`), and credential rows SHALL carry the hash of the refresh token retired by the most recent rotation.

#### Scenario: Fresh database receives the full owned inventory

- **WHEN** the schema is applied to a newly created empty database
- **THEN** querying the catalog lists every owned table under `x_archive` and no table outside it created by the service

#### Scenario: Connection state is constrained to the documented vocabulary

- **WHEN** the applied schema's account state constraint is inspected
- **THEN** only the six documented connection states are accepted values
