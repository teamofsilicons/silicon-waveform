-- Keep durable history states internally consistent for new and updated rows.
-- NOT VALID preserves compatibility with installations that predate lifecycle
-- finalization; all future inserts and updates must satisfy these checks.
ALTER TABLE waveform_jobs
    ADD CONSTRAINT waveform_jobs_finished_at_state_check
    CHECK ((status = 'running' AND finished_at IS NULL)
        OR (status IN ('failed', 'completed') AND finished_at IS NOT NULL))
    NOT VALID;

ALTER TABLE waveform_jobs
    ADD CONSTRAINT waveform_jobs_error_code_state_check
    CHECK ((status = 'failed' AND error_code IS NOT NULL)
        OR (status IN ('running', 'completed') AND error_code IS NULL))
    NOT VALID;
