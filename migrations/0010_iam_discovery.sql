-- IAM owns sandbox lifecycle; imported secrets select a plane, never a user.
ALTER TABLE waveform_environments ADD COLUMN iam_control_version bigint;
ALTER TABLE waveform_environments ADD COLUMN iam_cleaned_at timestamptz;
ALTER TABLE waveform_environments ADD COLUMN webhook_key_digest text;
ALTER TABLE waveform_account_preferences ADD COLUMN telemetry_enabled boolean NOT NULL DEFAULT true;
CREATE TABLE waveform_bug_reports (
 id uuid PRIMARY KEY, plane_id uuid NOT NULL REFERENCES waveform_environments(id) ON DELETE CASCADE,
 org_id text NOT NULL, actor_id uuid NOT NULL, idempotency_key text NOT NULL,
 payload jsonb NOT NULL, status text NOT NULL CHECK(status IN ('queued','sent','simulated')),
 attempts integer NOT NULL DEFAULT 0, next_attempt_at timestamptz NOT NULL DEFAULT now(),
 created_at timestamptz NOT NULL DEFAULT now(), sent_at timestamptz,
 UNIQUE(plane_id,org_id,actor_id,idempotency_key)
);
