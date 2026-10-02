-- Feature consent is independent of ordinary Waveform login. No IAM credentials
-- or authorization codes are stored in plaintext. Environment cleanup removes
-- these rows before an actor mapping or imported testing secret is reused.
CREATE TABLE waveform_storage_authorizations (
 id uuid PRIMARY KEY,
 plane_id uuid NOT NULL REFERENCES waveform_environments(id) ON DELETE CASCADE,
 org_id text NOT NULL,
 actor_id uuid NOT NULL,
 idempotency_key text NOT NULL,
 request_cipher bytea NOT NULL,
 state_digest bytea NOT NULL,
 state_cipher bytea NOT NULL,
 iam_id uuid,
 consent_url text,
 status text NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','completed')),
 code_digest bytea,
 expires_at timestamptz NOT NULL DEFAULT (now()+interval '15 minutes'),
 created_at timestamptz NOT NULL DEFAULT now(),
 UNIQUE(plane_id,org_id,actor_id,idempotency_key)
);
CREATE TABLE waveform_storage_grants (
 plane_id uuid NOT NULL REFERENCES waveform_environments(id) ON DELETE CASCADE,
 org_id text NOT NULL,
 actor_id uuid NOT NULL,
 endpoint_id text NOT NULL,
 token_cipher bytea NOT NULL,
 expires_at timestamptz NOT NULL,
 invalidated_at timestamptz,
 updated_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY(plane_id,org_id,actor_id,endpoint_id)
);
