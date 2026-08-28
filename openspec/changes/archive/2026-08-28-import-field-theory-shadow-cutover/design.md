## Context

See `proposal.md` for motivation and `docs/research/field-theory-source-discovery.md` for source
evidence. Field Theory's raw JSONL and derived SQLite index contain post and bookmark observations
but no archive-owner identity. The retired monolith export has the same ownership gap. Existing X
tables make complete official snapshots authoritative; the transition must not put legacy absence
or Field Theory's historical ordering into that path.

The service has no operator command surface today. Its HTTP listener is process administration only,
so migration controls must not become remotely callable routes. Development status requires editing
the one current schema definition in place and forbids migration machinery or a second API version.

## Goals / Non-Goals

**Goals:**

- Provide an executable, bounded operator workflow for source preflight, import, shadow reporting,
  checklist generation, and approval recording.
- Preserve useful legacy post content and organization evidence while keeping incomplete identity
  explicit and later provider normalization able to supersede the projection.
- Make every report reproducible from durable, account-scoped input digests without exposing private
  bookmark content.
- Keep the first complete official snapshot as the only absence-authority boundary.

**Non-Goals:**

- Reading or transferring Field Theory cookies, OAuth token files, browser profiles, sessions, or
  authorization headers.
- Treating legacy categories as native X folders, linked article bodies as post content, or opaque
  ordering keys as timestamps.
- Publishing a new fleet event or changing Platform routing/schedules in this repository.
- Deleting the retired source or imported evidence during cutover or rollback.

## Decisions

### D1: Use a dedicated local operator binary, not HTTP administration routes

Add `ratatoskr-x-transition` under the existing service package with a closed command vocabulary:
`preflight`, `import`, `shadow-report`, `checklist`, and `record-approval`. Database and current OAuth
configuration come from the existing typed environment configuration. Source paths and approval
documents are explicit local arguments but are never persisted or logged; reports may be written to
an operator-selected file or stdout.

This keeps migration authority behind host access and avoids creating an unauthenticated network
control plane. Reusing the process admin router was rejected because it exposes only liveness,
readiness, metrics, and version and has no owner authorization boundary. A general CLI framework is
unnecessary for five exact commands; adding one would be a new production dependency without
material capability gain.

### D2: Support pinned, allow-listed data projections rather than arbitrary database discovery

Source adapters accept:

- retired-monolith `x_bookmark_metadata` CSV with its exact nine-column header;
- Field Theory `bookmarks.jsonl` cache schema v1;
- Field Theory `bookmarks.db` application schema v6.

JSONL is preferred when both Field Theory artifacts are supplied because upstream defines it as raw
cache authority and SQLite as a derived search index. SQLite is opened with SQLx SQLite options that
disable creation and request read-only immutable access; only `meta` and the 37 documented
`bookmarks` columns are queried. Enabling SQLx's pinned SQLite feature reuses the existing dependency
family and avoids shelling out to an unversioned `sqlite3` executable. The gate must prove Linux and
macOS build compatibility before this decision is accepted; failure returns to dependency review,
not a subprocess fallback.

Every adapter enforces bounded bytes, rows, text length, and JSON depth, computes a SHA-256 source
digest, validates the full selected shape, and materializes a bounded normalized batch before any
target transaction. Unknown versions, duplicate source keys with different evidence, or credential
and session field names fail preflight. The tool never scans neighboring files such as
`oauth-token.json`.

### D3: Separate legacy observation storage from current snapshot authority

Add four tables to the current `schema.sql`:

- `legacy_import_runs`: target account, source kind/version/digest, importer parser version,
  ownership-approval digest, terminal status, and deterministic counts;
- `legacy_import_items`: run/source identity, safe row digest, provider post identity, normalized
  post link, observation times, bounded legacy category/folder metadata, resolution class, and
  conflict evidence;
- `legacy_shadow_reports`: import run plus complete official snapshot, both input digests, canonical
  deterministic report payload/digest, counts, and review state;
- `legacy_transition_approvals`: report/checklist digest, target account/internal owner, decision
  time, and supersession state.

Imported membership lives in `legacy_import_items`, not snapshot staging or bookmark authority
tables. Thus a legacy-only item is preserved without appearing as an official current bookmark and
cannot be removed by a partial scan. A matched item may link to an existing current bookmark, but
that link conveys identity only.

Rows with a stable post provider ID create or update `posts` using an importer-owned parser-version
constant and `availability = unknown`. If a stable author provider ID is present, normal user/post
identity mapping is used. If only a mutable handle exists, the imported post keeps a nullable author
link and is not published as a normalized social source until official normalization resolves it.
An official provider upsert for the same post ID replaces the current projection while the immutable
legacy item retains provenance. Synthesizing a provider user from a handle or a shared "unknown"
provider ID was rejected because both fabricate identity.

