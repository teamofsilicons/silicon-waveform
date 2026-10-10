import argparse,hashlib,json,os,pathlib,shlex,subprocess,time
p=argparse.ArgumentParser();p.add_argument('--archive',type=pathlib.Path,required=True);p.add_argument('--sha256',required=True);p.add_argument('--revision',required=True);p.add_argument('--resume-staged',action='store_true');a=p.parse_args();os.umask(0o077)
assert hashlib.sha256(a.archive.read_bytes()).hexdigest()==a.sha256
config=pathlib.Path('/etc/waveform-accounts');unit=pathlib.Path('/etc/systemd/system/waveform-accounts-frontend.service');assert not unit.exists() or a.resume_staged
def run(args,**kw):return subprocess.run(args,check=True,capture_output=True,text=True,**kw).stdout
old=run(['systemctl','show','waveform-api','waveform-frontend','--property=MainPID,ActiveState'])
assert old.count('ActiveState=active')==2
run(['systemctl','is-active','--quiet','waveform-accounts'])
run(['docker','load','--input',str(a.archive)])
info=json.loads(run(['docker','image','inspect','waveform-frontend:release']))[0];assert info['Architecture']=='arm64' and info['Config']['Labels']['org.opencontainers.image.revision']==a.revision
image=info['Id'];assert image.startswith('sha256:')
env=dict(x.split('=',1) for x in shlex.split((config/'web.env').read_text()));assert env['APP_API_URL']=='https://api.waveform.teamofsilicons.com' and env['PUBLIC_URL']=='https://waveform.teamofsilicons.com'
assert all(not any(c in v for c in '\n\r\x00') for v in env.values())
(config/'web.docker.env').write_text(''.join(k+'='+v+'\n' for k,v in env.items()));(config/'web.docker.env').chmod(0o600)
args=['/usr/bin/docker','run','--name','waveform-accounts-frontend','--network','waveform','--memory','384m','--read-only','--cap-drop','ALL','--pids-limit','128','--security-opt','no-new-privileges','--tmpfs','/tmp:rw,nosuid,noexec,size=16m','--tmpfs','/app/.next/cache:rw,nosuid,noexec,size=64m','--log-driver','json-file','--log-opt','max-size=10m','--log-opt','max-file=3','--env-file',str(config/'web.docker.env'),'-e','PORT=4325','-e','HOSTNAME=0.0.0.0',image]
unit.write_text('[Unit]\nDescription=Waveform Accounts website\nRequires=docker.service waveform-accounts.service\nAfter=docker.service waveform-accounts.service network-online.target\n[Service]\nRestart=always\nRestartSec=5\nTimeoutStartSec=240\nTimeoutStopSec=45\nExecStartPre=-/usr/bin/docker rm waveform-accounts-frontend\nExecStart='+shlex.join(args)+'\nExecStop=/usr/bin/docker stop --time 30 waveform-accounts-frontend\n[Install]\nWantedBy=multi-user.target\n');unit.chmod(0o644)
run(['systemctl','daemon-reload']);run(['systemctl','enable','--now','waveform-accounts-frontend'])
ready=False
for _ in range(45):
 r=subprocess.run(['docker','exec','waveform-accounts-frontend','node','-e',"fetch('http://127.0.0.1:4325/auth/session').then(async r=>{let v=await r.json();process.exit(r.ok&&v.signed_in===false?0:1)}).catch(()=>process.exit(1))"],capture_output=True)
 if r.returncode==0:ready=True;break
 time.sleep(1)
assert ready,'New frontend readiness failed; website proxy not changed'
proxy=pathlib.Path('/etc/waveform/Caddyfile');previous=proxy.read_text();needle='reverse_proxy waveform-frontend:4325';assert previous.count(needle)==1
(config/'Caddyfile.before-accounts-website').write_text(previous);proxy.write_text(previous.replace(needle,'reverse_proxy waveform-accounts-frontend:4325'))
try:run(['docker','exec','waveform-proxy','caddy','validate','--config','/etc/caddy/Caddyfile']);run(['docker','exec','waveform-proxy','caddy','reload','--config','/etc/caddy/Caddyfile'])
except BaseException:proxy.write_text(previous);raise
assert run(['systemctl','show','waveform-api','waveform-frontend','--property=MainPID,ActiveState'])==old
report={'source_revision':a.revision,'archive_sha256':a.sha256,'image_id':image,'website':'https://waveform.teamofsilicons.com','signed_out_readiness':True,'old_api_and_frontend_pids_unchanged':True,'new_service':'waveform-accounts-frontend','rollback_proxy':str(config/'Caddyfile.before-accounts-website')}
(config/'frontend-release.json').write_text(json.dumps(report,indent=2));print(json.dumps(report))
