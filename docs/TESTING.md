# X connector testing strategy

Required tests:

- OAuth PKCE/state/account binding, encryption, refresh, revoke, and scope downgrade.
- Post/reply/quote/repost/media/URL normalization and unknown variants.
- Partial and full bookmark pagination, interruption, duplicates, checkpoints, and false-removal prevention.
- Folder listing/membership reconciliation.
- Honest observation timestamps.
- Idempotent add/remove mutations and partial provider failures.
- Rate-limit/credit budget, retry-after, reauthorization, deleted/protected/suspended states.
- SQL schema initialization, outbox/inbox replay, authorization, and no-content logging.
- Concurrent SocialSource request deduplication and exact current/historical Knowledge completion
  linkage, including replay and removed-source rejection.
- Compliance due/bound/account selection, one budget charge per provider call, available and
  indeterminate ledger evidence, atomic authoritative takedown, singleton tombstone/removal event,
  and delayed-work resurrection prevention.
- Field Theory legacy import/shadow reconciliation.

Default tests use synthetic/WireMock fixtures; optional sandbox tests use a dedicated account and explicit budget.

Compliance integration tests use a classified fake behind the official-provider seam. They prove
selection, persistence, and takedown state-machine behavior; they are not evidence that an HTTP
provider adapter, scheduler, message broker, or live personal X account has been exercised.

## Test-first

A change is planned before it is built, and the plan is a task list in which behaviour arrives in
pairs: one task adds a failing test, the next makes it pass. `openspec/config.yaml` carries that
rule, which is what puts it into every planning and implementation request rather than only into this
document.

The loop:

1. Write the test the scenario names. Run it. Confirm it fails, and read the failure — a test that
   fails because it does not compile has proved nothing about the behaviour.
2. Write the smallest change that makes it pass. Run it again.
3. Refactor only once it is green, adding no test and changing no behaviour.

Two checks stand behind this, and neither of them can see the order:

- `openspec validate --archived`, in `.github/workflows/openspec.yml`, fails when a change was
  archived with a task left unticked.
- A step in `.github/workflows/fleet.yml` fails when this repository holds a manifest and a `ci.yml`
  that never runs a test.

`ratatoskr-workspace/docs/QUALITY_GATES.md` records why the order itself is not checkable, rather
than leaving the gap to be discovered.
