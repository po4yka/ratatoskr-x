## Purpose

Defines conservative handoff of external links from preserved X posts to Extractor while retaining
one shared capture outcome and provenance for every post that referenced the article.

## ADDED Requirements

### Requirement: Only eligible external links are captured
The service SHALL consider only normalized HTTP(S) links from a post's supported expanded-link
metadata. It SHALL exclude malformed, non-web, and X-owned permalink links, and it SHALL retain
the original post independently of whether any linked article can be captured.

#### Scenario: an expanded external article link is selected
- **WHEN** a captured X post contains an expanded HTTPS link to a non-X host
- **THEN** the service records that external link as eligible for article capture

#### Scenario: an X permalink is not delegated
- **WHEN** a captured X post contains an X-owned permalink or a non-HTTP(S) link
- **THEN** the service emits no extractor capture command for that link

### Requirement: One account URL capture serves all referencing posts
The service SHALL deduplicate an eligible link by account owner and canonical normalized URL. It
SHALL create one article-capture operation and command for that key, while retaining a link from
every originating post to the shared capture.

#### Scenario: two posts share one external article
- **WHEN** two posts belonging to the same account contain equivalent eligible external links
- **THEN** one article capture command is durable and both posts link to that capture

### Requirement: Extractor outcomes attach to every linked post idempotently
The capture command SHALL carry a correlation reference identifying the account article capture.
The service SHALL accept only a correlated, owner-matching terminal extractor outcome, retain its
Document identity and Document IR BlobRef when succeeded, and attach that result to every linked
post. A redelivery, foreign owner, malformed BlobRef, or unknown correlation SHALL not change
linked-post state.

#### Scenario: a successful extractor report links the document to every origin
- **WHEN** Extractor returns a successful correlated outcome containing a Document IR BlobRef
- **THEN** every post linked to the capture records the same document identity and BlobRef exactly
once

#### Scenario: an unrelated extractor outcome is ignored safely
- **WHEN** an extractor outcome has a different owner or correlation reference
- **THEN** the archive attaches no document result and leaves its inbox state unapplied

### Requirement: Capture handoff commits before publication and remains at-least-once safe
The article-capture record, all originating post links, and its capture-command outbox row SHALL
commit atomically. The command dispatcher SHALL publish only committed rows and retries SHALL reuse
the same capture operation and idempotency key.

#### Scenario: persistence cannot enqueue the capture command
- **WHEN** the capture command outbox write fails
- **THEN** neither the article capture nor any originating post link is committed
