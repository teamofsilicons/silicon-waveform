-- Silicon Accounts identity, added alongside the IAM-era tables. Nothing is dropped or rewritten.
--
-- Accounts uuids are short, case-sensitive base62 strings (e.g. 'zQo'), not RFC 4122 UUIDs, so they live in text
-- columns and are compared exactly. Every existing table keeps its private storage_actor_id uuid; this mapping is
-- the only place an account uuid meets that key, so history, idempotency records and the associated data of
-- encrypted provider keys stay valid.
CREATE TABLE waveform_account_keys (
    account_uuid text PRIMARY KEY CHECK (account_uuid ~ '^[A-Za-z0-9]{1,64}$'),
    storage_actor_id uuid NOT NULL UNIQUE
        CHECK (storage_actor_id <> '00000000-0000-0000-0000-000000000000'),
    kind text NOT NULL CHECK (kind IN ('carbon', 'silicon')),
    source text NOT NULL CHECK (source IN ('sign_in', 'proof', 'lookup', 'link_identities')),
    created_at timestamptz NOT NULL DEFAULT now()
);

-- Display cache and revocation state per account, fed by token claims, proof verifications, account lookups and
-- the Accounts webhook. A Silicon always has exactly one custodian (a Carbon).
CREATE TABLE waveform_accounts (
    account_uuid text PRIMARY KEY REFERENCES waveform_account_keys(account_uuid),
    kind text NOT NULL CHECK (kind IN ('carbon', 'silicon')),
    current_id text CHECK (current_id IS NULL OR length(current_id) BETWEEN 3 AND 128),
    display_name text CHECK (display_name IS NULL OR length(display_name) <= 256),
    pfp_url text CHECK (pfp_url IS NULL OR length(pfp_url) <= 2048),
    custodian_uuid text CHECK (custodian_uuid IS NULL OR custodian_uuid ~ '^[A-Za-z0-9]{1,64}$'),
    custodian_id text,
    profile_version bigint NOT NULL DEFAULT 0,
    id_changed_at timestamptz,
    custodian_changed_at timestamptz,
    custodian_checked_at timestamptz,
    revoked_before timestamptz,
    revoked_reason text,
    deleted_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (kind = 'silicon' OR custodian_uuid IS NULL)
);
CREATE INDEX waveform_accounts_custodian_idx ON waveform_accounts (custodian_uuid)
    WHERE custodian_uuid IS NOT NULL;

-- The IAM-era principals and the private keys they owned, kept for the cutover. `waveform-api link-identities`
-- fills accounts_uuid from the operator's mapping file and creates the matching waveform_account_keys rows.
CREATE TABLE waveform_identity_links (
    iam_principal_id text PRIMARY KEY CHECK (length(iam_principal_id) BETWEEN 1 AND 255),
    iam_public_id text,
    storage_actor_id uuid NOT NULL,
    accounts_uuid text CHECK (accounts_uuid IS NULL OR accounts_uuid ~ '^[A-Za-z0-9]{1,64}$'),
    linked_at timestamptz,
    source text NOT NULL CHECK (source IN ('iam_actor_keys', 'mapping_file')),
    CHECK ((accounts_uuid IS NULL) = (linked_at IS NULL))
);
CREATE INDEX waveform_identity_links_storage_idx ON waveform_identity_links (storage_actor_id);
INSERT INTO waveform_identity_links (iam_principal_id, iam_public_id, storage_actor_id, source)
SELECT public_id, public_id, storage_actor_id, 'iam_actor_keys'
FROM waveform_actor_keys
WHERE plane_id = '00000000-0000-0000-0000-000000000000';

-- Accounts webhook deliveries, deduplicated on event_id. Only content-free metadata is kept.
CREATE TABLE waveform_accounts_webhook_events (
    event_id text PRIMARY KEY CHECK (length(event_id) BETWEEN 1 AND 128),
    event_type text NOT NULL CHECK (length(event_type) BETWEEN 1 AND 128),
    account_uuid text,
    occurred_at timestamptz,
    received_at timestamptz NOT NULL DEFAULT now(),
    outcome text NOT NULL
);

-- User verification proofs Waveform holds for an account at another app (Briefcase). Both the proof token (sap_)
-- and its rotating refresh token (sapr_) are encrypted with WAVEFORM_ENCRYPTION_KEY, bound to
-- 'storage-proof/{storage_actor_id}/{receiving_app}/{token|refresh}'.
CREATE TABLE waveform_storage_proofs (
    storage_actor_id uuid NOT NULL,
    receiving_app text NOT NULL CHECK (receiving_app ~ '^[a-z][a-z0-9_-]{0,79}$'),
    proof_id text NOT NULL CHECK (length(proof_id) BETWEEN 1 AND 128),
    scopes text[] NOT NULL,
    token_cipher bytea NOT NULL,
    token_expires_at timestamptz NOT NULL,
    refresh_cipher bytea,
    refresh_expires_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    invalidated_at timestamptz,
    invalidated_reason text,
    PRIMARY KEY (storage_actor_id, receiving_app)
);

-- Generated speech Waveform keeps itself when it could not (or must not) store it in Briefcase. Held for the
-- idempotency retention and served only to the requesting account.
CREATE TABLE waveform_held_audio (
    request_id uuid PRIMARY KEY,
    storage_actor_id uuid NOT NULL,
    filename text NOT NULL CHECK (length(filename) BETWEEN 1 AND 255),
    media_type text NOT NULL DEFAULT 'audio/mpeg',
    audio bytea NOT NULL CHECK (octet_length(audio) BETWEEN 1 AND 26214400),
    reason text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    CHECK (expires_at > created_at)
);
CREATE INDEX waveform_held_audio_expiry_idx ON waveform_held_audio (expires_at);
CREATE INDEX waveform_held_audio_owner_idx ON waveform_held_audio (storage_actor_id);

-- The app that called Waveform for an account with a proof ("created by Interface for c:ada").
ALTER TABLE waveform_jobs ADD COLUMN issuing_app text
    CHECK (issuing_app IS NULL OR issuing_app ~ '^[a-z][a-z0-9_-]{0,79}$');

-- Account history no longer filters by org_id. New rows write the org sentinel '@', which no IAM handle can be.
CREATE INDEX waveform_jobs_account_history ON waveform_jobs (plane_id, actor_id, created_at DESC, id DESC);
