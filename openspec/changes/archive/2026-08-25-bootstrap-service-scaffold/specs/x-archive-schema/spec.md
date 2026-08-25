# x-archive-schema delta

## Purpose

Defines the first-version `x_archive` PostgreSQL schema that this service owns: what database objects exist after schema application, that application is idempotent and performed without migrations, how disposable test databases are produced from the same definition, and that the schema keeps this bounded context's data isolated.

## ADDED Requirements

### Requirement: Owned schema inventory

Applying the schema to an empty database SHALL create exactly the service's owned tables inside the `x_archive` schema: accounts, credentials, users, posts, post relations, media, bookmarks, bookmark folders, bookmark folder items, sync runs, snapshots, rate limit state, tombstones, outbox events, and inbox events. No table owned by another bounded context SHALL be created.

#### Scenario: Fresh database receives the full owned inventory

- **WHEN** the schema is applied to a newly created empty database
- **THEN** querying the catalog lists every owned table under `x_archive` and no table outside it created by the service

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
