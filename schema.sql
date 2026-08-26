-- The owned first-version schema of ratatoskr-x, edited in place while the development status
-- forbids migrations. Columns are honest placeholders for plan item 1: identities, observation
-- timestamp names, and state vocabularies follow AGENTS.md; column detail grows with the
-- changes that need it. Everything lives inside x_archive and nothing references outside it.

CREATE SCHEMA IF NOT EXISTS x_archive;

CREATE TABLE IF NOT EXISTS x_archive.accounts (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    provider_user_id text NOT NULL UNIQUE,
    state            text NOT NULL DEFAULT 'connected'
        CHECK (state IN ('connected', 'refresh_required', 'reauth_required',
                         'revoked', 'suspended', 'paused')),
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS x_archive.users (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    provider_id  text NOT NULL UNIQUE,
    username     text,
    display_name text,
    parser_version integer NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS x_archive.credentials (
    id                uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    account_id        uuid NOT NULL REFERENCES x_archive.accounts (id),
    encrypted_payload bytea NOT NULL,
    granted_scopes    text[] NOT NULL,
    status            text NOT NULL CHECK (status IN ('active', 'expired', 'revoked')),
    expires_at        timestamptz,
    superseded_refresh_hash text,
    created_at        timestamptz NOT NULL DEFAULT now()
);

-- One-time PKCE authorization intents. The lookup key is the SHA-256 digest of
-- the OAuth state, so a database leak yields nothing directly usable; the code
-- verifier rests encrypted beside it until the intent is consumed or expires.
CREATE TABLE IF NOT EXISTS x_archive.oauth_intents (
    id                     uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    internal_user_id       uuid NOT NULL,
    state_hash             text NOT NULL UNIQUE,
    code_verifier_encrypted bytea NOT NULL,
    nonce                  text NOT NULL,
    redirect_uri           text NOT NULL,
    requested_scopes       text[] NOT NULL,
    created_at             timestamptz NOT NULL,
    expires_at             timestamptz NOT NULL,
    consumed_at            timestamptz
);

-- Fixed request-budget windows per account. Historical rows are immutable once
-- a later window exists; the gate charges usage before any provider call.
CREATE TABLE IF NOT EXISTS x_archive.api_budget_windows (
    account_id     uuid NOT NULL REFERENCES x_archive.accounts (id),
    window_start   timestamptz NOT NULL,
    window_seconds integer NOT NULL CHECK (window_seconds > 0),
    request_cap    integer NOT NULL CHECK (request_cap > 0),
    used_requests  integer NOT NULL DEFAULT 0 CHECK (used_requests >= 0),
    PRIMARY KEY (account_id, window_start)
);

CREATE TABLE IF NOT EXISTS x_archive.posts (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    provider_id    text NOT NULL UNIQUE,
    author_user_id uuid NOT NULL REFERENCES x_archive.users (id),
    text           text NOT NULL DEFAULT '',
    long_text      text,
    language       text,
    published_at   timestamptz,
    edited_at      timestamptz,
    -- Linkage evidence, not a foreign key: thread reconstruction joins on it
    -- without trusting referential integrity.
    conversation_provider_id text,
    -- Public metric counts as last observed; NULL means the provider never
    -- stated a value. Absence is not zero.
    like_count       bigint,
    retweet_count    bigint,
    reply_count      bigint,
    quote_count      bigint,
    bookmark_count   bigint,
    impression_count bigint,
    parser_version   integer NOT NULL,
    availability   text NOT NULL DEFAULT 'active'
        CHECK (availability IN ('active', 'deleted', 'protected', 'author_suspended',
                                'unavailable', 'unknown')),
    raw_blob_ref   text,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS x_archive.post_relations (
    id                     uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    post_id                uuid NOT NULL REFERENCES x_archive.posts (id) ON DELETE CASCADE,
    related_post_provider_id text NOT NULL,
    relation               text NOT NULL CHECK (relation IN ('reply', 'quote', 'repost')),
    -- Unconstrained by design: the referenced post may be absent from the same
    -- batch, so this column keys provider identity, not local rows.
    parser_version         integer NOT NULL,
    UNIQUE (post_id, related_post_provider_id, relation)
);

CREATE TABLE IF NOT EXISTS x_archive.media (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    post_id     uuid NOT NULL REFERENCES x_archive.posts (id) ON DELETE CASCADE,
    provider_id text NOT NULL,
    kind        text CHECK (kind IN ('photo', 'video', 'animated_gif')),
    metadata    jsonb NOT NULL DEFAULT '{}',
    -- Metadata references only; media bytes are never downloaded here.
    blob_ref    text,
    parser_version integer NOT NULL,
    UNIQUE (post_id, provider_id)
);

CREATE TABLE IF NOT EXISTS x_archive.bookmarks (
    id                    uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    account_id            uuid NOT NULL REFERENCES x_archive.accounts (id),
    post_id               uuid NOT NULL REFERENCES x_archive.posts (id),
    -- Observation names are deliberate: X exposes no authoritative save or removal time.
    first_observed_saved_at timestamptz NOT NULL DEFAULT now(),
    last_observed_saved_at  timestamptz NOT NULL DEFAULT now(),
    observed_removed_at     timestamptz,
    UNIQUE (account_id, post_id)
);

CREATE TABLE IF NOT EXISTS x_archive.bookmark_folders (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    account_id  uuid NOT NULL REFERENCES x_archive.accounts (id),
    provider_id text NOT NULL,
    name        text,
    UNIQUE (account_id, provider_id)
);

CREATE TABLE IF NOT EXISTS x_archive.bookmark_folder_items (
    folder_id uuid NOT NULL REFERENCES x_archive.bookmark_folders (id) ON DELETE CASCADE,
    post_id   uuid NOT NULL REFERENCES x_archive.posts (id),
    first_observed_in_folder_at timestamptz NOT NULL DEFAULT now(),
    observed_removed_from_folder_at timestamptz,
    PRIMARY KEY (folder_id, post_id)
);

CREATE TABLE IF NOT EXISTS x_archive.sync_runs (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    account_id uuid REFERENCES x_archive.accounts (id),
    run_type   text NOT NULL
        CHECK (run_type IN ('incremental', 'full', 'folder_listing', 'folder_membership',
                            'compliance_revalidation')),
    state      text NOT NULL CHECK (state IN ('running', 'completed', 'failed', 'cancelled')),
    started_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz
);

CREATE TABLE IF NOT EXISTS x_archive.snapshots (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    sync_run_id  uuid NOT NULL REFERENCES x_archive.sync_runs (id),
    complete     boolean NOT NULL DEFAULT false,
    completed_at timestamptz,
    page_count   integer NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS x_archive.rate_limit_state (
    account_id uuid PRIMARY KEY REFERENCES x_archive.accounts (id),
    remaining  integer,
    reset_at   timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS x_archive.tombstones (
    id                   uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    post_provider_id     text NOT NULL,
    reason               text NOT NULL
        CHECK (reason IN ('deleted', 'protected', 'author_suspended', 'unavailable', 'unknown')),
    evidence_snapshot_id uuid REFERENCES x_archive.snapshots (id),
    recorded_at          timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS x_archive.outbox_events (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    aggregate      text NOT NULL,
    event_type     text NOT NULL,
    payload        jsonb NOT NULL,
    correlation_id text,
    causation_id   text,
    created_at     timestamptz NOT NULL DEFAULT now(),
    published_at   timestamptz
);

CREATE TABLE IF NOT EXISTS x_archive.inbox_events (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    source      text NOT NULL,
    event_type  text NOT NULL,
    event_id    text NOT NULL UNIQUE,
    payload     jsonb NOT NULL,
    consumed_at timestamptz
);
