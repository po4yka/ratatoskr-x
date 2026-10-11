## Context

The authoritative cross-repository text is XR-021 CONTRACTS.md R2-09, R2-03, R2-04 and R2-10. This document records only decisions internal to `ratatoskr-x`.

## Decisions

### Removal happens inside `terminate`

`PublicCaptureWorker::terminate` already commits the capture status and the single terminal report in one transaction. For a `Deleted` failure it first takes the same advisory lock `preserve` takes for `(owner, provider_post_id)`, deletes the owner's `explicit_sources` row with `RETURNING social_source_id`, and queues a `SocialSourceRemoved` envelope built with the shared `queue_event` helper. The delete and the envelope are guarded by the same `reported_at is null` update as the report, so a redelivery removes and reports nothing twice. No removed marker is kept: a later capture of a restored post is a first capture and preserves a new source with a new identity.

### Relay health is a shared flag, not a task result

`RelayHealth` wraps an `Arc<AtomicBool>` (initially healthy). After each pass `run_once` decides: when the pass attempted a publish and one failed, the relay reads the oldest unpublished publishable row (`undeliverable_at is null`) and is unhealthy when its `attempt_count >= 3` or it is older than 300 seconds; a pass that fails nothing, including an empty one, is healthy. `services/x` reads the flag into `RuntimeState::set_bus_ready` on every relay tick, in addition to the existing task-exit handling, so a failing relay never exits the process.

### The payload ceiling is checked where the limit is known

`JetStreamPublisher::send` compares the body length with the connected server's `max_payload` minus 1 KiB of headroom and returns a `PublishFailure` of class `payload_too_large` without publishing. The relay treats that class as permanent: it sets `undeliverable_at`, records the class in `last_error`, increments `attempt_count`, and continues with the next row in the same pass. An undeliverable row is excluded from the due query and from the health decision.

### Health is judged from the rows the relay still holds

CONTRACTS.md R2-03 words the verdict per pass. Read literally, a pass that finds no due row would restore health one second after a failing row was backed off for up to five minutes, so readiness would flap. The relay therefore evaluates the outbox after every pass: it is unhealthy while a publishable unpublished row has `attempt_count >= 3`, or has `attempt_count >= 1` and is older than 300 seconds (`attempt_count >= 1` is the evidence that a publish failed). It is healthy again when no such row remains, because the row published or became undeliverable. A flag change to unhealthy logs the class and increments `x_outbox_relay_unhealthy_total{class}`; `x_outbox_undeliverable_total{class}` counts undeliverable rows.

### The smoke scenario needs its own broker

`services/x/tests/smoke.rs::readiness_follows_the_relay` starts a private `nats-server` with JetStream and nkey authorization whose X stanza omits `evt.social.source.captured.v1`, reloads the stanza with SIGHUP and clears row backoffs by SQL so the scenario does not wait for real backoff. CI copies the binary out of the `nats:2-alpine` container it already runs; a missing binary fails the test.

## Risks

An undeliverable row is never retried, so an operator who raises the broker limit must clear `undeliverable_at` by hand. That is the CONTRACTS.md R2-04 rule.
