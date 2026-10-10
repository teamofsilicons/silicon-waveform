-- Keeping Waveform's account cache true to Silicon Accounts. Additive only: nothing is dropped or rewritten.

-- When Waveform last asked Silicon Accounts about the account (GET /v1/accounts/{uuid}). A background check asks
-- about every account at least once a day: an account that used Waveform only through another app's proof, or that
-- removed Waveform's access, gets no account.deleted webhook, so this is how its deletion reaches Waveform.
ALTER TABLE waveform_accounts ADD COLUMN looked_up_at timestamptz;
CREATE INDEX waveform_accounts_lookup_due_idx ON waveform_accounts (looked_up_at NULLS FIRST)
    WHERE deleted_at IS NULL;

COMMENT ON COLUMN waveform_accounts.id_changed_at IS
    'The instant as of which current_id is known to be right: the occurred_at of account.id_changed, the iat of a token that carried the id, or the start of a lookup. Older evidence never replaces newer.';

-- Sign-out and deletion events about an account Waveform has not seen yet (it signed in to Waveform but had not
-- called it). Reads of the account honour them, so a token issued before the event is still refused. Rows are folded
-- into waveform_accounts when the account is first seen, and pruned once every token they could concern has expired.
CREATE TABLE waveform_account_pending_events (
    account_uuid text PRIMARY KEY CHECK (account_uuid ~ '^[A-Za-z0-9]{1,64}$'),
    revoked_before timestamptz NOT NULL,
    revoked_reason text NOT NULL,
    deleted_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT now()
);
