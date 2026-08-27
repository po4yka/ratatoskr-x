# compliance-revalidation

## Purpose

Defines bounded periodic checks of preserved X content and the durable, replay-safe takedown path
that removes downstream Knowledge derivatives when upstream authorization no longer permits use.

## Requirements

### Requirement: Revalidation is bounded, account-scoped, and due-driven

Each revalidation invocation SHALL inspect no more than its explicit item bound, SHALL select only
unremoved sources belonging to the requested account whose last check is older than the supplied
due instant, and SHALL consume the existing provider-request budget before each upstream call.

#### Scenario: recent and excess sources are not checked

- **WHEN** an account has recently checked sources and more due sources than the invocation bound
- **THEN** no recent source is called and the number of provider calls does not exceed the bound

### Requirement: Every attempted provider check has durable ledger evidence

The service SHALL append one account- and source-scoped ledger entry for every provider check that
returns an observation or an indeterminate failure. An entry SHALL record the check instant,
provider request identifier when supplied, observed availability or bounded failure class, and no
credential, bearer header, post body, username, or URL.

#### Scenario: available observation is recorded

- **WHEN** the provider confirms that a due preserved post remains available
- **THEN** one ledger entry records the available outcome and request evidence for that account and
  source

#### Scenario: indeterminate failure has no takedown authority

- **WHEN** a due check is rate-limited, loses scope, fails transiently, or returns invalid evidence
- **THEN** one indeterminate ledger entry is recorded and the source, tombstone, and deletion
  outbox state remain unchanged

### Requirement: Authoritative unavailability triggers one atomic takedown

An authoritative deleted, protected, suspended-author, or unavailable observation SHALL atomically
record its ledger entry, update the provider availability state, mark the account source removed,
write a source-scoped tombstone, and enqueue one `social.source.removed.v1` fact with
`reason = "retention_policy"`. The provider-specific reason SHALL remain in X-owned evidence; the
shared removal fact SHALL remain a library-removal fact and SHALL not claim provider deletion.

#### Scenario: provider deletion propagates to Knowledge

- **WHEN** an authoritative revalidation reports that a preserved source was deleted upstream
- **THEN** the transaction commits one ledger entry, one tombstone, and one social-source removal
  request that makes Knowledge delete or tombstone its analysis, embedding, and search projection

#### Scenario: repeated takedown remains idempotent

- **WHEN** the same source is revalidated as unavailable after its first committed takedown
- **THEN** the audit ledger may retain the additional check but no second tombstone or removal
  request is created

### Requirement: Removed sources cannot be silently re-analysed

Once compliance revalidation marks a source removed, later ordinary bookmark or capture
observations SHALL not emit a captured or updated fact for it. A later Knowledge completion for
the removed source SHALL not create a new active link.

#### Scenario: delayed observations do not resurrect a takedown

- **WHEN** an ordinary source observation or delayed completion arrives after an authoritative
  takedown
- **THEN** no new analysis request or completion link makes the source active again
