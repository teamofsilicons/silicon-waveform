#!/usr/bin/env python3
"""Bind retained Waveform row keys using a trusted pre-cutover IAM JSON export.

Stop Waveform requests while importing, before starting the canonical adapter.
Input: [{legacy_id, public_id, testing_environment_id}]. UUIDs are existing local
storage keys only; the application authenticates exclusively using public_id.
"""
import argparse
import json
import subprocess
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--database-url', required=True)
parser.add_argument('--identity-map', type=Path, required=True)
parser.add_argument('--psql', default='psql')
args = parser.parse_args()
rows = json.loads(args.identity_map.read_text())
if isinstance(rows, dict):
    rows = rows.get('production', []) + rows.get('testing', [])
if not isinstance(rows, list):
    raise SystemExit('Identity export must be an array')
encoded = json.dumps(rows).replace("'", "''")
sql = """BEGIN;
CREATE TEMP TABLE identity_import AS SELECT * FROM jsonb_to_recordset('%s'::jsonb)
 AS i(legacy_id uuid, public_id text, testing_environment_id uuid);
CREATE TEMP TABLE observed_actors AS
 SELECT plane_id,actor_id FROM waveform_account_preferences UNION
 SELECT plane_id,actor_id FROM waveform_provider_keys UNION
 SELECT plane_id,actor_id FROM waveform_jobs UNION
 SELECT plane_id,actor_id FROM waveform_idempotency_records UNION
 SELECT plane_id,actor_id FROM waveform_bug_reports;
DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM observed_actors a
 JOIN waveform_environments e ON e.id=a.plane_id
 LEFT JOIN identity_import i ON i.legacy_id=a.actor_id AND i.testing_environment_id IS NOT DISTINCT FROM e.iam_environment_id
 WHERE i.public_id IS NULL) THEN
 RAISE EXCEPTION 'IAM identity export does not cover every retained Waveform actor';
 END IF;
END $$;
INSERT INTO waveform_actor_keys(plane_id,public_id,storage_actor_id)
 SELECT DISTINCT a.plane_id,i.public_id,a.actor_id FROM observed_actors a
 JOIN waveform_environments e ON e.id=a.plane_id
 JOIN identity_import i ON i.legacy_id=a.actor_id AND i.testing_environment_id IS NOT DISTINCT FROM e.iam_environment_id
 ON CONFLICT DO NOTHING;
DO $$ BEGIN
 IF EXISTS(SELECT 1 FROM observed_actors a JOIN waveform_environments e ON e.id=a.plane_id
 JOIN identity_import i ON i.legacy_id=a.actor_id AND i.testing_environment_id IS NOT DISTINCT FROM e.iam_environment_id
 LEFT JOIN waveform_actor_keys k ON k.plane_id=a.plane_id AND k.public_id=i.public_id AND k.storage_actor_id=a.actor_id
 WHERE k.public_id IS NULL) THEN RAISE EXCEPTION 'Conflicting Waveform identity mapping'; END IF;
END $$;
COMMIT;
""" % encoded
subprocess.run([args.psql, args.database_url, '-X', '-v', 'ON_ERROR_STOP=1', '-q'], input=sql, text=True, check=True)
print('Waveform canonical identity bindings imported; retained rows and ciphertext are unchanged.')
