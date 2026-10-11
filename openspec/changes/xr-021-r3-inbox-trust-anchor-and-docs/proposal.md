## Why

Changeset XR-021 round 3 fixes defects that the round 2 native smokes and the independent verifiers found. Every non-Edge NATS identity could subscribe to the shared `_INBOX.>` namespace and read the deliveries of every other service (R3-01). The public-capture client trusts only the host trust store, so the success leg of the capture flow cannot run against a loopback HTTPS fake on a development machine (R3-15). A terminal capture keeps a `next_attempt_at` in the future that reads like a pending retry (R3-15). The operator example names a database no deployment creates and no role file exists (R3-11), and the README says no workspace pin exists (R3-19). This change cites XR-021 CONTRACTS.md sections R3-01, R3-15, R3-11 and R3-19 of the Round 3 amendments.

## What Changes

- **Breaking (named, not staged):** the service opens its NATS connection through one function, `connect_bus`, with the inbox prefix `_INBOX_x`. A service built before this round cannot complete JetStream requests under the new ACL, so reload NATS and deploy the services of one snapshot together. `deploy/nats/identity.conf` ships the X stanza, byte-equal to Platform's, and a test runs the real connection function against a broker enforcing it.
- New optional key `RATATOSKR__PUBLIC_CAPTURE__EXTRA_CA_FILE` (additive): an absolute path to a PEM file whose certificates are added to the trust anchors of the public-capture client, in addition to the platform verifier. Validated at load: absolute, readable, at least one certificate.
- `PublicCaptureWorker::terminate` sets `next_attempt_at = reported_at` so a terminal capture leaves no future retry time.
- **Breaking (example only):** `deploy/systemd/x.conf.example` names the shared database `ratatoskr` instead of `ratatoskr_x`, and `deploy/postgres/role.sql` creates the `ratatoskr_x` role.
- README: the cross-service evidence paragraph is rewritten from the code and the workspace as they are; the new key and the deployment provisioning order are documented.

## Capabilities

### Modified Capabilities

- `service-bootstrap`: the bus connection uses the service's own inbox prefix; the operator example names the shared database.
- `explicit-capture`: the public-capture client can trust a configured CA; a terminal capture is terminal.

## Impact

- Affected code: `services/x` (bus connection, bootstrap error, tests), `x-core` configuration, `x-sync` public post resolver and capture worker, deployment files, README and DEVELOPMENT.
- Out of scope: the dormant bookmark and compliance lane, live X API calls, the browser extension grammar, Instagram and Threads, Platform's ACL.
