from pathlib import Path
import subprocess,time
root=Path(__file__).resolve().parents[2]
cid=subprocess.check_output(['docker','run','--rm','-d','-e','POSTGRES_PASSWORD=postgres','postgres:16-alpine'],text=True).strip()
try:
 for n in range(60):
  r=subprocess.run(['docker','exec',cid,'pg_isready','-h','127.0.0.1','-U','postgres'],capture_output=True)
  if r.returncode==0:break
  time.sleep(.25)
 else:raise RuntimeError('postgres not ready')
 def sql(text,ok=True):
  r=subprocess.run(['docker','exec','-i',cid,'psql','-X','-v','ON_ERROR_STOP=1','-U','postgres'],input=text,text=True,capture_output=True)
  if (r.returncode==0)!=ok:raise RuntimeError(r.stderr)
  return r.stdout
 for p in sorted((root/'silicon-waveform/migrations').glob('*.sql')):
  if p.name.startswith('0014'):break
  sql('BEGIN;\n'+p.read_text()+'\nCOMMIT;')
 sql("INSERT INTO waveform_actor_keys VALUES('00000000-0000-0000-0000-000000000000','alice','11111111-1111-4111-8111-111111111111'),('00000000-0000-0000-0000-000000000000','assistant:tos','22222222-2222-4222-8222-222222222222');")
 sql('BEGIN;\n'+(root/'silicon-waveform/migrations/0014_public_identifier_schema.sql').read_text()+'\nCOMMIT;')
 out=sql("SELECT public_id,storage_actor_id FROM waveform_actor_keys ORDER BY public_id;")
 assert 'c:alice' in out and 'si:assistant' in out and '11111111-1111-4111-8111-111111111111' in out and '22222222-2222-4222-8222-222222222222' in out,out
 print('Waveform: full migration chain + public rename preserves both original storage UUIDs.')
 sql("INSERT INTO waveform_actor_keys VALUES('00000000-0000-0000-0000-000000000000','assistant:other','33333333-3333-4333-8333-333333333333');")
 sql('BEGIN;\n'+(root/'silicon-waveform/migrations/0014_public_identifier_schema.sql').read_text()+'\nCOMMIT;',ok=False)
 out=sql("SELECT public_id,storage_actor_id FROM waveform_actor_keys WHERE public_id='assistant:other';")
 assert '33333333-3333-4333-8333-333333333333' in out,out
 print('Waveform: many-to-one collision fails without changing the retained owner.')
finally:subprocess.run(['docker','rm','-f',cid],capture_output=True)
