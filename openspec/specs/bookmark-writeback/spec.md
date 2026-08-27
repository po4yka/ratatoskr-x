# bookmark-writeback Specification

## Purpose

Defines the only provider write capability owned by this service: explicitly consented, replay-safe
bookmark add/remove operations whose dry runs, budget admission, provider evidence, and audit history
remain truthful and isolated from every other X action.

## Requirements

### Requirement: Mutation authority is limited to connected-account bookmarks

The service SHALL accept only bookmark add and bookmark remove actions for the provider account bound
to the authenticated internal owner. It SHALL derive the provider user identity from that connected
account, never from an untrusted request field, and SHALL reject posting, liking, reposting, folder
mutation, bulk mutation, or any other X write without reserving budget or contacting the provider.
Source content, imported data, local deletion, and automated analysis SHALL NOT create a mutation
request.

#### Scenario: Non-bookmark action is refused locally

- **WHEN** an authenticated owner asks the write-back service to perform a like, repost, post, folder mutation, or bulk mutation
- **THEN** the request is refused as unsupported, an audit refusal is retained, and no provider call or budget charge occurs

#### Scenario: Connected provider identity cannot be substituted

- **WHEN** a request targets a provider user identity other than the identity bound to the owned connected account
- **THEN** the request is refused before provider contact and the attempted substitution is audited without retaining credentials

### Requirement: Every live action consumes matching explicit consent

Before a live add or remove can execute, the service SHALL hold an unexpired consent record created
for the authenticated internal owner and bound immutably to one account, one action, one provider post
identity, the approval instant, and the initiating surface. A live operation SHALL atomically consume
that consent at most once. Missing, expired, already-consumed, foreign-owner, wrong-account,
wrong-action, or wrong-target consent SHALL block the write before budget reservation and provider
contact. Recording consent SHALL NOT itself invoke X.

#### Scenario: Unconsented write is blocked

- **WHEN** a live bookmark add or remove has no matching unexpired consent record
- **THEN** the operation is refused, the consent-gate decision is audited, and neither the write budget nor the provider is touched

#### Scenario: Consent evidence records who approved what, when, and where

- **WHEN** an authenticated owner approves one bookmark action through an initiating surface
- **THEN** the retained consent identifies that owner, account, add-or-remove action, target provider post, approval instant, and surface without containing an OAuth secret

#### Scenario: Consent cannot authorize a different mutation

- **WHEN** consent recorded for one action or target is presented for a different action, target, account, or owner
- **THEN** the live operation is refused before budget or provider contact and the mismatch is audited

#### Scenario: Concurrent consumers use consent once

- **WHEN** two live operations race to consume the same matching consent
- **THEN** at most one operation is admitted to provider execution and the other observes a consumed-consent refusal

### Requirement: Write scope and connection state gate live execution

A live operation SHALL require an active read connection and an independently active local write
authorization whose current encrypted credential grant includes the exact provider prerequisites and
`bookmark.write`. A read credential, per-action consent, or provider scope alone SHALL NOT substitute
for the other gates. Missing or revoked write authority, ownership mismatch, inactive connection, or
downgraded scope SHALL be refused before budget reservation and provider contact.

#### Scenario: Read-only account cannot mutate bookmarks

- **WHEN** a connected account has the normal read credential and valid per-action consent but no active write authorization
- **THEN** the operation is refused as write authorization required and no provider call occurs

#### Scenario: Write grant without action consent cannot mutate bookmarks

- **WHEN** an account has an active bookmark-write credential but the request lacks matching per-action consent
- **THEN** the consent gate refuses the operation and the provider receives no request

### Requirement: Live execution is idempotent under an account-scoped key

Each live request SHALL carry an idempotency key unique within the connected account. The first
request SHALL durably bind that key to a canonical fingerprint of owner, account, action, target, and
consent before provider contact. An exact retry SHALL return the stored outcome without consuming
another consent, reserving more budget, or repeating a completed provider mutation. Reusing the key
with a different fingerprint SHALL be refused. Concurrent exact requests SHALL converge on one
provider attempt.

#### Scenario: Exact retry returns the first result

- **WHEN** a completed bookmark mutation is submitted again with the same account, key, and request fingerprint
- **THEN** the stored result is returned, one replay audit event is appended, and provider-call and write-budget counts remain unchanged

#### Scenario: Key reuse with changed content is rejected

- **WHEN** an existing account-scoped idempotency key is submitted with a different action, target, owner, or consent
- **THEN** the request is refused as an idempotency conflict and the original operation and result remain unchanged

#### Scenario: Concurrent exact requests call the provider once

- **WHEN** two identical live requests with one idempotency key arrive concurrently
- **THEN** both callers converge on one durable operation outcome produced by at most one provider mutation attempt

### Requirement: Dry run mirrors local admission without side effects

A dry run SHALL evaluate the same owner, account, supported-action, active-write-authorization,
consent binding and freshness, current durable bookmark observation, and write-budget eligibility
used by live admission. It SHALL return a typed would-submit, would-already-satisfy, or would-refuse
outcome carrying the observation instant and any budget reset instant that informed it. A dry run
SHALL NOT consume consent, reserve budget, contact X, or change bookmark projection state, and its
outcome SHALL state that provider state can change after the preview. Every dry run SHALL be audited.

