## Context

See `proposal.md` and the four delta specs. The current repository already has a PKCE OAuth service
with one active encrypted credential per account, durable fixed-window request budgets keyed only by
account/time, an owned PostgreSQL schema, and application services in `x-sync` behind hand-written
provider traits. It does not have a write authorization purpose, consent/operation/audit persistence,
budget classes, or an official bookmark-mutation HTTP adapter.

The official X API currently documents OAuth 2.0 PKCE with `users.read`, `tweet.read`, and
`bookmark.write` for bookmark mutations, POST `/2/users/{id}/bookmarks` with `tweet_id`, and DELETE
`/2/users/{id}/bookmarks/{tweet_id}`. The documentation does not promise two independent OAuth 2.0
credential families for the same app/user, so this design does not depend on that behavior. The
service remains on its first API/schema version and edits `schema.sql` in place without migrations.

## Goals / Non-Goals

**Goals:**

- Provide a callable production application service and concrete official-X HTTP adapter for one
  add/remove action, while keeping every authority gate inside the X bounded context.
- Make consent, idempotency, budget admission, provider evidence, and projection changes durable
  enough to survive retries and process restarts.
- Keep dry-run and uncertain-result reporting honest: previews are local observations, and a
  possibly accepted mutation is never repeated merely because the caller retried.
- Preserve the existing full-snapshot absence invariant while allowing confirmed provider writes to
  update known bookmark projections with distinct evidence.

**Non-Goals:**

- Adding a public HTTP/message command, Platform/UI contract, or live deployment wiring.
- Posting, liking, reposting, native-folder mutation, automated writes, or bulk execution.
- Selective provider-scope revocation, which the current official documentation does not define.
- Browser-session endpoints, personal-account fixtures, retry loops, or a migration framework.

## Decisions

### D1: OAuth re-consent widens one credential family and records local write authority

`x-oauth` gains an authorization-intent purpose (`read_connection` or `bookmark_write`) and optional
existing-account binding. The bookmark-write URL requests the configured read scopes plus only
`bookmark.write`; this preserves the scopes required by background read work while showing a
separate provider consent screen. Callback admission validates the normal PKCE/state/owner/redirect
matrix and calls the authenticated-user endpoint before accepting that the grant belongs to the
existing provider account.

Only a complete matching grant atomically replaces the encrypted credential and activates the
account's `bookmark_write_authorizations` row. A downgrade or identity mismatch records bounded
evidence but leaves the last read credential intact. The local authorization row is an additional
gate; the presence of `bookmark.write` in token scopes alone cannot authorize a mutation.

Alternative: store a second read/write token family. Rejected because the official documentation
does not promise independent parallel grants, and relying on it would fabricate a provider
capability. Alternative: add `bookmark.write` to default connection scopes. Rejected because it
violates least privilege and the explicit separate-consent boundary.

### D2: Consent is a short-lived, one-action durable capability

`BookmarkWritebackService::record_consent` receives trusted authenticated-owner context plus account,
action, provider post id, approval instant, and a validated bounded `ConsentSurfaceId`. It verifies
owner/account binding and stores an immutable consent with a generated identifier and expiry derived
from a configured maximum age. The clock is injected; approval in the future or outside the allowed
age is refused. The surface value is syntax/length validated locally, while the future shared
Platform integration owns the cross-repository vocabulary.

A live operation locks the consent row and consumes it only after every local gate and the write
budget reservation succeed. A dry run locks nothing and never consumes it. One consent may therefore
support previews and exactly one live attempt, but never a different action/target or a later bulk
run.

Alternative: accept a boolean `confirmed` on the mutation command. Rejected because it cannot prove
who approved what or prevent replay. Alternative: reuse OAuth consent as action consent. Rejected
because a broad provider grant is not explicit approval of one destructive or externally visible
action.

### D3: One durable operation row claims an idempotency key before side effects

`bookmark_write_operations` has a unique `(account_id, idempotency_digest)` and stores a SHA-256
request fingerprint over owner, account, action, target, consent, and execution mode. The raw caller
key is not retained. Insertion elects one executor; exact concurrent/replayed calls observe the same
operation, while a different fingerprint is a conflict. The executor appends `received`, validates
gates, and then advances through a closed state machine:

```text
received -> refused | dry_run | provider_in_flight
provider_in_flight -> succeeded | failed | uncertain | projection_pending
uncertain -> reconciled_succeeded | reconciled_not_current
```

The elected live executor opens a transaction that locks the consent, rechecks every local gate,
and asks the classed budget gate to reserve outside the consent row's key space. On refusal it rolls
back without consumption. On acceptance it consumes consent and commits `provider_in_flight` before
network I/O. If this commit fails after reservation, the service refunds that exact reservation;
once provider contact begins, the conservative charge is retained.

Exact retries of every terminal or uncertain state return the stored result and append `replayed`;
they never call X again. In-flight retries return the existing in-progress identity rather than
electing another executor. A budget refusal is terminal for that idempotency key, but leaves the
consent available for a new explicitly keyed attempt after reset.

Alternative: hold one database transaction open across the HTTP request. Rejected because it would
hold consent/operation locks during an unbounded external dependency. Alternative: depend on X to
deduplicate requests. Rejected because the documented bookmark endpoints expose no idempotency key.

### D4: Dry run uses the same decision engine and a read-only budget inspection

