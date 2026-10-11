## Context

The authoritative cross-repository text is XR-021 CONTRACTS.md R3-01, R3-15, R3-11 and R3-19. This document records only decisions internal to `ratatoskr-x`.

## Decisions

### One connection function, one prefix literal

`services/x/src/bus.rs::connect_bus(&BusConfig)` reads the seed, applies `custom_inbox_prefix(INBOX_PREFIX)` with `INBOX_PREFIX = "_INBOX_x"` and connects. `main` calls it and nothing else opens a connection. The literal sits in one `const` so the workspace text scan finds it inside a `custom_inbox_prefix(` call. The authorized-broker test starts a `nats-server` whose only users are the shipped stanza (with a real public key substituted for the placeholder) and an administrator standing in for Edge, provisions the two streams and durables, and connects through `connect_bus`. Under the new ACL a client without the prefix cannot receive any reply, so the test times out on the missing prefix.

### The smoke scenario broker enforces the shipped stanza

`readiness_follows_the_relay` hand-wrote an X stanza with `_INBOX.>`. A service that subscribes to `_INBOX_x.>` would be refused there, so the scenario now builds its broker from `deploy/nats/identity.conf` and withholds the captured subject by removing that line, exactly as the other repositories' authorized-broker tests do.

### The extra CA merges, it never replaces

`AppBearerResolver` gains an options struct `ResolverOptions { connect_timeout, request_timeout, extra_ca_pem }`. The extra certificates are added with `reqwest::ClientBuilder::add_root_certificate`, which with the `rustls` feature extends the platform verifier instead of replacing it. The configuration validates the file at load (absolute, readable, at least one PEM certificate); `main` reads it once and hands the bytes to the resolver.

### A terminal capture clears its retry time

`terminate` already stamps `reported_at`. It now also sets `next_attempt_at = reported_at` in the same update, so `next_attempt_at <= reported_at` for every terminal capture.

## Risks

The inbox prefix is a hard cutover with Platform's round 3 ACL: the service and the ACL must be deployed together. Nothing is deployed (the host is frozen).
