-- Isolate idempotency records between production and Waveform test planes.
ALTER TABLE waveform_idempotency_records
    ADD COLUMN IF NOT EXISTS plane_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000';
ALTER TABLE waveform_idempotency_records
    DROP CONSTRAINT IF EXISTS waveform_idempotency_records_pkey;
ALTER TABLE waveform_idempotency_records
    ADD PRIMARY KEY (plane_id, actor_type, actor_id, org_id, operation, idempotency_key);
