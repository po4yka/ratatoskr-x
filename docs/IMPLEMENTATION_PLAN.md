# X connector implementation plan

1. Scaffold Rust service, typed config, telemetry, health, errors, and `x_archive` schema.
2. Implement encrypted OAuth PKCE connection, scopes, refresh, revoke, and budgets.
3. Implement post/author/relation/media normalization.
4. Implement complete bookmark snapshot with atomic authority and checkpoints.
5. Add safe frequent partial scans.
6. Implement folder and membership snapshots.
7. Publish normalized SocialSource and linked-article extraction events.
8. Add Knowledge integration and compliance revalidation. *(implemented: durable application
   services and contracts; runtime scheduler/provider adapter/outbox transport wiring pending)*
9. Add separately consented idempotent bookmark write-back. *(implemented: add/remove-only library
   service, separate OAuth and per-action consent, dry run, isolated budget, official adapter,
   uncertainty reconciliation, and append-only audit; external authenticated runtime/UI wiring is
   pending a workspace change)*
10. Import Field Theory data, compare shadow snapshots, then cut over.

Definition of Done: no false removals, read/write scopes secure, cost bounded, private content authorized, schema/events/tests and the planned workspace vertical slice pass. Deferred: DMs and broad account/social graph ingestion.
