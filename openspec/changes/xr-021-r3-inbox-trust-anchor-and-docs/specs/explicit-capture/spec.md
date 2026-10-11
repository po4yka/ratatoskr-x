## MODIFIED Requirements

### Requirement: The public-capture client can trust a configured CA

The service SHALL accept an optional `public_capture.extra_ca_file`, an absolute path to a PEM file with at least one certificate, validated at load. The certificates SHALL be added to the trust anchors of the public-capture client in addition to the platform verifier. Its absence SHALL change nothing.

#### Scenario: a private CA is trusted only when configured

- **WHEN** an HTTPS server presents a certificate issued by a private CA
- **THEN** the lookup fails transiently without the CA file and resolves the post with it

#### Scenario: a bad CA file is refused at load

- **WHEN** the path is relative, missing, or the file holds no PEM certificate
- **THEN** configuration loading fails naming `public_capture.extra_ca_file`

### Requirement: A terminal capture leaves no future retry time

When a capture ends terminally, the service SHALL set its `next_attempt_at` to its `reported_at` in the same update that records the outcome.

#### Scenario: terminal outcomes clear the retry time

- **WHEN** a capture ends deleted, inaccessible or transient-exhausted
- **THEN** its `next_attempt_at` is not after its `reported_at`
