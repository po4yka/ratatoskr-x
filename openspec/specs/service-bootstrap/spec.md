# service-bootstrap

## Purpose

Defines how the ratatoskr-x service process starts, accepts or rejects its configuration, emits structured telemetry, exposes process-state endpoints, and reports failure classes, so operators and tests can rely on stable observable behavior from the first runnable version onward.

## Requirements

### Requirement: Typed finite configuration

The service SHALL load its entire runtime configuration from `RATATOSKR__`-prefixed environment variables into a typed, closed set of settings, SHALL reject any environment key it does not declare, and SHALL reject semantically invalid values while reporting every violation together.

#### Scenario: Declared variables produce a usable configuration

- **WHEN** the process starts with only declared `RATATOSKR__` variables set, or none at all
- **THEN** configuration loading succeeds and every setting is available with a defined default where the variable is absent

#### Scenario: Unknown variable is refused

- **WHEN** an environment variable prefixed `RATATOSKR__` names a key that is not part of the declared configuration
- **THEN** configuration loading fails and the failure identifies the offending key without printing any secret values

#### Scenario: Invalid value reports all violations

- **WHEN** two or more declared variables hold values that violate the configuration rules
- **THEN** configuration loading fails once and reports both violations in a single operator-readable summary

### Requirement: Configuration rejection exit code

The service SHALL terminate with exit code 78 when configuration is rejected, before binding any network listener.

#### Scenario: Rejected configuration terminates with the documented status

- **WHEN** the process is started with a configuration that fails validation
- **THEN** the process exits with code 78, serves no HTTP endpoint, and logs the rejection reason

### Requirement: Structured telemetry

The service SHALL emit structured log events to standard output in either machine-readable JSON or human-readable pretty form as configured, SHALL honor a configurable filter expression for event verbosity, and SHALL fail startup with a typed error when the filter expression cannot be parsed.

#### Scenario: JSON events on stdout

- **WHEN** the log format is set to JSON and the service logs an event
- **THEN** the event appears on standard output as a single JSON object containing the message and level

#### Scenario: Unparsable filter fails fast

- **WHEN** the configured filter expression is not a valid directive list
- **THEN** startup fails with a typed telemetry error and the process does not serve traffic

### Requirement: Process-state endpoints

The service SHALL expose `/health/live`, `/health/ready`, `/metrics`, and `/version` over HTTP. Liveness SHALL return success whenever the process is running. Readiness SHALL return failure with named check results until initialization completes and success afterwards. Metrics SHALL be served in Prometheus text exposition format. Version SHALL identify the service name, build version, source revision, and toolchain version. All four responses SHALL be served with `Cache-Control: no-store`.

#### Scenario: Liveness while running

- **WHEN** a GET request hits `/health/live` on a running process
- **THEN** the response is 200 with a body whose state field reports liveness

#### Scenario: Readiness tracks initialization

- **WHEN** a GET request hits `/health/ready` before readiness is granted and again after it is granted
- **THEN** the first response is 503 listing unmet checks by name and the second response is 200 reporting readiness

#### Scenario: Metrics exposition

- **WHEN** a GET request hits `/metrics`
- **THEN** the response body is Prometheus text exposition with the matching content type

#### Scenario: Version identification

- **WHEN** a GET request hits `/health/live`'s sibling `/version`
- **THEN** the response reports the service name `ratatoskr-x`, the crate version, the source revision or `unknown`, and the compiler version

#### Scenario: No caching of state endpoints

- **WHEN** any process-state endpoint responds
- **THEN** the response carries `Cache-Control: no-store`

### Requirement: Typed failure classes

Startup and runtime failures SHALL be represented by typed error values naming their subsystem, SHALL NOT be constructed from free-form strings at failure sites, and SHALL map to documented process exit codes.

#### Scenario: Each failing subsystem yields its own error variant

- **WHEN** configuration, telemetry, or database initialization fails during bootstrap
- **THEN** the resulting error value exposes which subsystem failed through its type, and each such error type maps configuration rejection to exit code 78 and other failures to nonzero exit codes distinct from 78
