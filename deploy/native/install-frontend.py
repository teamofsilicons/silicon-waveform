#!/usr/bin/env python3
"""Replace only Waveform's frontend container, preserving its durable login store.

Run as root through SSM. Does not touch the native API, Caddy, PostgreSQL or Interface.
"""
import argparse
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import time

parser = argparse.ArgumentParser()
parser.add_argument('--image', required=True)
args = parser.parse_args()
if not re.fullmatch(r'234951665042\.dkr\.ecr\.us-east-1\.amazonaws\.com/silicon-waveform-production@sha256:[a-f0-9]{64}', args.image):
    parser.error('provide an immutable Waveform ECR image digest')

def run(*command, **options):
    return subprocess.run(command, check=True, text=True, **options)

config = Path('/etc/waveform')
unit = Path('/etc/systemd/system/waveform-frontend.service')
if not unit.exists():
    raise SystemExit('Expected the existing waveform-frontend service')
state = Path('/var/lib/waveform/frontend-sessions')
state.mkdir(parents=True, mode=0o700, exist_ok=True)
if state.is_symlink():
    raise SystemExit('Frontend session directory must not be a symlink')
os.chown(state, 1000, 1000)
state.chmod(0o700)
password = run('aws', 'ecr', 'get-login-password', '--region', 'us-east-1', capture_output=True).stdout
run('docker', 'login', '--username', 'AWS', '--password-stdin', args.image.split('/')[0], input=password,
    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
run('docker', 'pull', args.image)
previous = unit.read_text()
command = ['/usr/bin/docker', 'run', '--name', 'waveform-frontend', '--network', 'waveform', '--memory', '192m',
    '--read-only', '--cap-drop', 'ALL', '--pids-limit', '128', '--security-opt', 'no-new-privileges',
    '--tmpfs', '/tmp:rw,nosuid,noexec,size=16m', '--log-driver', 'json-file', '--log-opt', 'max-size=10m', '--log-opt', 'max-file=3',
    '--mount', f'type=bind,src={state},dst=/var/lib/waveform/sessions',
    '-e', 'WAVEFORM_SESSION_DIRECTORY=/var/lib/waveform/sessions',
    '-e', 'WAVEFORM_FRONTEND_ORIGIN=https://waveform.teamofsilicons.com',
    '-e', 'WAVEFORM_BACKEND_URL=https://backend.waveform.teamofsilicons.com',
    '-e', 'WAVEFORM_IAM_AUTH_ORIGIN=https://auth.iam.teamofsilicons.com', '-e', 'WAVEFORM_APP_ID=waveform', args.image]
if len(re.findall(r'^ExecStart=', previous, re.MULTILINE)) != 1:
    raise SystemExit('Expected exactly one frontend ExecStart')
candidate = re.sub(r'^ExecStart=.*$', lambda _: 'ExecStart=' + shlex.join(command), previous, flags=re.MULTILINE)
backup = config/'frontend.service.before-session-release'
backup.write_text(previous)
backup.chmod(0o600)
unit.write_text(candidate)
try:
    run('systemctl', 'daemon-reload')
    run('systemctl', 'restart', 'waveform-frontend')
    for _ in range(30):
        ready = subprocess.run(['docker', 'exec', 'waveform-frontend', 'node', '-e',
            "fetch('http://127.0.0.1:4325/api/session').then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))"], capture_output=True)
        if ready.returncode == 0:
            break
        time.sleep(1)
    else:
        raise RuntimeError('Frontend failed readiness')
except Exception:
    unit.write_text(previous)
    run('systemctl', 'daemon-reload')
    run('systemctl', 'restart', 'waveform-frontend')
    raise
images_path = config/'images.json'
images = json.loads(images_path.read_text())
images['frontend'] = args.image
images_path.write_text(json.dumps(images, indent=2) + '\n')
print('Waveform frontend is ready; session directory retained.')
