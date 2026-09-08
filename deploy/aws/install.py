#!/usr/bin/env python3
"""Install/update one Waveform host. Secrets are fetched on the host, never printed."""
import argparse
import base64
import json
import os
from pathlib import Path
import shlex
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request

p = argparse.ArgumentParser()
p.add_argument('--secret-arn', required=True)
p.add_argument('--image', required=True)
p.add_argument('--frontend-image', help='Frontend image; retain the installed image when omitted')
p.add_argument('--backup-bucket', required=True)
p.add_argument('--region', default='us-east-1')
p.add_argument('--start-api', action='store_true')
a = p.parse_args()
os.umask(0o077)
config = Path('/etc/waveform')
data = Path('/var/lib/waveform')
config.mkdir(mode=0o700, exist_ok=True)
data.mkdir(mode=0o700, exist_ok=True)


def run(cmd, **kw):
    return subprocess.run(cmd, check=True, **kw)


def write(path, body, mode=0o600):
    path = Path(path)
    path.write_text(body)
    path.chmod(mode)


def env_text(values):
    for key, value in values.items():
        if any(c in str(value) for c in '\r\n\x00'):
            raise ValueError('multiline environment values are not supported')
    return ''.join(f'{key}={value}\n' for key, value in values.items())


secret = json.loads(run([
    'aws', 'secretsmanager', 'get-secret-value', '--region', a.region,
    '--secret-id', a.secret_arn, '--query', 'SecretString', '--output', 'text',
], capture_output=True, text=True).stdout)
env = {}
for line in Path(__file__).with_name('defaults.env').read_text().splitlines():
    if line and not line.startswith('#') and '=' in line:
        k, v = line.split('=', 1)
        env[k] = v
env.update({k: str(v) for k, v in secret.items() if k.startswith('WAVEFORM_')})
env.update({
    'WAVEFORM_ENVIRONMENT': 'production',
    'WAVEFORM_BIND_ADDR': '0.0.0.0:8080',
    'WAVEFORM_DATABASE_URL': 'postgres://waveform:' + urllib.parse.quote(secret['WAVEFORM_POSTGRES_PASSWORD'], safe='') + '@postgres/waveform?sslmode=verify-full&sslrootcert=/etc/waveform/db-ca.crt',
    'WAVEFORM_DATABASE_MAX_CONNECTIONS': '8',
    'WAVEFORM_IAM_BASE_URL': 'https://backend.iam.teamofsilicons.com',
    'WAVEFORM_IAM_APP_ID': 'tos>waveform',
    'WAVEFORM_IAM_AUDIENCE': 'tos>waveform',
    'WAVEFORM_BRIEFCASE_BASE_URL': 'https://backend.briefcase.teamofsilicons.com',
    'WAVEFORM_BRIEFCASE_PERMANENT_ORIGIN': 'https://briefcase.teamofsilicons.com',
    'WAVEFORM_BRIEFCASE_CDN_ORIGIN': 'https://briefcase.teamofsilicons.com',
    'WAVEFORM_BRIEFCASE_APP_ID': 'tos>waveform',
    'WAVEFORM_BRIEFCASE_AUDIENCE': 'tos>briefcase',
    'WAVEFORM_WEBHOOK_PUBLIC_URL': 'https://backend.waveform.teamofsilicons.com/webhook/',
    'WAVEFORM_REQUIRE_FULL_PROVIDER_CHAIN': 'true',
    'WAVEFORM_LOG_JSON': 'true',
    'WAVEFORM_MAX_IN_FLIGHT': '8',
    'WAVEFORM_FFMPEG_PATH': '/usr/bin/ffmpeg',
    'WAVEFORM_GEMINI_MAX_CONCURRENCY': '2',
    'WAVEFORM_ELEVENLABS_MAX_CONCURRENCY': '2',
    'WAVEFORM_OPENAI_MAX_CONCURRENCY': '2',
    'WAVEFORM_DEEPGRAM_MAX_CONCURRENCY': '2',
    'SILICON_IAM_CLIENT_AUTO_UPDATE': 'false',
    'BRIEFCASE_CLIENT_AUTO_UPDATE': 'false',
})
for key in ['WAVEFORM_' + s for s in ['IAM_APP_SECRET', 'WEBHOOK_SIGNING_SECRET', 'ENCRYPTION_KEY', 'IDEMPOTENCY_DIGEST_KEY', 'GEMINI_API_KEY', 'ELEVENLABS_API_KEY', 'OPENAI_API_KEY', 'DEEPGRAM_API_KEY']]:
    if not env.get(key):
        raise ValueError('Required configuration is absent: ' + key)
