#!/usr/bin/env python3
"""Replace only the Accounts website after probing an exact ARM64 image; retain rollback."""
import argparse, hashlib, json, pathlib, re, subprocess, time
p=argparse.ArgumentParser();p.add_argument('--archive',type=pathlib.Path,required=True);p.add_argument('--sha256',required=True);p.add_argument('--revision',required=True);a=p.parse_args()
assert re.fullmatch('[a-f0-9]{40}',a.revision) and hashlib.sha256(a.archive.read_bytes()).hexdigest()==a.sha256
def run(args):return subprocess.run(args,check=True,capture_output=True,text=True).stdout
unit=pathlib.Path('/etc/systemd/system/waveform-accounts-frontend.service');before=unit.read_text();proxy=pathlib.Path('/etc/waveform/Caddyfile').read_bytes();assert b'reverse_proxy waveform-accounts-frontend:4325' in proxy
old=run(['systemctl','show','waveform-api','waveform-frontend','--property=MainPID,ActiveState']);assert old.count('ActiveState=active')==2
run(['docker','load','--input',str(a.archive)]);image=json.loads(run(['docker','image','inspect','waveform-frontend:release']))[0]
assert image['Architecture']=='arm64' and image['Config']['Labels']['org.opencontainers.image.revision']==a.revision
image_id=image['Id'];assert re.fullmatch('sha256:[a-f0-9]{64}',image_id)
old_image=re.findall('sha256:[a-f0-9]{64}',before);assert len(old_image)==1
name='waveform-accounts-frontend-candidate'
run(['docker','run','-d','--name',name,'--network','waveform','--memory','384m','--read-only','--cap-drop','ALL','--security-opt','no-new-privileges','--tmpfs','/tmp:rw,nosuid,noexec,size=16m','--tmpfs','/app/.next/cache:rw,nosuid,noexec,size=64m','--env-file','/etc/waveform-accounts/web.docker.env','-e','PORT=4325','-e','HOSTNAME=0.0.0.0',image_id])
def ready(container):
 for _ in range(45):
  r=subprocess.run(['docker','exec',container,'node','-e',"fetch('http://127.0.0.1:4325/auth/session').then(async r=>{let v=await r.json();process.exit(r.ok&&v.signed_in===false?0:1)}).catch(()=>process.exit(1))"],capture_output=True)
  if r.returncode==0:return True
  time.sleep(1)
 return False
try:assert ready(name),'Candidate readiness failed'
finally:run(['docker','rm','-f',name])
backup=pathlib.Path('/etc/waveform-accounts/frontend-before-'+a.revision[:7]+'.service');assert not backup.exists();backup.write_text(before);backup.chmod(0o600)
unit.write_text(before.replace(old_image[0],image_id))
try:
 run(['systemctl','daemon-reload']);run(['systemctl','restart','waveform-accounts-frontend']);assert ready('waveform-accounts-frontend'),'Updated website not ready'
except BaseException:
 unit.write_text(before);run(['systemctl','daemon-reload']);run(['systemctl','restart','waveform-accounts-frontend']);raise
assert run(['systemctl','show','waveform-api','waveform-frontend','--property=MainPID,ActiveState'])==old
assert pathlib.Path('/etc/waveform/Caddyfile').read_bytes()==proxy
result={'source_revision':a.revision,'image_id':image_id,'archive_sha256':a.sha256,'readiness':True,'legacy_services_and_proxy_unchanged':True,'rollback_unit':str(backup)}
pathlib.Path('/etc/waveform-accounts/frontend-release.json').write_text(json.dumps(result,indent=2));print(json.dumps(result))
