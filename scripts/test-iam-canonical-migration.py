#!/usr/bin/env python3
"""Verify schema registration and the private identity import commit atomically."""
import argparse
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import uuid

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--database-url', required=True)
parser.add_argument('--psql', default='psql')
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(root / 'deploy/native'))
import canonical_upgrade
importer = canonical_upgrade.load_importer(root / 'scripts/import-iam-identities.py')
schema = 'canonical_cutover_' + uuid.uuid4().hex

def sql(statement, scoped=True):
    return subprocess.run([args.psql, args.database_url, '-X', '-qAt', '-v', 'ON_ERROR_STOP=1'],
        input=(f'SET search_path TO {schema};\n' if scoped else '') + statement,
        text=True, check=True, capture_output=True).stdout.strip()

sql(f'CREATE SCHEMA {schema}', False)
try:
    role = sql('SELECT current_user')
    for path in sorted((root / 'migrations').glob('*.sql')):
        if int(path.name.split('_')[0]) < 13:
            sql(path.read_text())
    sql("""CREATE TABLE _sqlx_migrations(version bigint PRIMARY KEY, description text NOT NULL,
      installed_on timestamptz NOT NULL DEFAULT now(), success boolean NOT NULL,
      checksum bytea NOT NULL, execution_time bigint NOT NULL);
      INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time)
       VALUES(12,'fixture',true,''::bytea,0);""")
    legacy = str(uuid.uuid4())
    sql(f"INSERT INTO waveform_account_preferences(plane_id,org_id,actor_id) VALUES('{uuid.UUID(int=0)}','tos','{legacy}');")
    migration = (root / 'migrations/0013_canonical_actor_keys.sql').read_bytes()
    statement = lambda rows: canonical_upgrade.migration_sql(migration, rows, role, importer)
    try:
        sql(statement([]))
        raise AssertionError('Missing identity mapping accepted')
    except subprocess.CalledProcessError:
        pass
    assert sql("SELECT max(version)=12 AND to_regclass('waveform_actor_keys') IS NULL FROM _sqlx_migrations") == 't'
    rows = [{'legacy_id': legacy, 'public_id': 'saket', 'testing_environment_id': None}]
    for _ in range(2):
        sql(statement(rows))
    assert sql("SELECT max(version)=13 AND bool_and(success) FROM _sqlx_migrations") == 't'
    assert sql(f"SELECT count(*)=1 AND bool_and(storage_actor_id='{legacy}'::uuid) FROM waveform_actor_keys WHERE public_id='saket'") == 't'
    sql("UPDATE _sqlx_migrations SET checksum=''::bytea WHERE version=13")
    try:
        sql(statement(rows))
        raise AssertionError('Changed migration checksum accepted')
    except subprocess.CalledProcessError:
        pass
    print('Canonical migration/import is atomic, preserves retained keys, replays safely and rejects checksum drift.')
finally:
    sql(f'DROP SCHEMA {schema} CASCADE', False)
