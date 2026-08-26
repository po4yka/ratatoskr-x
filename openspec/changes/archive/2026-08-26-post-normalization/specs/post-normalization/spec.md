## ADDED Requirements

### Requirement: Normalization derives from official API payloads only

Normalization SHALL accept official X API payload envelopes — the data array, includes collections, and their documented member objects — and SHALL reject inputs that do not deserialize into those documented shapes. Intercepted GraphQL traffic, rendered page HTML, and scraped document fragments SHALL NOT be valid normalization inputs. Normalization of an envelope SHALL be a pure function: it performs no network access, no database access, and no media-byte acquisition.

#### Scenario: Single-post envelope normalizes to post and author records

- **WHEN** an envelope carrying one committed fixture post and its author user object is normalized
- **THEN** one normalized post record and one normalized author record come out, carrying the post's text, language, publication timestamp, public metric counts, and conversation linkage, and the author's provider identity and display attributes

#### Scenario: Thread envelope links replies into a conversation

- **WHEN** an envelope carrying a root post and its replies is normalized
- **THEN** each reply yields a `reply` relation naming its parent post by provider id, every thread member carries the same conversation linkage value, and the root post itself carries no reply relation

### Requirement: Determinism

Normalizing the same input envelope twice SHALL produce structurally identical output. Record collections SHALL be emitted in a defined order independent of input map iteration order.

#### Scenario: Repeated normalization is byte-stable

- **WHEN** any committed fixture envelope is normalized twice within one process and across processes
- **THEN** the serialized forms of the resulting record sets are identical

### Requirement: Unknown-field tolerance is record-and-preserve

Unrecognized members of known payload objects SHALL be captured into extension maps attached to the corresponding normalized records rather than rejected or silently dropped. Normalization SHALL refuse an input with a typed error only when a documented required member is absent or unparseable — for example a post without a provider id, or a timestamp outside the documented format. The tolerance policy SHALL be stated in the change design so future contributors extend it deliberately rather than accidentally.

#### Scenario: Unknown field is preserved

- **WHEN** a fixture payload object contains a member that the current DTO layer does not model
- **THEN** normalization succeeds and the member appears, name and value intact, in the extension material of the corresponding normalized record

#### Scenario: Structurally broken input is refused with a typed error

- **WHEN** a fixture payload omits the post provider id or carries an unparseable publication timestamp
- **THEN** normalization fails with a typed error identifying the offending object and member, and emits no partial records for that input

### Requirement: Parser-version stamping on every normalized record

Every normalized record SHALL carry the active parser version as a single crate-level constant stamped at construction. The stamp SHALL survive serialization and deserialization of normalized records unchanged, and the database columns that persist these records SHALL hold it so payloads retained later can be re-parsed by a newer parser version.

#### Scenario: Every emitted record carries the active stamp

- **WHEN** any committed fixture envelope is normalized
- **THEN** every emitted post, author, relation, and media record carries the same parser-version value equal to the active constant

#### Scenario: Round trip preserves the stamp

- **WHEN** normalized records are serialized to JSON and deserialized again
- **THEN** the parser version read back equals the value before serialization

### Requirement: Author resolution failure is a typed refusal

A post whose author provider id cannot be resolved against the user objects carried by the same envelope SHALL fail normalization with a typed error naming the affected post. Normalization SHALL NOT synthesize placeholder authors, SHALL NOT leave author-less records behind, and SHALL emit no records from the failed envelope.

#### Scenario: Deleted-author edge refuses the batch

- **WHEN** a fixture envelope carries a post whose author id has no matching user object
- **THEN** normalization fails with the typed unresolved-author error naming that post's provider id and no records are emitted for the envelope

### Requirement: Relation fidelity with provider identity keys

Post references SHALL map onto the closed relation vocabulary: replied-to references become `reply`, quoted references become `quote`, and retweeted references become `repost`. A relation SHALL be recorded even when the referenced post is not part of the same envelope, keyed by the related post's provider id. A reference whose type falls outside the vocabulary SHALL be preserved through the unknown-field policy and SHALL NOT emit a relation row.

#### Scenario: Quote relation survives a missing quoted post

- **WHEN** a fixture envelope quotes a post that is not included in the envelope
- **THEN** normalization succeeds and the quoting post carries a `quote` relation keyed by the quoted post's provider id

#### Scenario: Retweet reference maps to repost

- **WHEN** a fixture envelope carries a post referencing another post as a retweet
- **THEN** the referencing post carries a `repost` relation keyed by the referenced provider id

### Requirement: Media normalizes to metadata only

Media attachments SHALL normalize to metadata records — provider id, kind among photo, video, and animated gif, dimensions where supplied, alt text where supplied, and duration for video — and SHALL NOT imply media-byte download or storage; media blob references remain unset at this layer. Post records SHALL link their media by provider id.

#### Scenario: Photo attachment yields a metadata record

- **WHEN** a fixture envelope attaches a photo to a post with dimensions and alt text
- **THEN** one normalized media record emerges with kind photo, the supplied metadata, and no blob reference

### Requirement: Long-form and article-backed content keeps honest provenance

When an official payload supplies long-form note text for a post, normalization SHALL place it in the long-text field while keeping the canonical short text unchanged. Edit metadata SHALL be preserved as the provider supplies it; where the provider exposes no exact edit timestamp, normalization SHALL leave the edit timestamp unset rather than inventing one. Article wrappers or other provider objects without fully documented normalization rules SHALL ride the record-and-preserve policy instead of receiving invented structure.

#### Scenario: Long-form note text lands in the long-text field

- **WHEN** a fixture envelope carries a post with long-form note content alongside its short text
- **THEN** the normalized post keeps the short text in its text field, carries the note content in its long-text field, and leaves the edit timestamp unset because the payload supplies none

#### Scenario: Article-backed post normalizes without inventing structure

- **WHEN** the committed article-backed fixture post is normalized
- **THEN** normalization succeeds using only documented payload structure, and any article wrapper member rides the extension preservation path
