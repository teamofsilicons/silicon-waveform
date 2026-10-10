-- Contract 2.0.0 of API v1: Silicon Accounts authentication (Bearer JWT or Proof), no organization header, the
-- Accounts webhook and Briefcase storage reporting. The path stays /api/v1; only the contract version changes.
UPDATE waveform_api_contracts SET contract_version = '2.0.0' WHERE version = 'v1' AND contract_version = '1.0.0';
