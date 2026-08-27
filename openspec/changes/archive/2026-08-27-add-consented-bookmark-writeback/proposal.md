## Why

The legacy monolith never mutated X, and the current connector intentionally keeps read access
separate from provider writes. Ratatoskr now needs narrowly bounded bookmark add/remove operations
without turning a read connection, a replayed command, a dry run, or an exhausted read budget into
write authority.

## What Changes

- Add bookmark add/remove application operations for the connected account through an official X
  provider adapter; posting, liking, reposting, folder mutation, and every other provider write stay
  unsupported.
- Require both a separately granted bookmark-write OAuth scope and explicit consent evidence for the
  exact action, target, account owner, approval time, and initiating surface on every live request.
- Make execution replay-safe under a caller-supplied idempotency key, including stored terminal
  outcomes and rejection of key reuse with different request content.
- Add a dry-run mode that performs the same ownership, scope, consent, target-state, and budget
  eligibility checks and returns the corresponding would-be outcome without reserving budget or
  contacting X.
- Put provider mutations behind their own hard durable write budget class, independent of all read
  and synchronization allowances.
- Persist an append-only, secret-free audit trail for accepted, refused, dry-run, replayed, attempted,
  uncertain, and completed operations together with provider request/result evidence.
- Keep bulk operations out of scope; every future bulk run would require its own explicit per-run
  consent and is not implemented by this change.

## Capabilities

### New Capabilities

- `bookmark-writeback`: Explicitly consented, dry-runnable, idempotent bookmark add/remove execution
  with a complete audit trail and no other X mutation authority.

### Modified Capabilities

- `oauth-connection`: Add a separate bookmark-write authorization extension and independent removal
  of write authority without widening the default read connection.
- `api-budget`: Partition durable provider cost accounting by hard budget class so bookmark writes
  cannot consume or borrow read allowance.
- `x-archive-schema`: Add in-place first-version consent, operation, audit, and classed budget state
  required by write-back.

## Impact

- Affects `x-oauth`, `x-budget`, `x-persistence`, `x-sync`, `schema.sql`, their integration tests,
  and the repository's README/architecture/testing documentation.
- Adds an internal application/provider seam only; no shared event contract, cross-repository API,
  production HTTP route, UI surface, migration file, live credential, or personal-account test is
  introduced.
- The caller remains responsible for authenticating the internal user and supplying a validated
  initiating-surface value. Live runtime/UI wiring is a separate cross-repository change.
- No production dependency is expected. Synthetic fakes and disposable PostgreSQL remain the
  validation boundary.
