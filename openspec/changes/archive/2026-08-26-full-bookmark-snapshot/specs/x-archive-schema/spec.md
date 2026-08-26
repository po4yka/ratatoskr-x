## MODIFIED Requirements

### Requirement: Owned schema inventory

Applying the schema to an empty database SHALL create exactly the service's owned tables inside the `x_archive` schema: accounts, credentials, users, posts, post relations, media, bookmarks, bookmark folders, bookmark folder items, sync runs, snapshots, snapshot bookmark items, bookmark snapshot authority, rate limit state, oauth intents, api budget windows, tombstones, outbox events, and inbox events. No table owned by another bounded context SHALL be created. Account rows SHALL carry a closed connection-state vocabulary (`connected`, `refresh_required`, `reauth_required`, `revoked`, `suspended`, `paused`), and credential rows SHALL carry the hash of the refresh token retired by the most recent rotation. Post rows SHALL carry conversation linkage by provider id, nullable public-metric counts, and a parser-version stamp; users, post relations, and media rows SHALL each carry a parser-version stamp recording which parser produced them. Bookmark rows SHALL retain only truthful observation timestamps and, when inferred absent by a complete snapshot, a reference to that snapshot; snapshot/run rows SHALL retain opaque resume state and non-negative page, item, addition, retention, and removal statistics.

#### Scenario: Fresh database receives the full owned inventory

- **WHEN** the schema is applied to a newly created empty database
- **THEN** querying the catalog lists every owned table under `x_archive` and no table outside it created by the service

#### Scenario: Connection state is constrained to the documented vocabulary

- **WHEN** the applied schema's account state constraint is inspected
- **THEN** only the six documented connection states are accepted values

#### Scenario: Posts carry conversation, count, and parser columns

- **WHEN** the applied posts table's columns are inspected
- **THEN** a nullable conversation-provider-id text column, nullable bigint count columns for the documented public metrics, and a non-nullable integer parser-version column all exist

#### Scenario: Normalization targets stamp their parser version

- **WHEN** the applied users, post relations, and media tables' columns are inspected
- **THEN** each carries a non-nullable integer parser-version column

#### Scenario: Snapshot authority has durable staging and evidence columns

- **WHEN** the applied bookmark, run, snapshot, staging, and authority tables are inspected
- **THEN** the schema can retain an opaque checkpoint, snapshot membership keyed to normalized posts, a single current snapshot per account, truthful removal evidence, and non-negative reconciliation statistics