write(config / 'api.env', env_text(env))
write(config / 'postgres.env', env_text({
    'POSTGRES_DB': 'waveform', 'POSTGRES_USER': 'postgres',
    'POSTGRES_PASSWORD': secret['POSTGRES_PASSWORD'],
    'POSTGRES_INITDB_ARGS': '--auth-host=scram-sha-256',
}))

# Keep the same private CA, server certificate and database password on updates.
tls = config / 'postgres-tls'
tls.mkdir(mode=0o755, exist_ok=True)
if not (tls / 'server.key').exists():
    run(['openssl', 'req', '-x509', '-newkey', 'rsa:3072', '-nodes', '-days', '3650',
         '-subj', '/CN=Waveform PostgreSQL CA', '-keyout', str(tls/'ca.key'),
         '-out', str(tls/'ca.crt')], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    run(['openssl', 'req', '-newkey', 'rsa:3072', '-nodes', '-subj', '/CN=postgres',
         '-keyout', str(tls/'server.key'), '-out', str(tls/'server.csr')],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    write(tls/'extensions.cnf', 'subjectAltName=DNS:postgres\nextendedKeyUsage=serverAuth\n')
    run(['openssl', 'x509', '-req', '-days', '825', '-in', str(tls/'server.csr'),
         '-CA', str(tls/'ca.crt'), '-CAkey', str(tls/'ca.key'), '-CAcreateserial',
         '-extfile', str(tls/'extensions.cnf'), '-out', str(tls/'server.crt')],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for name in ('server.key', 'server.crt'):
        os.chown(tls/name, 999, 999)
    (tls/'server.key').chmod(0o600)
    (tls/'server.crt').chmod(0o644)
    (tls/'ca.crt').chmod(0o644)

# App-owned database, without the PostgreSQL superuser credential in the API.
password = secret['WAVEFORM_POSTGRES_PASSWORD'].replace("'", "''")
init = config/'init.sql'
write(init, f"CREATE ROLE waveform LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE PASSWORD '{password}';\nALTER DATABASE waveform OWNER TO waveform;\nGRANT ALL ON SCHEMA public TO waveform;\n")
os.chown(init, 999, 999)
write(config/'pg_hba.conf', 'local all all trust\nhostssl all all 0.0.0.0/0 scram-sha-256\nhostnossl all all 0.0.0.0/0 reject\n', 0o644)
previous_images = json.loads((config/'images.json').read_text()) if (config/'images.json').exists() else {}
frontend_image = a.frontend_image or previous_images.get('frontend')
proxy_config = '''backend.waveform.teamofsilicons.com {
    encode zstd gzip
    header {
        Strict-Transport-Security "max-age=31536000"
        X-Content-Type-Options nosniff
        -Server
    }
    reverse_proxy waveform-api:8080
}
'''
if frontend_image:
    proxy_config += '''
waveform.teamofsilicons.com {
    encode zstd gzip
    header {
        Strict-Transport-Security "max-age=31536000"
        X-Content-Type-Options nosniff
        -Server
    }
    reverse_proxy waveform-frontend:4325
}
'''
maintenance_config = proxy_config.replace('reverse_proxy waveform-api:8080', 'header Retry-After 120\n    respond \"Service temporarily unavailable\" 503')
write(config/'Caddyfile', maintenance_config, 0o644)
for name in ('caddy-data', 'caddy-config'):
    directory=data/name
    directory.mkdir(mode=0o700, exist_ok=True)
    os.chown(directory, 10001, 10001)

# Fetch images before changing any running service. Use their immutable digests.
registry=a.image.split('/')[0]
credential=run(['aws','ecr','get-login-password','--region',a.region],capture_output=True,text=True).stdout
run(['docker','login','--username','AWS','--password-stdin',registry],input=credential,text=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
pinned={}
images = [('api',a.image),('postgres',previous_images.get('postgres','postgres:17-bookworm')),('proxy',previous_images.get('proxy','caddy:2-alpine'))]
if frontend_image:
    images.append(('frontend',frontend_image))
for name, image in images:
    run(['docker','pull',image],stdout=subprocess.DEVNULL)
    digests=json.loads(run(['docker','image','inspect',image,'--format','{{json .RepoDigests}}'],capture_output=True,text=True).stdout)
    repository = image.split('@')[0] if '@' in image else image.rsplit(':',1)[0]
    pinned[name]=next((x for x in digests if x.split('@')[0]==repository),digests[0])
write(config/'images.json',json.dumps(pinned,indent=2)+'\n')

wait_pg = '''#!/bin/bash
set -euo pipefail
for i in $(seq 1 90); do
 if docker exec waveform-postgres pg_isready -h 127.0.0.1 -U postgres >/dev/null 2>&1; then exit 0; fi
 sleep 2
done
exit 1
'''
write('/usr/local/sbin/waveform-wait-postgres',wait_pg,0o700)
common=['--log-driver','json-file','--log-opt','max-size=10m','--log-opt','max-file=3','--network','waveform','--security-opt','no-new-privileges']
commands={
 'postgres':['docker','run','--name','waveform-postgres','--network-alias','postgres','--memory','640m','--env-file',str(config/'postgres.env'),
     '-v',str(data/'postgres')+':/var/lib/postgresql/data','-v',str(init)+':/docker-entrypoint-initdb.d/001-waveform.sql:ro',
     '-v',str(tls/'server.key')+':/etc/postgres-tls/server.key:ro','-v',str(tls/'server.crt')+':/etc/postgres-tls/server.crt:ro',
     '-v',str(config/'pg_hba.conf')+':/etc/postgres-hba.conf:ro',*common,pinned['postgres'],
     '-c','ssl=on','-c','ssl_cert_file=/etc/postgres-tls/server.crt','-c','ssl_key_file=/etc/postgres-tls/server.key',
     '-c','hba_file=/etc/postgres-hba.conf','-c','shared_buffers=64MB','-c','max_connections=40'],
 'api':['docker','run','--name','waveform-api','--memory','768m','--read-only','--cap-drop','ALL','--tmpfs','/tmp:rw,nosuid,noexec,size=64m',
     '--env-file',str(config/'api.env'),'-v',str(tls/'ca.crt')+':/etc/waveform/db-ca.crt:ro',*common,pinned['api']],
 'proxy':['docker','run','--name','waveform-proxy','--memory','128m','--user','10001:10001','--read-only','--cap-drop','ALL',
     '--cap-add','NET_BIND_SERVICE','--tmpfs','/tmp:rw,nosuid,noexec,size=32m','-p','80:80','-p','443:443',
     '-v',str(config/'Caddyfile')+':/etc/caddy/Caddyfile:ro','-v',str(data/'caddy-data')+':/data',
     '-v',str(data/'caddy-config')+':/config',*common,pinned['proxy']],
}
if frontend_image:
    commands['frontend'] = ['docker','run','--name','waveform-frontend','--memory','192m',
        '--read-only','--cap-drop','ALL','--pids-limit','128',
        '--tmpfs','/tmp:rw,nosuid,noexec,size=16m',
        '-e','WAVEFORM_FRONTEND_ORIGIN=https://waveform.teamofsilicons.com',
        '-e','WAVEFORM_BACKEND_URL=https://backend.waveform.teamofsilicons.com',
        '-e','WAVEFORM_IAM_AUTH_ORIGIN=https://auth.iam.teamofsilicons.com',
        '-e','WAVEFORM_APP_ID=tos>waveform',*common,pinned['frontend']]
for name,command in commands.items():
    unit=f'''[Unit]
Description=Waveform {name}
Requires=docker.service
After=docker.service network-online.target
StartLimitIntervalSec=0

[Service]
Restart=always
RestartSec=5
TimeoutStartSec=240
TimeoutStopSec=45
ExecStartPre=-/usr/bin/docker rm waveform-{name}
'''
    if name=='api':unit+='ExecStartPre=/usr/local/sbin/waveform-wait-postgres\n'
    unit+='ExecStart='+shlex.join(['/usr/bin/docker',*command[1:]])+'\n'
    unit+=f'ExecStop=/usr/bin/docker stop --time 30 waveform-{name}\n\n[Install]\nWantedBy=multi-user.target\n'
    write('/etc/systemd/system/waveform-'+name+'.service',unit,0o644)

backup=f'''#!/bin/bash
set -euo pipefail
umask 077
file=$(mktemp /var/backups/waveform/backup.XXXXXX)
trap 'rm -f "$file"' EXIT
docker exec waveform-postgres pg_dump -U postgres -d waveform -Fc > "$file"
aws s3 cp "$file" s3://{a.backup_bucket}/backups/$(date -u +%Y-%m-%dT%H-%M-%SZ).dump --region {a.region} --only-show-errors
'''
write('/usr/local/sbin/waveform-backup',backup,0o700)
write('/etc/systemd/system/waveform-backup.service','[Unit]\nDescription=Waveform encrypted database backup\nAfter=waveform-postgres.service\n[Service]\nType=oneshot\nExecStart=/usr/local/sbin/waveform-backup\n',0o644)
write('/etc/systemd/system/waveform-backup.timer','[Unit]\nDescription=Daily Waveform database backup\n[Timer]\nOnCalendar=*-*-* 03:15:00 UTC\nRandomizedDelaySec=300\nPersistent=true\n[Install]\nWantedBy=timers.target\n',0o644)
run(['systemctl','daemon-reload'])
run(['systemctl','enable','waveform-postgres','waveform-proxy','waveform-backup.timer'])
run(['systemctl','start','waveform-postgres'])
run(['/usr/local/sbin/waveform-wait-postgres'])
run(['systemctl','restart','waveform-proxy'])
if a.start_api:
    # Reject invalid application credentials before reporting the public API ready.
    encoded=base64.b64encode((env['WAVEFORM_IAM_APP_ID']+':'+env['WAVEFORM_IAM_APP_SECRET']).encode()).decode()
    request=urllib.request.Request(env['WAVEFORM_IAM_BASE_URL']+'/api/v1/oauth/introspect',data=urllib.parse.urlencode({'token':'oat_'+'A'*43}).encode(),headers={'authorization':'Basic '+encoded,'content-type':'application/x-www-form-urlencoded','x-org-id':'tos'})
    try:
        with urllib.request.urlopen(request,timeout=20) as response:
            assert response.status==200
    except urllib.error.HTTPError as error:
        raise SystemExit(f'IAM application authentication rejected ({error.code}); API was not activated') from None
    run(['systemctl','enable','waveform-api'])
    run(['systemctl','restart','waveform-api'])
    ready=False
    deadline=time.monotonic()+120
    while time.monotonic()<deadline:
        try:
            probe=subprocess.run(['docker','exec','waveform-proxy','wget','-T','3','-q','-O','/dev/null','http://waveform-api:8080/health/ready'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=5)
        except subprocess.TimeoutExpired:
            continue
        if probe.returncode==0:
            ready=True
            break
        time.sleep(2)
    if not ready:raise SystemExit('API did not become ready; public endpoint remains unavailable')
    if frontend_image:
        run(['systemctl','enable','waveform-frontend'])
        run(['systemctl','restart','waveform-frontend'])
        frontend_ready = False
        for _ in range(30):
            probe = subprocess.run(['docker','exec','waveform-proxy','wget','-T','3','-q','-O','/dev/null',
                'http://waveform-frontend:4325/'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
            if probe.returncode == 0:
                frontend_ready = True
                break
            time.sleep(2)
        if not frontend_ready:
            raise SystemExit('Frontend did not become ready; proxy activation stopped')
    write(config/'Caddyfile',proxy_config,0o644)
    run(['docker','exec','waveform-proxy','caddy','reload','--config','/etc/caddy/Caddyfile','--adapter','caddyfile'],stdout=subprocess.DEVNULL)
    run(['systemctl','start','waveform-backup.timer'])
    print('Waveform application activated; verify public readiness and authentication')
else:
    print('Database and HTTPS proxy installed; API activation is waiting for valid production IAM credentials')
