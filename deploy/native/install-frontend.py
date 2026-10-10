#!/usr/bin/env python3
"""Replace only Waveform's website container (the Next.js web, signed in with Silicon Accounts).

Run as root through SSM, after the API runs on Silicon Accounts. Does not touch the native API, Caddy, PostgreSQL or
Interface: Caddy keeps routing waveform.teamofsilicons.com to waveform-frontend:4325.

The website keeps each Carbon's session in a sealed, httpOnly cookie, so the container holds no state. Its secrets
live in /etc/waveform/web.env (root, 0600) and reach the container through --env-file, never the command line:
APP_SECRET is copied from native.env's WAVEFORM_APP_SECRET on every run (the website and the API are the same app at
Silicon Accounts), and SESSION_SECRET is generated once and kept (a new one signs every browser out).

The image (built from web/Dockerfile) must run the Next.js standalone server as an unprivileged user on PORT, write
only to /tmp and /app/.next/cache, and answer GET /auth/session with 200.
"""
import argparse
import json
import os
from pathlib import Path
import re
import secrets
import shlex
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
parser.add_argument('--image', required=True)
args = parser.parse_args()
if not re.fullmatch(r'234951665042\.dkr\.ecr\.us-east-1\.amazonaws\.com/silicon-waveform-production@sha256:[a-f0-9]{64}', args.image):
    parser.error('provide an immutable Waveform ECR image digest')
if os.geteuid() != 0:
    raise SystemExit('Run as root')
os.umask(0o077)


def run(*command, **options):
    return subprocess.run(command, check=True, text=True, **options)


def env_values(path):
    values = {}
    for line in path.read_text().splitlines():
        if line and not line.startswith('#') and '=' in line:
            key, value = line.split('=', 1)
            if len(value) >= 2 and value[0] == value[-1] and value[0] in '"\'':
                value = value[1:-1]
            if key in values:
                raise SystemExit(f'{path} sets {key} twice')
            values[key] = value
    return values


def atomic_write(path, text):
    descriptor, name = tempfile.mkstemp(prefix='.' + path.name + '-', dir=path.parent)
    try:
        with os.fdopen(descriptor, 'w') as output:
            output.write(text)
            output.flush()
            os.fsync(output.fileno())
        os.chmod(name, 0o600)
        os.replace(name, path)
    finally:
        Path(name).unlink(missing_ok=True)


config = Path('/etc/waveform')
unit = Path('/etc/systemd/system/waveform-frontend.service')
if not unit.exists():
    raise SystemExit('Expected the existing waveform-frontend service')
native = env_values(config / 'native.env')
if not native.get('WAVEFORM_APP_SECRET') or 'WAVEFORM_IAM_APP_ID' in native:
    raise SystemExit('native.env is not on Silicon Accounts yet; run deploy/native/accounts_cutover.py first')
web_env_path = config / 'web.env'
web_env = env_values(web_env_path) if web_env_path.exists() else {}
web_env['APP_SECRET'] = native['WAVEFORM_APP_SECRET']
web_env.setdefault('SESSION_SECRET', secrets.token_urlsafe(48))
atomic_write(web_env_path, ''.join(f'{key}={value}\n' for key, value in web_env.items()))

password = run('aws', 'ecr', 'get-login-password', '--region', 'us-east-1', capture_output=True).stdout
run('docker', 'login', '--username', 'AWS', '--password-stdin', args.image.split('/')[0], input=password,
    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
run('docker', 'pull', args.image)
previous = unit.read_text()
command = ['/usr/bin/docker', 'run', '--name', 'waveform-frontend', '--network', 'waveform', '--memory', '256m',
    '--read-only', '--cap-drop', 'ALL', '--pids-limit', '128', '--security-opt', 'no-new-privileges',
    '--tmpfs', '/tmp:rw,nosuid,noexec,size=16m', '--tmpfs', '/app/.next/cache:rw,nosuid,noexec,size=64m',
    '--log-driver', 'json-file', '--log-opt', 'max-size=10m', '--log-opt', 'max-file=3',
    '--env-file', str(web_env_path),
    '-e', 'NODE_ENV=production', '-e', 'PORT=4325', '-e', 'HOSTNAME=0.0.0.0',
    '-e', 'APP_ID=waveform',
    '-e', 'ACCOUNTS_URL=' + native.get('ACCOUNTS_URL', 'https://accounts.teamofsilicons.com'),
    '-e', 'APP_API_URL=https://backend.waveform.teamofsilicons.com',
    '-e', 'PUBLIC_URL=https://waveform.teamofsilicons.com', args.image]
if len(re.findall(r'^ExecStart=', previous, re.MULTILINE)) != 1:
    raise SystemExit('Expected exactly one frontend ExecStart')
candidate = re.sub(r'^ExecStart=.*$', lambda _: 'ExecStart=' + shlex.join(command), previous, flags=re.MULTILINE)
backup = config / 'frontend.service.before-accounts-web'
backup.write_text(previous)
backup.chmod(0o600)
unit.write_text(candidate)
try:
    run('systemctl', 'daemon-reload')
    run('systemctl', 'restart', 'waveform-frontend')
    for _ in range(45):
        ready = subprocess.run(['docker', 'exec', 'waveform-frontend', 'node', '-e',
            "fetch('http://127.0.0.1:4325/auth/session').then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))"],
            capture_output=True)
        if ready.returncode == 0:
            break
        time.sleep(1)
    else:
        raise RuntimeError('The website did not answer /auth/session; the previous container was restored')
except Exception:
    unit.write_text(previous)
    run('systemctl', 'daemon-reload')
    run('systemctl', 'restart', 'waveform-frontend')
    raise
images_path = config / 'images.json'
images = json.loads(images_path.read_text())
images['frontend'] = args.image
images_path.write_text(json.dumps(images, indent=2) + '\n')
print('The Waveform website is ready on Silicon Accounts. /var/lib/waveform/frontend-sessions (the earlier '
      'website\'s sign-ins) is no longer used and was left in place.')
