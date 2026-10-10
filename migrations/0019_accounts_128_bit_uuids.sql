-- Accounts now issues canonical 128-bit UUIDs. Keep legacy keys readable until the coordinated backfill.
-- Private storage actor UUIDs and encryption AAD remain unchanged.
ALTER TABLE waveform_account_keys DROP CONSTRAINT waveform_account_keys_account_uuid_check;
ALTER TABLE waveform_account_keys ADD CONSTRAINT waveform_account_keys_account_uuid_check CHECK (account_uuid ~ '(^[A-Za-z0-9]{1,64}$|^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$)');
ALTER TABLE waveform_accounts DROP CONSTRAINT waveform_accounts_custodian_uuid_check;
ALTER TABLE waveform_accounts ADD CONSTRAINT waveform_accounts_custodian_uuid_check CHECK (custodian_uuid IS NULL OR custodian_uuid ~ '(^[A-Za-z0-9]{1,64}$|^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$)');
ALTER TABLE waveform_identity_links DROP CONSTRAINT waveform_identity_links_accounts_uuid_check;
ALTER TABLE waveform_identity_links ADD CONSTRAINT waveform_identity_links_accounts_uuid_check CHECK (accounts_uuid IS NULL OR accounts_uuid ~ '(^[A-Za-z0-9]{1,64}$|^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$)');
ALTER TABLE waveform_account_pending_events DROP CONSTRAINT waveform_account_pending_events_account_uuid_check;
ALTER TABLE waveform_account_pending_events ADD CONSTRAINT waveform_account_pending_events_account_uuid_check CHECK (account_uuid ~ '(^[A-Za-z0-9]{1,64}$|^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$)');
