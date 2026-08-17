# X connector testing strategy

Required tests:

- OAuth PKCE/state/account binding, encryption, refresh, revoke, and scope downgrade.
- Post/reply/quote/repost/media/URL normalization and unknown variants.
- Partial and full bookmark pagination, interruption, duplicates, checkpoints, and false-removal prevention.
- Folder listing/membership reconciliation.
- Honest observation timestamps.
- Idempotent add/remove mutations and partial provider failures.
- Rate-limit/credit budget, retry-after, reauthorization, deleted/protected/suspended states.
- SQL migrations, outbox/inbox replay, authorization, and no-content logging.
- Field Theory legacy import/shadow reconciliation.

Default tests use synthetic/WireMock fixtures; optional sandbox tests use a dedicated account and explicit budget.