Original text belongs in the normalized post projection when a valid post ID exists. Durable legacy
evidence stores only source IDs, safe organization metadata, timestamps, and digests; it does not
duplicate post bodies, handles, or URLs. Conflicting explicit/URL post IDs remain item evidence with
no post write.

### D4: Verify archive ownership against the current OAuth account before import

The approval document binds target account ID, internal owner ID, and source digest. A narrow
`CurrentAccountIdentity` seam resolves `/2/users/me` through the account's current encrypted OAuth
credential and compares the returned ID with `accounts.provider_user_id`. Tests use a hand-written
fake; production uses the official user-context endpoint. No source credential participates.

Preflight may run without approval or provider I/O, but `import` fails before PostgreSQL writes on a
missing/stale approval, disconnected current account, or identity mismatch. Automatically selecting
the only configured account was rejected because Field Theory contains no ownership evidence.

### D5: Commit one validated import atomically and key idempotence by evidence

After full source validation, one PostgreSQL transaction inserts the run, upserts eligible normalized
posts, and inserts all item outcomes. Uniqueness over account, source kind, source record key, source
digest, and importer parser version makes an identical rerun reuse the completed evidence rather than
duplicate it. A changed digest is a new auditable run; it never overwrites the earlier source claim.

No outbox event is emitted by import. This avoids presenting legacy evidence to downstream consumers
as official state and avoids an undeclared cross-repository contract. Report counts are computed from
persisted outcomes and sorted stable keys, not mutable input order.

### D6: Compare immutable import and complete-snapshot sets without side effects

`shadow-report` accepts one completed import run and one completed authoritative snapshot for the
same account. It joins primarily on provider post ID and records URL-derived secondary matches only
when the normalized identity is unambiguous. Diff classes are provider-ID match, URL-only match,
legacy-only, official-only, identity conflict, and unmapped; content, category, and folder evidence
are orthogonal flags.

Canonical JSON sorts class and account-scoped SHA-256 references before hashing. It includes counts,
input IDs/digests, source/parser versions, and completeness evidence, but no post bodies, handles,
raw URLs, credentials, or filesystem paths. Generating a report is a read-only operation except for
persisting that exact report. It never calls snapshot reconciliation.

### D7: Cutover approval is a recorded evidence gate, not an implicit deployment

`checklist` generates a digest-bound Markdown artifact covering source backup, import counts,
complete snapshot, shadow findings, privacy review, cost, workspace rollout, stability window,
rollback commands, and evidence-retention checks. `record-approval` accepts an explicit owner
decision for the exact checklist/report digest and marks older approval superseded when any bound
input changes.

This is a real local approval capability, but it intentionally cannot modify fleet schedules or
client routing. Those external writes require the owner-approved `ratatoskr-workspace` changeset
named in the checklist. Rollback changes that workspace routing while keeping both X evidence sets.

## Risks / Trade-offs

- [A private archive may use an older or forked Field Theory shape] → refuse unknown versions and
  produce a field-level preflight report; add a new fixture-backed adapter only from owner-provided
  evidence.
- [SQLite support increases the SQLx build graph] → enable only the pinned existing SQLx SQLite
  feature, run the full release/build/security gate, and do not add a second database library unless
  separately approved.
- [Legacy rows may lack stable author identity] → retain nullable imported author linkage and block
  social-source publication until official normalization resolves it.
- [Large archives could hold a transaction too long] → enforce configured byte/row limits and fail
  preflight rather than silently chunking one evidence digest into partially visible state.
- [SHA-256 references to public post IDs may be guessable] → scope hashes with the account and report
  domain; treat reports as owner-only artifacts and never metric labels.
- [A clean set comparison can still miss semantic corruption] → keep content/category/folder flags,
  require human checklist review, and bind approval to exact immutable inputs.
- [The real archive remains unavailable] → prove behavior with pinned synthetic/redacted fixtures and
  report real import/cutover as unexecuted until the owner supplies data and approval.

## Migration Plan

1. Apply the edited current schema definition in a disposable database and prove reapplication.
2. Run source preflight and idempotence tests against synthetic monolith CSV, Field Theory JSONL v1,
   and SQLite v6 fixtures.
3. Deploy the operator binary without invoking it; normal OAuth sync and authority are unchanged.
4. When the owner supplies a read-only source, back it up, record its digest, approve the target
   account mapping, and run preflight/import.
5. Complete a fresh official full snapshot, generate the shadow report and checklist, and obtain
   owner approval for the exact evidence digest.
6. Execute the separately reviewed workspace cutover during its stability window. Roll back its
   routing/schedule changes if acceptance fails; do not delete import, snapshot, report, or approval
   evidence.

## Open Questions

- The real archive path, exact source version, and row count remain operational inputs; preflight is
  designed to answer them without changing this behavior contract.
