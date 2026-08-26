# folder-snapshot Specification

## Purpose

Defines authoritative native X folder and folder-membership snapshots without conflating them with bookmarks or local organization.

## Requirements

### Requirement: Folder capability limits remain truthful
The service SHALL record a provider-declared lack of folder-listing or folder-membership-read capability for the account and operation that encountered it. It SHALL NOT create a folder, membership, removal observation, or successful authoritative snapshot from that limit.

#### Scenario: Provider cannot expose native folders
- **WHEN** a folder discovery run receives a documented provider capability limit
- **THEN** the run records that limit as its terminal outcome, leaves prior folder authority unchanged, and exposes no fabricated native folder state

### Requirement: Folder membership snapshots have independent atomic authority
The service SHALL stage each folder's observed post memberships separately from bookmark snapshots. Only a complete, successful membership traversal SHALL atomically make its staged set authoritative for that folder; readers SHALL observe either the preceding complete membership set or the complete replacement set, never a mixed set or an incomplete authority pointer.

#### Scenario: A complete membership snapshot replaces the prior set atomically
- **WHEN** a complete membership traversal for a folder stages a set that differs from the previously authoritative membership set
- **THEN** readers before completion observe the prior set and readers after completion observe exactly the staged replacement set

### Requirement: Membership differences are observations
The service SHALL record membership additions and authoritative absences as observations linked to the completing folder-membership snapshot. A complete snapshot's missing membership SHALL create an observed removal without deleting history; an incomplete, failed, truncated, cancelled, or capability-limited run SHALL create no removal observation.

#### Scenario: A complete diff records one addition and one observed removal
- **WHEN** a complete membership snapshot retains one post, adds one post, and omits one previously active post
- **THEN** the folder's current projection contains the retained and added posts, and the membership history records exactly one addition and one removal linked to that completed snapshot

#### Scenario: An incomplete membership scan cannot remove a post
- **WHEN** a folder membership traversal ends before every expected page is received and validated
- **THEN** its prior authoritative membership set and removal-observation history remain unchanged

### Requirement: Folder, bookmark, and local organization remain distinct
The service SHALL reconcile native folder entities and membership independently from bookmark saved-state authority. It SHALL NOT infer native folder membership from bookmark presence, local Ratatoskr collections, or tags, and SHALL document that a post can independently belong to bookmark state, native folders, and local organization.

#### Scenario: A local collection does not create native membership
- **WHEN** a bookmarked post is assigned to a local Ratatoskr collection without an observed native folder membership
- **THEN** the folder projection contains no membership for that post