One pure admission evaluator produces typed `Admitted`, `AlreadySatisfied`, or `Refused` decisions
from account/credential/write-authorization/consent/projection/budget facts. Live execution feeds it
a locked consent and an actual `BudgetGate::reserve`; dry run feeds it the same facts plus
`BudgetGate::inspect`, which computes the current class/window decision without inserting or
updating a row.

Dry-run output includes the local observation instant, evaluation instant, and budget reset where
relevant, and explicitly marks itself advisory. It is stored as an audited operation but does not
consume consent, charge cost, contact X, or change the bookmark. Live execution always re-evaluates;
a preview is never an authorization or reservation.

Alternative: implement dry run as a provider request followed by no mutation. Rejected because X
does not expose such a mutation preview and a network call would still consume rate/cost budget.

### D5: Budget class is part of every durable window identity

`BudgetClass` is a closed Rust enum persisted as a checked token. Existing callers explicitly use
their read class; write-back and later reconciliation provider calls use `bookmark_write`.
`api_budget_windows` changes its primary key to `(account_id, budget_class, window_start)`, and every
charge/refund/peek query includes the class. `BudgetGate` is constructed for exactly one class and
one cap so an application service cannot choose a different pool per request.

Alternative: create a second write-budget table. Rejected because it duplicates concurrency,
rollover, and refund logic. Alternative: use only a per-run cap. Rejected because it does not survive
restart or isolate concurrent processes.

### D6: The production adapter sends exactly the documented bookmark requests

An adapter module inside `x-sync::writeback` owns all `reqwest` request/response shapes. It loads and
decrypts the account credential behind the service boundary, verifies the local write authorization
and current scope set again, derives the path user id from the connected account, and issues only:

- POST `/2/users/{provider_user_id}/bookmarks` with `{ "tweet_id": target }`;
- DELETE `/2/users/{provider_user_id}/bookmarks/{target}`.

It uses the existing Rustls client, an end-to-end timeout, bounded response bodies, and no automatic
retry. A successful body must contain the expected `bookmarked` boolean. It retains only bounded
request id/rate metadata. Connect failures known to precede request transmission are transient;
timeouts, connection loss after transmission, malformed success bodies, and ambiguous server
failures are uncertain. Authentication loss, rate limiting, and definite provider refusals remain
distinct. Debug/error types redact authorization data. WireMock tests prove method/path/body,
response classification, size/timeout behavior, and secrecy with synthetic tokens.

Alternative: expose raw `reqwest` responses to the application service. Rejected because provider
shapes and sensitive headers must remain inside adapters.

### D7: Confirmed writes and later snapshots carry different authority evidence

A confirmed response updates a known target bookmark and operation in one transaction, setting
honest observed instants and a `last_write_operation_id`; remove evidence uses a distinct
`observed_removed_write_operation_id`, not the complete-snapshot foreign key. If the post is not
normalized locally, provider success is retained as `projection_pending` and no placeholder post is
created. A later scan performs ordinary normalization and reconciliation.

Uncertain operations are resolved only from a later complete successful snapshot: presence resolves
an add as current; absence resolves a remove as current. The opposite result becomes
`reconciled_not_current`, because the service cannot distinguish a mutation that never applied from a
later user action. Snapshot finalization appends reconciliation audit evidence and never initiates a
new mutation.

Alternative: repeat an uncertain mutation on retry. Rejected because the first request may have
succeeded. Alternative: infer absence from a partial scan. Rejected by the repository's core
snapshot-authority invariant.

### D8: Audit is append-only application evidence, not a log substitute

Every operation has ordered `bookmark_write_audit_events` rows with a closed event class and a JSON
details object validated by the writing function to bounded, non-sensitive fields. Consent evidence
is immutable; operation status is the current projection, while audit events are the history. Normal
repository APIs expose insert/list only for audit rows. Tests reconstruct the full success path and
the refusal/dry-run paths, and scan stored/debug values for marker tokens, authorization headers,
private post bodies, and raw response fragments.

Alternative: rely on tracing. Rejected because logs are neither transactionally durable nor a
complete user-action ledger.

## Risks / Trade-offs

- [A write re-consent replaces the active token family] -> Validate the complete read-plus-write
  scope set and provider identity before one atomic swap; retain the old credential on any failure.
- [Dry-run budget/state can change immediately afterwards] -> Return evaluation/evidence instants,
  call it advisory, and repeat all admission under live execution.
- [Process loss after provider contact can leave outcome uncertain] -> Commit `provider_in_flight`
  before I/O, never auto-retry it, and resolve only from a later complete snapshot.
- [Confirmed add targets a post absent from local normalization] -> Retain provider success as
  `projection_pending` and let supported read synchronization create the post; never fabricate it.
- [The X API contract or charging model changes] -> Keep shapes in one adapter, use redacted
  fixtures, and preserve a separately configured hard write class whose cap can be set to zero.
- [No external command/UI is wired by this repository-local change] -> Deliver the real callable
  application service and concrete adapter, document the boundary, and make no live-deployment claim.

## Migration Plan

There is no database migration. Apply the edited first-version `schema.sql` to a fresh disposable
development database. Deploy the schema and code with the bookmark-write budget cap at zero, then
configure the official base URL and non-zero write cap, and only then add a separate workspace
changeset for an authenticated Platform/UI surface that records consent and invokes the service.
Rollback disables the write cap and caller wiring first, then restores the previous code/schema only
in disposable development environments. Existing provider mutations and audit evidence are external
facts and must never be represented as rolled back.
