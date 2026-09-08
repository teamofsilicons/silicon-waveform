-- Job history follows the canonical idempotent operation and its current attempt.
ALTER TABLE waveform_jobs ADD COLUMN lease_token uuid;
ALTER TABLE waveform_jobs ADD COLUMN lease_expires_at timestamptz;
CREATE INDEX waveform_jobs_expired_attempts ON waveform_jobs (lease_expires_at)
    WHERE status = 'running';
