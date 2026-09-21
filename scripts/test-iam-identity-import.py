#!/usr/bin/env python3
"""Check scoped canonical imports against a disposable PostgreSQL schema."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
from urllib.parse import urlencode
import uuid

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--database-url', required=True)
parser.add_argument('--psql', default='psql')
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]
schema = 'identity_import_' + uuid.uuid4().hex

def sql(statement, scoped=True):
    return subprocess.run([args.psql, args.database_url, '-X', '-q', '-v', 'ON_ERROR_STOP=1'], input=(f'SET search_path TO {schema};\n' if scoped else '') + statement, text=True, check=True, capture_output=True)

sql(f'CREATE SCHEMA {schema}', False)
try:
    for migration in sorted((root / 'migrations').glob('*.sql')):
        sql(migration.read_text())
    zero = str(uuid.UUID(int=0))
    worlds = [zero, str(uuid.uuid4()), str(uuid.uuid4())]
    keys = [str(uuid.uuid4()) for _ in worlds]
    for world, key in zip(worlds, keys):
        if world != zero:
            sql(f"INSERT INTO waveform_environments(id,name,org_id,creator_id,root_key_hash,root_key_cipher,iam_key_cipher,briefcase_key_cipher,iam_environment_id) VALUES('{world}','Test','tos','{key}',decode(replace('{world}','-',''),'hex'),'x','x','x','{world}');")
        sql(f"INSERT INTO waveform_account_preferences(plane_id,org_id,actor_id) VALUES('{world}','tos','{key}'); INSERT INTO waveform_provider_keys VALUES('{world}','tos','{key}','openai',decode('cafe','hex'),now());")
    mapped = [{'legacy_id': key, 'public_id': 'saket', 'testing_environment_id': None if world == zero else world} for world, key in zip(worlds, keys)]
    with tempfile.TemporaryDirectory(prefix='waveform-identity-fixture-') as directory:
        export = Path(directory) / 'map.json'
        url = args.database_url + ('&' if '?' in args.database_url else '?') + urlencode({'options': f'-csearch_path={schema}'})
        command = ['python3', str(root / 'scripts/import-iam-identities.py'), '--database-url', url, '--identity-map', str(export), '--psql', args.psql]
        export.write_text(json.dumps({'production': mapped[:1], 'testing': mapped[1:2]}))
        export.chmod(0o600)
        failed = subprocess.run(command, capture_output=True, text=True)
        if failed.returncode == 0:
            raise AssertionError('Incomplete export accepted')
        sql("DO $$BEGIN IF EXISTS(SELECT 1 FROM waveform_actor_keys) THEN RAISE EXCEPTION 'partial import persisted'; END IF; END$$;")
        export.write_text(json.dumps({'production': mapped[:1], 'testing': mapped[1:]}))
        for _ in range(2):
            subprocess.run(command, check=True, capture_output=True, text=True)
        sql("""DO $$BEGIN
        IF (SELECT count(*) FROM waveform_actor_keys WHERE public_id='saket') <> 3
         OR EXISTS(SELECT 1 FROM waveform_account_preferences p LEFT JOIN waveform_actor_keys k ON k.plane_id=p.plane_id AND k.storage_actor_id=p.actor_id WHERE k.public_id IS NULL)
         OR EXISTS(SELECT 1 FROM waveform_provider_keys WHERE encode(secret_cipher,'hex')<>'cafe')
        THEN RAISE EXCEPTION 'identity isolation or retained ciphertext changed'; END IF;
        END$$;""")
    print('Waveform production + two test environments preserve keys/ciphertext; missing imports fail atomically and exact imports replay.')
finally:
    sql(f'DROP SCHEMA {schema} CASCADE', False)
