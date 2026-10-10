-- Immutable import ledger for the Accounts old_uuid,new_uuid,kind export.
CREATE TABLE waveform_account_uuid_migrations (
    old_uuid text PRIMARY KEY,
    new_uuid text NOT NULL UNIQUE,
    kind text NOT NULL CHECK (kind IN ('carbon','silicon')),
    migrated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (old_uuid <> new_uuid),
    CHECK (new_uuid ~ '^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$')
);
ALTER TABLE waveform_accounts DROP CONSTRAINT waveform_accounts_account_uuid_fkey;
ALTER TABLE waveform_accounts ADD CONSTRAINT waveform_accounts_account_uuid_fkey
    FOREIGN KEY (account_uuid) REFERENCES waveform_account_keys(account_uuid) ON UPDATE CASCADE;
