-- All product data is explicitly scoped to a plane. Nil identifies production.
CREATE TABLE waveform_environments (
    id uuid PRIMARY KEY,
    org_id text,
    creator_id uuid,
    name text NOT NULL,
    description text,
    root_key_hash bytea UNIQUE,
    root_key_cipher bytea,
    iam_key_cipher bytea,
    briefcase_key_cipher bytea,
    iam_environment_id uuid,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_activity_at timestamptz NOT NULL DEFAULT now(),
    deleted_at timestamptz,
    CHECK (id = '00000000-0000-0000-0000-000000000000' OR
        (org_id IS NOT NULL AND creator_id IS NOT NULL AND root_key_hash IS NOT NULL
         AND root_key_cipher IS NOT NULL AND iam_key_cipher IS NOT NULL
         AND briefcase_key_cipher IS NOT NULL AND iam_environment_id IS NOT NULL))
);
INSERT INTO waveform_environments (id, name)
VALUES ('00000000-0000-0000-0000-000000000000', 'Production');

CREATE TABLE waveform_provider_defaults (
    plane_id uuid PRIMARY KEY REFERENCES waveform_environments(id) ON DELETE CASCADE,
    tts_order jsonb NOT NULL,
    stt_order jsonb NOT NULL
);
INSERT INTO waveform_provider_defaults VALUES (
    '00000000-0000-0000-0000-000000000000',
    '["gemini","elevenlabs","openai"]', '["gemini","openai","deepgram"]'
);

CREATE TABLE waveform_account_preferences (
    plane_id uuid NOT NULL REFERENCES waveform_environments(id) ON DELETE CASCADE,
    org_id text NOT NULL,
    actor_id uuid NOT NULL,
    tts_order jsonb,
    stt_order jsonb,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (plane_id, org_id, actor_id)
);

CREATE TABLE waveform_provider_keys (
    plane_id uuid NOT NULL REFERENCES waveform_environments(id) ON DELETE CASCADE,
    org_id text NOT NULL,
    actor_id uuid NOT NULL,
    provider text NOT NULL CHECK (provider IN ('gemini','elevenlabs','openai','deepgram')),
    secret_cipher bytea NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (plane_id, org_id, actor_id, provider)
);

CREATE TABLE waveform_webhook_events (
    plane_id uuid NOT NULL REFERENCES waveform_environments(id) ON DELETE CASCADE,
    event_id uuid NOT NULL,
    event_type text NOT NULL,
    aggregate_id uuid NOT NULL,
    aggregate_version bigint NOT NULL,
    occurred_at timestamptz NOT NULL,
    received_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (plane_id, event_id)
);

CREATE TABLE waveform_jobs (
    id uuid PRIMARY KEY,
    plane_id uuid NOT NULL REFERENCES waveform_environments(id) ON DELETE CASCADE,
    org_id text NOT NULL,
    actor_id uuid NOT NULL,
    actor_kind text NOT NULL CHECK (actor_kind IN ('carbon','silicon')),
    operation text NOT NULL CHECK (operation IN ('tts','stt')),
    status text NOT NULL CHECK (status IN ('running','failed','completed')),
    first_line text NOT NULL DEFAULT '',
    duration_ms bigint CHECK (duration_ms >= 0),
    provider text,
    request_digest bytea NOT NULL,
    idempotency_key_hash bytea NOT NULL,
    result jsonb,
    error_code text,
    created_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz,
    UNIQUE (plane_id, org_id, actor_id, idempotency_key_hash)
);
CREATE INDEX waveform_jobs_history ON waveform_jobs (plane_id, org_id, actor_id, created_at DESC, id DESC);
CREATE INDEX waveform_environment_retention ON waveform_environments (deleted_at, last_activity_at)
    WHERE id <> '00000000-0000-0000-0000-000000000000';
