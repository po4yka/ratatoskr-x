## MODIFIED Requirements

### Requirement: The service uses its own bus inbox prefix

The service SHALL open its NATS connection through one function that sets the custom inbox prefix `_INBOX_x`, so that JetStream replies reach it under an ACL whose only subscribe permission is `_INBOX_x.>`. `deploy/nats/identity.conf` SHALL carry the X stanza of the deployed ACL.

#### Scenario: JetStream requests complete under the X stanza

- **WHEN** the service connects through its connection function to a broker enforcing only the shipped X stanza
- **THEN** both Edge-provisioned durables verify and a publish of `evt.platform.operation.reported.v1` is acknowledged

### Requirement: The operator example names the shared database

`deploy/systemd/x.conf.example` SHALL point the database URL at the shared database `ratatoskr` on port 5432 with the role `ratatoskr_x`, and `deploy/postgres/role.sql` SHALL create that role with no superuser rights and grant it `connect, create` on `ratatoskr`.

#### Scenario: the example and the role file agree

- **WHEN** the example is loaded through the service's configuration loader
- **THEN** it is accepted, its database is `ratatoskr`, and the role file creates the role it names
