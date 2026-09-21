-- IAM identities are immutable public handles. The UUID remains a private
-- Waveform storage key so history, idempotency and provider-key AAD stay intact.
-- Existing keys are populated by the trusted pre-cutover identity import.
CREATE TABLE waveform_actor_keys (
    plane_id uuid NOT NULL REFERENCES waveform_environments(id) ON DELETE CASCADE,
    public_id text NOT NULL CHECK (length(public_id) BETWEEN 1 AND 255 AND public_id = btrim(public_id)),
    storage_actor_id uuid NOT NULL CHECK (storage_actor_id <> '00000000-0000-0000-0000-000000000000'),
    PRIMARY KEY (plane_id, public_id),
    UNIQUE (plane_id, storage_actor_id)
);
ALTER TABLE waveform_webhook_events ALTER COLUMN aggregate_id TYPE text USING aggregate_id::text;
