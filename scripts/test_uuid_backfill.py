#!/usr/bin/env python3
"""Verify account UUID backfill against all real PostgreSQL migrations, in a disposable schema."""
from pathlib import Path
import sys,subprocess,os,uuid,shutil
ROOT=Path(__file__).resolve().parents[1]
os.chdir(ROOT)
sys.path.insert(0,str(ROOT/'scripts'))
from migrate_account_uuids import parse_mapping,migration_sql
schema='backfill_'+uuid.uuid4().hex
database=os.environ.get('WAVEFORM_TEST_DATABASE_URL')
if not database:
 print('SKIP PostgreSQL UUID backfill test: set WAVEFORM_TEST_DATABASE_URL');raise SystemExit(0)
base={'PGDATABASE':database}
def q(sql,schema_on=True,ok=True):
 env={**os.environ,**base}
 if schema_on:env['PGOPTIONS']='-c search_path='+schema
 p=subprocess.run([shutil.which('psql') or '/opt/homebrew/opt/postgresql@16/bin/psql','-X','-q','-A','-t','-v','ON_ERROR_STOP=1','--dbname',base['PGDATABASE']],input=sql,text=True,env=env,capture_output=True)
 assert (p.returncode==0)==ok,p.stderr
 return p.stdout.strip()
# Reject malformed or ambiguous mapping files before opening a transaction.
for bad in ['old_uuid,new_uuid,kind\nA,bogus,carbon\n',
            f'old_uuid,new_uuid,kind\nA,{uuid.uuid4()},carbon\nA,{uuid.uuid4()},silicon\n',
            f'old_uuid,new_uuid,kind\nA,{uuid.uuid1()},carbon\n']:
 try: parse_mapping(bad)
 except ValueError: pass
 else: raise AssertionError('invalid mapping accepted')
q('CREATE SCHEMA '+schema,False)
try:
 for p in sorted(Path('migrations').glob('*.sql')):q('BEGIN;'+p.read_text()+';COMMIT;')
 owner=str(uuid.uuid4());child=str(uuid.uuid4());sk=str(uuid.uuid4());ck=str(uuid.uuid4());req=str(uuid.uuid4())
 q(f"INSERT INTO waveform_account_keys(account_uuid,storage_actor_id,kind,source) VALUES('OldA','{sk}','carbon','sign_in'),('OldS','{ck}','silicon','sign_in'); INSERT INTO waveform_accounts(account_uuid,kind,current_id,custodian_uuid) VALUES('OldA','carbon','c:stable',NULL),('OldS','silicon','si:stable','OldA'); INSERT INTO waveform_held_audio(request_id,storage_actor_id,filename,audio,reason,expires_at) VALUES('{req}','{sk}','voice.mp3','\\x112233','held',now()+interval '1 hour'); INSERT INTO waveform_storage_proofs(storage_actor_id,receiving_app,proof_id,scopes,token_cipher,token_expires_at,refresh_cipher) VALUES('{sk}','briefcase','proof-stable',ARRAY['briefcase.files.read'],'\\xaabb',now()+interval '1 hour','\\xccdd'); INSERT INTO waveform_provider_keys(plane_id,org_id,actor_id,provider,secret_cipher) VALUES('00000000-0000-0000-0000-000000000000','@','{sk}','openai','\\x010203');")
 rows=parse_mapping(f'old_uuid,new_uuid,kind\nOldA,{owner},carbon\nOldS,{child},silicon\n')
 print('dry',q(migration_sql(rows)))
 assert q("SELECT account_uuid FROM waveform_account_keys WHERE kind='carbon'")=='OldA'
 print('apply',q(migration_sql(rows,True)))
 assert q(f"SELECT storage_actor_id FROM waveform_account_keys WHERE account_uuid='{owner}'")==sk
 assert q(f"SELECT custodian_uuid FROM waveform_accounts WHERE account_uuid='{child}'")==owner
 assert q("SELECT encode(secret_cipher,'hex') FROM waveform_provider_keys")=='010203'
 assert q("SELECT encode(audio,'hex') FROM waveform_held_audio")=='112233'
 assert q("SELECT proof_id||':'||invalidated_reason||':'||(refresh_cipher IS NULL)::text FROM waveform_storage_proofs")=='proof-stable:account_uuid_migrated:true'
 assert q("SELECT count(*) FROM waveform_account_pending_events WHERE deleted_at IS NOT NULL")== '2'
 print('replay',q(migration_sql(rows,True)))
 bad=parse_mapping(f'old_uuid,new_uuid,kind\nOldA,{uuid.uuid4()},carbon\n')
 q(migration_sql(bad,True),ok=False)
 assert q('SELECT count(*) FROM waveform_account_uuid_migrations')=='2'
 print('PASS full migrations; dry/apply/replay/conflict; custody; private keys/audio unchanged; proofs reauth; old fences')
finally:q('DROP SCHEMA '+schema+' CASCADE',False)
