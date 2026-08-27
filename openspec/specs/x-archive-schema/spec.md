# x-archive-schema

## Purpose

Defines the first-version `x_archive` PostgreSQL schema that this service owns: what database objects exist after schema application, that application is idempotent and performed without migrations, how disposable test databases are produced from the same definition, and that the schema keeps this bounded context's data isolated.

## Requirements

### Requirement: Owned schema inventory

Applying the schema to an empty database SHALL create exactly the service's owned tables inside the `x_archive` schema: accounts, credentials, users, posts, post relations, media, bookmarks, bookmark folders, bookmark folder items, sync runs, snapshots, snapshot bookmark items, bookmark snapshot authority, bookmark incremental state, bookmark reconciliation repairs, rate limit state, oauth intents, api budget windows, tombstones, social sources, social source revisions, article captures, post article links, outbox events, and inbox events. No table owned by another bounded context SHALL be created. Account rows SHALL carry a closed connection-state vocabulary (`connected`, `refresh_required`, `reauth_required`, `revoked`, `suspended`, `paused`) and the internal owner identity required by account-scoped records; credential rows SHALL carry the hash of the refresh token retired by the most recent rotation. Post rows SHALL carry conversation linkage by provider id, nullable public-metric counts, a parser-version stamp, and a normalized array of provider-expanded URLs; users, post relations, and media rows SHALL each carry a parser-version stamp recording which parser produced them. Social-source records SHALL keep an account-scoped stable identity and revision digest. Article captures SHALL preserve the normalized URL, operation and correlation identifiers, terminal state, and optional Document identity and Document IR BlobRef. Post article links SHALL permit several posts to reference one account article capture.

#### Scenario: Fresh database receives the full owned inventory

- **WHEN** the schema is applied to a newly created empty database
- **THEN** querying the catalog lists every owned table under `x_archive` and no table outside it created by the service

#### Scenario: Connection state is constrained to the documented vocabulary

- **WHEN** the applied schema's account state constraint is inspected
- **THEN** only the six documented connection states are accepted values

#### Scenario: Posts carry conversation, count, and parser columns

- **WHEN** the applied posts table's columns are inspected
- **THEN** a nullable conversation-provider-id text column, nullable bigint count columns for the documented public metrics, and a non-nullable integer parser-version column all exist

#### Scenario: Posts retain provider-expanded URLs

- **WHEN** a normalized post is persisted before bookmark or explicit capture
- **THEN** its provider-expanded URL array remains available to both source-capture paths

#### Scenario: Normalization targets stamp their parser version

- **WHEN** the applied users, post relations, and media tables' columns are inspected
- **THEN** each carries a non-nullable integer parser-version column

#### Scenario: Snapshot authority has durable staging and evidence columns

- **WHEN** the applied bookmark, run, snapshot, staging, and authority tables' columns are inspected
- **THEN** the schema can retain an opaque checkpoint, snapshot membership keyed to normalized posts, a single current snapshot per account, truthful removal evidence, and non-negative reconciliation statistics

#### Scenario: Source and article-capture identities are durable and scoped

- **WHEN** the applied schema is inspected after two posts reference the same normalized external URL for one account
- **THEN** it permits one account-scoped article capture, links both posts to it, and retains an optional terminal Document IR BlobRef without a cross-schema foreign key

### Requirement: The archive schema keeps account ownership and trustworthy bookmark observations

The owned `x_archive` schema SHALL keep provider identifiers namespaced by provider and account-scoped bookmark observations separate from normalized posts. It SHALL store first and last observation instants without representing either as an authoritative provider save time, retain observed removals rather than deleting records, and include in-place durable state for each account's incremental watermark, complete-snapshot requirement, partial-scan outcome, and idempotent complete-snapshot repair evidence.

#### Scenario: A bookmark observation never exposes the post publication time as saved time

- **WHEN** a normalized post has a publication timestamp and becomes an account bookmark observation
- **THEN** its bookmark record persists distinct first/last observation fields and has no column or API field that claims the post timestamp was a provider saved time

#### Scenario: A repair record is unique to its complete snapshot and bookmark

- **WHEN** reconciliation attempts to record the same bookmark repair more than once for one complete snapshot
- **THEN** the schema retains exactly one repair record for that snapshot and bookmark

### Requirement: In-place idempotent application without migrations

Schema application SHALL be safe to repeat: applying the current definition to a database that already matches it SHALL succeed and leave the structure unchanged. The service SHALL NOT create or use migration machinery; the current `schema.sql` definition is the single source of the database shape, edited in place while development status permits.

#### Scenario: Reapplication changes nothing

- **WHEN** the schema is applied twice to the same database without edits between applications
- **THEN** the second application succeeds and the catalog-visible table set is identical after both applications

#### Scenario: No migration bookkeeping appears

- **WHEN** the schema has been applied to a fresh database
- **THEN** no migration-tracking table exists anywhere in the database

### Requirement: Disposable test databases from the schema

A test harness SHALL be able to create a uniquely named disposable database from the same `schema.sql` definition using an administrative connection URL taken from the environment, and SHALL remove that database on explicit cleanup so concurrent test runs stay isolated.

#### Scenario: Harness creates and removes an isolated database

- **WHEN** the harness creates a test database, applies the schema, and then requests cleanup
- **THEN** the database existed under a unique generated name while in use, served queries against the applied schema, and no longer exists after cleanup

### Requirement: Bounded-context isolation

The schema SHALL enforce provider identity uniqueness within the service's own tables and SHALL NOT contain foreign keys referencing schemas outside `x_archive`.

#### Scenario: Constraints stay inside the boundary

- **WHEN** the applied schema's constraints are inspected
- **THEN** every foreign key resolves between tables of the `x_archive` schema and provider identity columns carry uniqueness guarantees where the model declares them