#### Scenario: Eligible dry run reports the live request it would make

- **WHEN** a dry-run bookmark action passes every local live gate and the durable projection does not already prove the desired state
- **THEN** the result names the would-be add or remove request and its evidence instants while provider calls, budget usage, consent consumption, and bookmark state remain unchanged

#### Scenario: Dry run faithfully reports a live refusal

- **WHEN** a dry run lacks matching consent, write scope, ownership, connection state, or available write budget
- **THEN** it returns the same typed local refusal class that a live request would reach at admission and still performs no provider work or durable charge

#### Scenario: Dry run recognizes already satisfied durable state

- **WHEN** the latest durable authoritative evidence already establishes the requested bookmark state
- **THEN** the dry run reports would-already-satisfy with that evidence instant and makes no provider claim newer than the evidence

### Requirement: Every provider request uses the isolated write budget

Immediately before every bookmark mutation provider call, the service SHALL reserve the operation's
cost from the hard bookmark-write budget class. A refusal SHALL leave the consent available for a
later live attempt, record the reset instant in the operation and audit trail, and perform no
provider call. Read and synchronization allowance SHALL NOT admit, fund, or be reduced by a
bookmark-write request.

#### Scenario: Exhausted write budget blocks a live mutation

- **WHEN** an otherwise admitted bookmark action cannot reserve its required bookmark-write cost
- **THEN** the operation returns a budget refusal with reset time, records no provider attempt, leaves consent unconsumed, and does not change any read-budget usage

#### Scenario: Read allowance cannot be borrowed for a write

- **WHEN** the bookmark-write class is exhausted while a read class still has allowance
- **THEN** the live mutation remains blocked and the read allowance stays unchanged

### Requirement: Provider outcomes and local projection are truthful

The provider adapter SHALL expose only bookmark add and remove for the connected account and SHALL
classify success, already-satisfied state, authorization loss, rate limit with reset evidence,
definite refusal, transient failure before acceptance, and uncertain mutation outcome. A confirmed
provider result SHALL update the account bookmark projection and operation result together with its
non-sensitive provider request evidence when the normalized target exists. If the provider confirms
an add for a target not yet normalized locally, the result SHALL state that provider mutation
succeeded while projection reconciliation remains pending rather than creating an invented post. An
uncertain outcome SHALL remain visibly uncertain and
an exact retry SHALL NOT blindly repeat the mutation; later authoritative reconciliation is required
before another provider mutation can be admitted for that operation.

#### Scenario: Confirmed add updates bookmark state with operation evidence

- **WHEN** X confirms that the connected account now bookmarks the target post
- **THEN** the operation completes as added or already present and the local active bookmark observation references the confirming write operation without inventing a native save timestamp

#### Scenario: Confirmed remove updates bookmark state with operation evidence

- **WHEN** X confirms that the connected account no longer bookmarks the target post
- **THEN** the operation completes as removed or already absent and the local removal observation references the confirming write operation without inventing a native removal timestamp

#### Scenario: Confirmed unknown target reports pending projection

- **WHEN** X confirms a bookmark add for a provider post that has no normalized local post row
- **THEN** the operation reports provider success with projection reconciliation pending, retains the provider evidence, and creates no fabricated normalized post or bookmark row

#### Scenario: Uncertain result is not retried blindly

- **WHEN** provider acceptance cannot be determined after a mutation request may have reached X
- **THEN** the operation is stored and returned as uncertain, an exact retry performs no second mutation, and only later authoritative state evidence can resolve it

### Requirement: Audit trail is complete, append-only, and secret-free

The service SHALL append ordered audit evidence for consent recording, request receipt, gate
admission or refusal, dry-run result, idempotent replay or conflict, budget refusal, provider attempt,
provider classification, local reconciliation, uncertain state, and terminal result whenever those
stages occur. Each entry SHALL retain operation/account identity, actor, action, target, initiating
surface, occurrence time, correlation and idempotency identity, and bounded provider request/result
evidence applicable to that stage. Audit rows SHALL NOT be updated or deleted by normal operation and
SHALL NOT contain tokens, authorization headers, private post bodies, or raw provider responses.

#### Scenario: Successful write has end-to-end audit evidence

- **WHEN** a consented bookmark mutation succeeds
- **THEN** ordered audit entries reconstruct approval, admission, the single budgeted provider attempt, provider classification, local reconciliation, and returned result with actor, action, target, time, and surface intact

#### Scenario: Refusal and dry run remain auditable

- **WHEN** a request is refused before provider contact or completes as a dry run
- **THEN** its audit history records the deciding gates and truthful result without fabricating a provider request

#### Scenario: Audit diagnostics exclude credentials and content

- **WHEN** persisted audit rows and their diagnostic renderings are inspected using marker secrets and private post text
- **THEN** none of those marker values or raw provider bodies appear
