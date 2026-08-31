CREATE TYPE waveform_operation AS ENUM ('tts', 'stt');

CREATE TYPE waveform_actor_type AS ENUM ('carbon', 'silicon');

CREATE TYPE waveform_idempotency_state AS ENUM ('pending', 'completed');

CREATE TABLE waveform_idempotency_records (
    actor_type waveform_actor_type NOT NULL,
    actor_id UUID NOT NULL,
    org_id TEXT NOT NULL CHECK (length(org_id) BETWEEN 1 AND 64),
    operation waveform_operation NOT NULL,
    idempotency_key TEXT NOT NULL CHECK (length(idempotency_key) BETWEEN 8 AND 255),
    request_digest BYTEA NOT NULL CHECK (octet_length(request_digest) = 32),
    request_id UUID NOT NULL,
    state waveform_idempotency_state NOT NULL DEFAULT 'pending',
    lease_token UUID,
    lease_expires_at TIMESTAMPTZ,
    response_body JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (actor_type, actor_id, org_id, operation, idempotency_key),
    CONSTRAINT waveform_pending_lease_check CHECK (
        (state = 'pending' AND lease_token IS NOT NULL AND lease_expires_at IS NOT NULL AND response_body IS NULL)
        OR
        (state = 'completed' AND lease_token IS NULL AND lease_expires_at IS NULL AND response_body IS NOT NULL)
    ),
    CONSTRAINT waveform_tts_response_has_no_temporary_url CHECK (
        operation <> 'tts'
        OR response_body IS NULL
        OR NOT (response_body ? 'temporary_url')
    ),
    CONSTRAINT waveform_expiry_check CHECK (expires_at > created_at)
);

CREATE INDEX waveform_idempotency_expiry_idx
    ON waveform_idempotency_records (expires_at);

CREATE INDEX waveform_idempotency_request_idx
    ON waveform_idempotency_records (request_id);

COMMENT ON TABLE waveform_idempotency_records IS
    'Durable deduplication with a short-lived normalized success cache; request inputs, media, audio, credentials, and provider bodies are never stored.';
