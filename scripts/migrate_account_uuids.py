#!/usr/bin/env python3
"""Preview/apply the Accounts old_uuid,new_uuid,kind export to an offline Waveform database.

Apply normal SQL migrations first. Stop API/workers for the coordinated cutover.
Requires psql and WAVEFORM_DATABASE_URL (or PGDATABASE). Defaults to a rolled-back dry run.
"""
import argparse
import csv
import io
import os
from pathlib import Path
import re
import shutil
import subprocess
import uuid


def parse_mapping(text):
    reader = csv.reader(io.StringIO(text))
    if next(reader, None) != ["old_uuid", "new_uuid", "kind"]:
        raise ValueError("Expected exact header old_uuid,new_uuid,kind")
    rows, old_ids, new_ids = [], set(), set()
    for line, fields in enumerate(reader, 2):
        if not fields:
            continue
        if len(fields) != 3:
            raise ValueError(f"Line {line}: expected three fields")
        old, new, kind = fields
        try:
            parsed = uuid.UUID(new)
        except ValueError as error:
            raise ValueError(f"Line {line}: new_uuid is not a UUID") from error
        if str(parsed) != new or parsed.version != 4:
            raise ValueError(f"Line {line}: new_uuid must be canonical lowercase UUIDv4")
        if not re.fullmatch(r"[A-Za-z0-9]{1,64}|[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", old):
            raise ValueError(f"Line {line}: invalid old_uuid")
        if kind not in ("carbon", "silicon") or old == new:
            raise ValueError(f"Line {line}: invalid kind or unchanged UUID")
        if old in old_ids or new in new_ids:
            raise ValueError(f"Line {line}: duplicate source or target; account merging is forbidden")
        old_ids.add(old)
        new_ids.add(new)
        rows.append((old, new, kind))
    if not rows or old_ids & new_ids:
        raise ValueError("Mapping must be nonempty and cannot contain cycles or chained remaps")
    return rows


