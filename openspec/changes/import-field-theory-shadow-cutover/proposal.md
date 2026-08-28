## Why

The retired monolith and Field Theory may contain X bookmark history that the official OAuth
connector cannot reconstruct, but those sources are unowned legacy observations rather than
authoritative snapshots. Ratatoskr needs a repeatable, credential-free import and shadow verdict
before the owner can approve a cutover without losing history or creating false removals.

## What Changes

- Add an operator-invoked importer for the retired monolith bookmark metadata shape and the pinned
  Field Theory JSONL v1 / read-only SQLite v6 shapes, with strict source preflight and no credential,
  cookie, token, or session access.
- Require an explicit target X account and current OAuth identity verification before real writes;
  retain source IDs, observation timestamps, safe row evidence, import-parser version, and
  `legacy-import` provenance.
- Import normalized post evidence when stable provider identity is available, report unresolved or
  conflicting identities without fabricating users, and keep Field Theory categories distinct from
  native X folders.
- Make imports restartable and idempotent, with deterministic counts for inserted, matched,
  conflicted, and unmapped records.
- Add shadow comparison between imported observations and completed official OAuth snapshots,
  producing a deterministic redacted diff without granting absence authority to legacy or partial
  state.
- Add an owner-gated cutover and rollback checklist. The tooling records readiness evidence but
  cannot activate cutover; actual routing/schedule changes require owner approval and a separate
  `ratatoskr-workspace` changeset.
- Add synthetic/redacted fixtures derived from pinned public Field Theory source and document the
  remaining private-data prerequisites for a real run.

## Capabilities

### New Capabilities

- `legacy-field-theory-transition`: Credential-free legacy import, truthful identity/provenance
  mapping, deterministic shadow comparison, and owner-gated cutover readiness.

### Modified Capabilities

- `x-archive-schema`: Persist legacy import runs, source-row evidence, account-scoped observations,
  parser-version provenance, and shadow verdicts in the current schema definition.

## Impact

- Affected code: `crates/x-persistence`, `crates/x-sync`, operator-facing service/admin wiring,
  schema tests, synthetic fixtures, and `docs/cutover`.
- Existing behavior: official full snapshots remain the sole source of absence authority; no
  bookmark/OAuth contract changes and no new externally visible event are introduced.
- Source systems: retired-monolith extracts and Field Theory artifacts are opened read-only and are
  never deleted or retained as runtime dependencies.
- Security/privacy: source allow-lists reject credential-bearing shapes; diagnostics and reports use
  stable non-secret identifiers and digests, not post bodies, usernames, URLs, or source paths.
- Cross-repository boundary: implementation is local to X. Any real ingestion routing or schedule
  cutover remains blocked until an owner-approved workspace changeset supplies rollout and rollback.
- Dependency decision: reading SQLite requires an in-process, read-only adapter. The apply phase
  must prefer the already-used SQLx family if its SQLite feature fits the gate; any new production
  crate still requires an explicit maintenance, security, license, and compatibility review before
  addition.
