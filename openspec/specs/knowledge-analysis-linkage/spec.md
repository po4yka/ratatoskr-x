# knowledge-analysis-linkage

## Purpose

Defines how X source revisions request Knowledge analysis and retain privacy-safe completion
linkage without taking ownership of analyses, embeddings, or search documents.

## Requirements

### Requirement: Source revisions are deduplicated Knowledge requests

Each `social.source.captured.v1` or `social.source.updated.v1` fact SHALL be the complete Knowledge
analysis request defined by the workspace `social-analysis-intake` specification. The X archive
SHALL durably emit at most one request for each `(social_source_id, content_digest)` even when the
same normalized provider observation is processed concurrently or retried.

#### Scenario: concurrent identical observations emit one request

- **WHEN** two transactions concurrently preserve the same account source revision
- **THEN** both operations converge successfully on one source revision and one social-source
  request for its content digest

### Requirement: Completion facts link to exact retained revisions

The X archive SHALL accept only typed `knowledge.analysis.completed.v1` facts from Knowledge whose
owner, social-source identity, and content digest match a retained local source revision. It SHALL
store only those contract linkage fields and the completion instant, with no model output,
Knowledge-private run identity, embedding identity, or search-document identity.

#### Scenario: current completion links without a Knowledge identifier

- **WHEN** Knowledge completes analysis for the source's current content digest
- **THEN** one durable link exists for that source revision and the link contains no
  Knowledge-private identifier

#### Scenario: historical completion remains distinguishable

- **WHEN** a completion arrives for a retained older digest after the source has changed
- **THEN** the older completion remains linked to its own revision and is not presented as the
  current source's completion

#### Scenario: foreign or unknown completion is rejected

- **WHEN** a completion names a different owner, an unknown source, or a digest not retained for
  that source
- **THEN** no completion link and no applied inbox receipt is committed

### Requirement: Completion consumption is replay-safe

The completion event and its source-revision link SHALL commit atomically. Redelivery of the same
event, or a second completion fact for an already-linked source digest, SHALL not create another
link.

#### Scenario: completion redelivery creates one link

- **WHEN** the same valid Knowledge completion is consumed more than once
- **THEN** exactly one completion link exists for its `(social_source_id, content_digest)`

### Requirement: Search projection ownership remains in Knowledge

X SHALL expose current-versus-historical completion state solely by comparing the linked digest
with its current source revision. Knowledge SHALL remain the owner of search projection inputs,
search documents, embeddings, and deletion execution under the workspace contract.

#### Scenario: source update changes current linkage without rewriting history

- **WHEN** a source with a linked completion publishes a new content digest
- **THEN** the earlier link remains historical and no Knowledge search-document identifier is
  copied into X-owned storage
