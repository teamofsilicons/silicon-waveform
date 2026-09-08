#!/usr/bin/env python3
"""Create/start the isolated PostgreSQL fixture used by Waveform local tests."""
import os
from pathlib import Path
import secrets
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
STATE = ROOT / '.local-test'
STATE.mkdir(mode=0o700, exist_ok=True)
STATE.chmod(0o700)
PG = Path('/opt/homebrew/opt/postgresql@16/bin')
if not PG.is_dir():
    found = shutil.which('initdb')
    if not found:
        raise SystemExit('Install PostgreSQL 16+ and add its bin directory to PATH.')
    PG = Path(found).parent
PORT = '55439'
PASSWORD = STATE / 'password'
if not PASSWORD.exists():
    PASSWORD.write_text(secrets.token_hex(32))
    PASSWORD.chmod(0o600)
DATA = STATE / 'postgres'
if not (DATA / 'PG_VERSION').exists():
    subprocess.run([str(PG / 'initdb'), '-D', str(DATA), '-U', 'waveform_test', '-A', 'scram-sha-256', '--pwfile', str(PASSWORD)], check=True, stdout=subprocess.DEVNULL)
status = subprocess.run([str(PG / 'pg_ctl'), '-D', str(DATA), 'status'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
if status.returncode:
    subprocess.run([str(PG / 'pg_ctl'), '-D', str(DATA), '-l', str(STATE / 'postgres.log'), '-o', f'-h 127.0.0.1 -p {PORT} -k {STATE}', '-w', 'start'], check=True, stdout=subprocess.DEVNULL)
env = dict(os.environ, PGPASSWORD=PASSWORD.read_text())
exists = subprocess.run([str(PG / 'psql'), '-h', '127.0.0.1', '-p', PORT, '-U', 'waveform_test', '-d', 'postgres', '-tAc', "SELECT 1 FROM pg_database WHERE datname='waveform_test'"], env=env, check=True, capture_output=True, text=True)
if not exists.stdout.strip():
    subprocess.run([str(PG / 'createdb'), '-h', '127.0.0.1', '-p', PORT, '-U', 'waveform_test', 'waveform_test'], env=env, check=True)
config = STATE / 'environment'
url = f'postgres://waveform_test:{PASSWORD.read_text()}@127.0.0.1:{PORT}/waveform_test'
config.write_text(f"export WAVEFORM_TEST_DATABASE_URL='{url}'\n")
config.chmod(0o600)
print(f'Isolated PostgreSQL is ready on 127.0.0.1:{PORT}; source .local-test/environment to run database tests.')
