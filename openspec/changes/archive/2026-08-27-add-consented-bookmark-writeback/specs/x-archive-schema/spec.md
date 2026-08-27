## MODIFIED Requirements

### Requirement: Owned schema inventory

Applying the schema to an empty database SHALL create exactly the service's owned tables inside the
`x_archive` schema: accounts, credentials, users, posts, post relations, media, bookmarks, bookmark
folders, bookmark folder items, sync runs, snapshots, snapshot bookmark items, bookmark snapshot
authority, bookmark incremental state, bookmark reconciliation repairs, rate limit state, oauth
intents, api budget windows, bookmark write authorizations, bookmark write consents, bookmark write
operations, bookmark write audit events, tombstones, social sources, social source revisions,
Knowledge analysis links, compliance revalidation ledger, article captures, post article links,
explicit captures, outbox events, and inbox events. No table owned by another bounded context SHALL
be created. Account rows SHALL carry a closed connection-state vocabulary (`connected`,
`refresh_required`, `reauth_required`, `revoked`, `suspended`, `paused`) and the internal owner
identity required by account-scoped records; credential rows SHALL carry the hash of the refresh
token retired by the most recent rotation, while OAuth intents SHALL distinguish default-read from
bookmark-write authorization purpose and bind write intents to an existing account.
Post rows SHALL carry conversation linkage by provider id, nullable public-metric counts, a
parser-version stamp, and a normalized array of provider-expanded URLs; users, post relations, and
media rows SHALL each carry a parser-version stamp recording which parser produced them.
Social-source records SHALL keep an account-scoped stable identity, revision digest, and explicit
removal state. Knowledge analysis links SHALL reference an exact retained source revision without
any cross-schema foreign key. Compliance ledger entries and tombstones SHALL preserve account,
source, provider-state, and request evidence without retaining credentials or provider content.
Article captures SHALL preserve the normalized URL, operation and correlation identifiers,
terminal state, and optional Document identity and Document IR BlobRef. Post article links SHALL
permit several posts to reference one account article capture. Explicit captures SHALL retain the
accepted command and operation identity, original permalink, captured instant, and explicit browser
provenance without claiming a native X bookmark. Bookmark-write consent and operation rows SHALL
retain exact owner/action/target/surface/idempotency bindings, while append-only audit rows retain
ordered decisions and bounded provider evidence without token or post-body columns. API budget
windows SHALL key usage by account, closed operation class, and window start.

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

#### Scenario: Analysis and compliance evidence is source-scoped

- **WHEN** the applied schema is inspected for Knowledge completion and compliance evidence
- **THEN** completion linkage references one retained source digest while ledger and tombstone rows remain scoped to the owning account and social source

#### Scenario: Bookmark-write state is scoped and secret-free

- **WHEN** the applied write-authorization, consent, operation, and audit tables are inspected
- **THEN** their ownership, one-action consent, idempotency uniqueness, append-only evidence, and credential exclusions are enforceable inside `x_archive`

#### Scenario: Budget-window identity includes operation class

- **WHEN** read and bookmark-write windows start for one account at the same instant
- **THEN** the schema retains both rows independently under their closed budget classes

## ADDED Requirements

### Requirement: Bookmark observations retain write-operation authority separately

Bookmark rows SHALL retain provider-confirmed add or remove operation evidence separately from
complete-snapshot removal evidence. A write result SHALL use honest observation instants and SHALL
NOT be represented as an authoritative native save or removal timestamp. A later complete snapshot
SHALL remain able to supersede the current projection without erasing the write audit history.

#### Scenario: Confirmed write does not masquerade as snapshot evidence

- **WHEN** a successful bookmark add or remove updates the account projection
- **THEN** the bookmark identifies the confirming write operation and observation instant while its complete-snapshot evidence fields retain their distinct meaning

#### Scenario: Later snapshot preserves mutation history

- **WHEN** a complete bookmark snapshot later observes state different from a prior confirmed write result
- **THEN** snapshot reconciliation updates current bookmark projection authority without deleting or rewriting the earlier write operation and audit rows