def migration_sql(rows, apply=False):
    # Every interpolated field passed the allowlists above; no arbitrary SQL identifiers or text.
    values = ",\n".join("('%s','%s','%s')" % row for row in rows)
    end = "COMMIT" if apply else "ROLLBACK"
    return f"""
BEGIN;
SET LOCAL lock_timeout='15s';
LOCK TABLE waveform_account_keys, waveform_accounts, waveform_identity_links,
 waveform_account_pending_events, waveform_accounts_webhook_events,
 waveform_storage_proofs, waveform_account_uuid_migrations IN ACCESS EXCLUSIVE MODE;
CREATE TEMP TABLE uuid_import(old_uuid text PRIMARY KEY,new_uuid text UNIQUE,kind text) ON COMMIT DROP;
INSERT INTO uuid_import VALUES {values};
DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM uuid_import m JOIN waveform_account_uuid_migrations l
   ON m.old_uuid=l.old_uuid OR m.new_uuid=l.new_uuid
   WHERE (m.old_uuid,m.new_uuid,m.kind) IS DISTINCT FROM (l.old_uuid,l.new_uuid,l.kind))
 THEN RAISE EXCEPTION 'Mapping conflicts with an already applied immutable mapping'; END IF;
 IF EXISTS(SELECT 1 FROM uuid_import m JOIN waveform_account_keys k ON k.account_uuid=m.old_uuid WHERE k.kind<>m.kind)
 THEN RAISE EXCEPTION 'Mapping kind does not match the existing account'; END IF;
 IF EXISTS(SELECT 1 FROM uuid_import m JOIN waveform_account_keys k ON k.account_uuid=m.new_uuid
   LEFT JOIN waveform_account_uuid_migrations l ON l.old_uuid=m.old_uuid
   WHERE l.old_uuid IS NULL OR k.kind<>m.kind)
 THEN RAISE EXCEPTION 'Target account already exists; refusing data merge'; END IF;
 IF EXISTS(SELECT 1 FROM uuid_import m JOIN waveform_account_pending_events e ON e.account_uuid=m.new_uuid
   LEFT JOIN waveform_account_uuid_migrations l ON l.old_uuid=m.old_uuid WHERE l.old_uuid IS NULL)
 THEN RAISE EXCEPTION 'Target already has lifecycle state; refusing merge'; END IF;
END $$;
CREATE TEMP TABLE uuid_fresh ON COMMIT DROP AS SELECT m.* FROM uuid_import m
 LEFT JOIN waveform_account_uuid_migrations l USING(old_uuid) WHERE l.old_uuid IS NULL;
CREATE TEMP TABLE uuid_actors ON COMMIT DROP AS SELECT k.storage_actor_id FROM waveform_account_keys k
 JOIN uuid_fresh m ON m.old_uuid=k.account_uuid;
-- Accounts revokes account-bound proof families. Retain proof metadata for audit,
-- discard unusable ciphertext, and require explicit approval from the new sign-in.
UPDATE waveform_storage_proofs p SET token_cipher=''::bytea,refresh_cipher=NULL,
 invalidated_at=now(),invalidated_reason='account_uuid_migrated',updated_at=now()
 WHERE invalidated_at IS NULL AND storage_actor_id IN (SELECT storage_actor_id FROM uuid_actors);
UPDATE waveform_account_keys k SET account_uuid=m.new_uuid FROM uuid_fresh m WHERE k.account_uuid=m.old_uuid;
-- The cache primary key follows via ON UPDATE CASCADE; custody is a separate reference.
UPDATE waveform_accounts a SET custodian_uuid=m.new_uuid FROM uuid_fresh m WHERE a.custodian_uuid=m.old_uuid;
UPDATE waveform_identity_links l SET accounts_uuid=m.new_uuid FROM uuid_fresh m WHERE l.accounts_uuid=m.old_uuid;
UPDATE waveform_account_pending_events e SET account_uuid=m.new_uuid FROM uuid_fresh m WHERE e.account_uuid=m.old_uuid;
UPDATE waveform_accounts_webhook_events e SET account_uuid=m.new_uuid FROM uuid_fresh m WHERE e.account_uuid=m.old_uuid;
INSERT INTO waveform_account_uuid_migrations(old_uuid,new_uuid,kind) SELECT * FROM uuid_fresh;
-- Old signed subjects must never create a second live identity after the backfill.
INSERT INTO waveform_account_pending_events(account_uuid,revoked_before,revoked_reason,deleted_at)
 SELECT old_uuid,now(),'account_uuid_migrated',now() FROM uuid_fresh
 ON CONFLICT(account_uuid) DO UPDATE SET revoked_before=GREATEST(waveform_account_pending_events.revoked_before,EXCLUDED.revoked_before),
 revoked_reason='account_uuid_migrated',deleted_at=COALESCE(waveform_account_pending_events.deleted_at,EXCLUDED.deleted_at);
SELECT json_build_object('apply',{'true' if apply else 'false'},'mapping_rows',(SELECT count(*) FROM uuid_import),
 'new_mappings',(SELECT count(*) FROM uuid_fresh),'storage_keys_preserved',(SELECT count(*) FROM uuid_actors),
 'already_applied',(SELECT count(*) FROM uuid_import)-(SELECT count(*) FROM uuid_fresh));
{end};
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mapping", type=Path)
    parser.add_argument("--apply", action="store_true", help="Commit the validated mapping; otherwise roll back")
    parser.add_argument("--psql", default=shutil.which("psql") or "/opt/homebrew/opt/postgresql@16/bin/psql")
    args = parser.parse_args()
    try:
        rows = parse_mapping(args.mapping.read_text())
    except (ValueError, OSError) as error:
        parser.error(str(error))
    database = os.environ.get("WAVEFORM_DATABASE_URL") or os.environ.get("PGDATABASE")
    if not database:
        parser.error("Set WAVEFORM_DATABASE_URL or PGDATABASE to the offline database")
    result = subprocess.run([args.psql, "-X", "-q", "-A", "-t", "-v", "ON_ERROR_STOP=1", "--dbname", database],
                            input=migration_sql(rows, args.apply), text=True,
                            env={**os.environ, "PGDATABASE": database})
    return result.returncode


if __name__ == "__main__":
    raise SystemExit(main())
