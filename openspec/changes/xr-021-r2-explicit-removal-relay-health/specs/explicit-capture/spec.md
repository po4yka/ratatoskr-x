## MODIFIED Requirements

### Requirement: A provider deletion removes the owner's source and tells Knowledge

When the public resolution reports a post deleted for a capture whose owner already holds an `explicit_sources` row for that post, the service SHALL delete that row and queue exactly one `social.source.removed.v1` envelope (producer `ratatoskr-x`, aggregate `social_source:<id>`, tenant `user:<owner>`, reason `retention_policy`) in the same transaction as the terminal `failed social.source.deleted` report. Inaccessible and transient outcomes SHALL remove nothing. A deletion found by the first capture of a post SHALL queue no removal. A later capture of a restored post SHALL preserve it as a new source.

#### Scenario: a re-capture of a deleted post removes the owner source

- **WHEN** an owner preserved a post and a second capture of the same post is resolved as deleted
- **THEN** one removal envelope for that source is outbox-bound, the owner's source row is gone, the capture is reported `failed social.source.deleted` once, and another tenant's source for the same post is untouched

#### Scenario: a first capture of a deleted post removes nothing

- **WHEN** the owner has no source for the post and the capture is resolved as deleted
- **THEN** no removal envelope is queued and the capture is reported deleted once

#### Scenario: inaccessible and transient outcomes remove nothing

- **WHEN** a re-capture is resolved as inaccessible, or fails transiently until its attempts end
- **THEN** the owner's source row stays and no removal envelope is queued

#### Scenario: a restored post is preserved as a new source

- **WHEN** a post removed by a deletion is captured again and the resolution returns it
- **THEN** a new source identity is preserved and one captured envelope is queued for it
