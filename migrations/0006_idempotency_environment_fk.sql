-- Retire all scoped idempotency state when a test environment is purged.
-- NOT VALID lets existing installations with orphaned legacy rows migrate;
-- new writes and future deletes still enforce the relationship.
ALTER TABLE waveform_idempotency_records
    ADD CONSTRAINT waveform_idempotency_plane_fk
    FOREIGN KEY (plane_id) REFERENCES waveform_environments(id) ON DELETE CASCADE
    NOT VALID;
