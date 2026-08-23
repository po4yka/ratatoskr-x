# X connector implementation plan

1. Scaffold Rust service, typed config, telemetry, health, errors, and `x_archive` schema.
2. Implement encrypted OAuth PKCE connection, scopes, refresh, revoke, and budgets.
3. Implement post/author/relation/media normalization.
4. Implement complete bookmark snapshot with atomic authority and checkpoints.
5. Add safe frequent partial scans.
6. Implement folder and membership snapshots.
7. Publish normalized SocialSource and linked-article extraction events.
8. Add Knowledge integration and compliance revalidation.
9. Add separately consented idempotent bookmark write-back.
10. Import Field Theory data, compare shadow snapshots, then cut over.

Definition of Done: no false removals, read/write scopes secure, cost bounded, private content authorized, schema/events/tests and the planned workspace vertical slice pass. Deferred: DMs and broad account/social graph ingestion.
