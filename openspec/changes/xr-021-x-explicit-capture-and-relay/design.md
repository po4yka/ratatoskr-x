## Context

The authoritative cross-repository contract is XR-021 CONTRACTS.md. This document records only decisions internal to `ratatoskr-x`.

## Decisions

### One envelope helper in `x-persistence`

`x-capture` and `x-sync` are siblings that both depend on `x-persistence`, so `crates/x-persistence/src/outbox.rs` owns `enqueue_event` and `enqueue_command`. Each takes a complete envelope and inserts `(id = event or command id, aggregate, event_type, payload = envelope JSON, correlation_id, causation_id)` in the caller's transaction. Callers mint the UUIDv7 id with the contract id types, so the id column and the envelope id cannot differ. Report envelopes come from the contracts `report_envelope`.

### Owner scoping replaces the shared-post read

The explicit lane never reads `x_archive.posts`, users, accounts, bookmarks or social_sources. Its own tables are `explicit_captures` (queue and retry ledger, owner from the command tenant) and `explicit_sources` (`UNIQUE (owner, provider_post_id)`, what the public call returned for that owner). Two tenants capturing one public post get distinct `social_source_id`s. A source-grep test asserts the worker module never names the forbidden tables.

### The shared source tail

`social_sources.rs` keeps `snapshot()`, `content_digest()` and `wire_timestamp()` as `pub(crate)` functions over plain inputs, and the bookmark path and the explicit path share one tail that writes a complete envelope. `wire_timestamp` emits the canonical contract form (fraction trimmed of trailing zeros) so a value such as `.120` can no longer produce a snapshot that the contract rejects.

### Resolution

`PublicPostResolver` is a trait over `provider_post_id`, with typed failures `Deleted`, `Inaccessible` and `Transient`. `AppBearerResolver` calls `GET {api_base_url}/2/tweets?ids=...` with the CD5 expansions and field lists, parses with `x_normalize::dto::Envelope` and `normalize`, never follows redirects, caps the body at 1 MiB and never prints the token. A whole-request 401 or 403 is `Transient` plus an error log and the metric `x_public_capture_credential_rejected_total`, because it is an operator problem, not a property of the post.

### Worker

`PublicCaptureWorker::run_due_once` claims due `accepted` rows with `FOR UPDATE SKIP LOCKED` and a 120 second lease (`next_attempt_at` pushed forward in the claiming transaction), resolves outside any transaction, then per outcome commits one transaction that updates the capture, upserts `explicit_sources`, appends a captured or updated envelope when the digest changed, and inserts the terminal report guarded by `UPDATE explicit_captures SET reported_at = now() WHERE capture_id = $1 AND reported_at IS NULL`. Retry delays are `min(30 s * 4^(n-1), 30 min)` and the fifth transient failure is terminal.

### Relay and lifecycle

`OutboxRelay::run_once` selects unpublished due rows ordered by `(created_at, id)`, publishes each with `Nats-Msg-Id`, awaits the `PubAck`, and only then sets `published_at`. A failed publish records `attempt_count`, a safe `last_error` class and `next_attempt_at` and the pass continues with later rows, so a failing head row never starves the rest. A row whose `event_type` has no subject is a programming error: the relay stops and the service flips readiness. `RuntimeState::set_bus_ready` is flipped by any bus task returning, and the service exits with a failure code.

## Risks

- Per S03 the shared subject `evt.platform.operation.reported.v1` is trusted by the envelope `producer` field; this is the documented residual risk, not fixed here.
- The X endpoint parameters are confirmed against the current X API documentation before the resolver is finalized.
