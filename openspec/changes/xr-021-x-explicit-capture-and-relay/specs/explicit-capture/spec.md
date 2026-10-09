## Purpose

Defines the owner-scoped explicit browser-capture lane of the X service: how an accepted capture command is kept, resolved through the public app-only X API, retried, published and reported, without ever exposing another account's synced posts.

## ADDED Requirements

### Requirement: Intake keeps the owner and queues the operation

The service SHALL store, for every accepted `social.capture.requested.v1` command for provider X, the owning user taken from the command tenant, the operation id and the provider post id parsed from the permalink, and SHALL queue the operation by writing a `queued` operation report in the same transaction that claims the broker delivery. It SHALL accept status permalinks on x.com, www.x.com, mobile.x.com, twitter.com, www.twitter.com and mobile.twitter.com. A command without a tenant SHALL be refused as poison. A permalink without a status id SHALL be stored as unavailable together with a terminal `failed` report in the same transaction.

#### Scenario: an accepted command keeps its owner and queues the operation

- **WHEN** a valid X browser-capture command for a tenant is delivered
- **THEN** one explicit-capture row holds the tenant as owner, the operation id, the provider post id and status `accepted`, and one `platform.operation.reported.v1` envelope with status `queued` and stage `capture_queued` is outbox-bound for that tenant

#### Scenario: a redelivered command adds nothing

- **WHEN** the same command is delivered again
- **THEN** no capture row and no outbox row is added

#### Scenario: a permalink without a status id is reported unavailable

- **WHEN** the command names a permalink that has no status id
- **THEN** the capture is stored with status `unavailable` and a terminal `failed` report with code `social.source.unavailable` and retryable false is written in the same transaction

### Requirement: Explicit capture never publishes another tenant's data

The explicit lane SHALL publish only what the public app-only resolution returned for the capturing owner. It SHALL NOT read posts, users, accounts, bookmarks or social sources that belong to account synchronization.

#### Scenario: a protected post synced for another account is not published

- **WHEN** tenant B captures a permalink whose shared synced post row was only visible to account A and the public resolution reports it not authorized
- **THEN** the operation ends `failed` with `social.source.unavailable`, no explicit source and no social event exists, and the shared post row is untouched

### Requirement: Resolution is public, typed and fail-closed

The service SHALL resolve a post with the app-only bearer credential and no user credential. It SHALL map a found post to a preserved source, a resource-not-found error to Deleted, not-authorized, unavailable and forbidden errors to Inaccessible, and rate limits, server errors, timeouts and transport errors to Transient. A whole-request 401 or 403 SHALL be Transient and SHALL increment `x_public_capture_credential_rejected_total`. The bearer token SHALL NOT appear in debug output, errors or logs. The service SHALL refuse to start when the bus is configured and the bearer token file is absent.

#### Scenario: the request carries the app-only credential and nothing else

- **WHEN** the resolver fetches a status id
- **THEN** the request has the bearer header, the `ids` parameter, the documented expansions and field lists, and no cookie or user header

### Requirement: Retry is bounded and the terminal report is sent once

A transient failure at attempt n SHALL delay the next attempt by `min(30 s * 4^(n-1), 30 min)`, and the fifth transient failure SHALL be a terminal `failed` report with retryable true. Permanent classes SHALL terminate immediately. Exactly one terminal report SHALL exist per operation, guarded by a once-only marker updated in the transaction that writes the report. Two workers racing for one capture SHALL resolve it once.

#### Scenario: transient failures back off and then fail

- **WHEN** the resolver reports Transient on five consecutive attempts
- **THEN** the next-attempt delays are 30 s, 2 min, 8 min and 30 min, and the fifth failure writes one terminal `failed` report with retryable true

### Requirement: Re-capture by the same owner is idempotent

A re-capture of the same owner and post SHALL re-resolve, update the explicit source, emit `social.source.updated.v1` only when the content digest changed, and always report success for the new operation. Two owners capturing one public post SHALL get distinct social source ids and only their own events.

#### Scenario: an unchanged re-capture reports success without a new fact

- **WHEN** the same owner captures the same post again and the content is unchanged
- **THEN** no social event is added and a `succeeded` report for the new operation points at the existing source
