#!/usr/bin/env python3
"""One-time, paused-writer IAM 3 cutover for an existing native Waveform service."""
import argparse
import base64
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import time
import urllib.parse
import urllib.request

import upgrade


def load_importer(path):
    spec = importlib.util.spec_from_file_location('identity_import', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def sql(statement):
    # Do not print SQL: the identity export is private operator data.
    result = subprocess.run(['docker', 'exec', '-i', 'waveform-postgres', 'psql', '-U', 'postgres',
                             '-d', 'waveform', '-X', '-qAt', '-v', 'ON_ERROR_STOP=1'],
                            input=statement, text=True, capture_output=True)
    upgrade.require(result.returncode == 0, 'Database preflight or transaction failed; API must remain on its previous release')
    return result.stdout.strip()


def migration_sql(migration, rows, role, importer):
    checksum = hashlib.sha384(migration).hexdigest()
    text = migration.decode()
    body = importer.import_sql(rows).strip().removeprefix('BEGIN;').removesuffix('COMMIT;')
    quoted_role = '"' + role.replace('"', '""') + '"'
    return f'''BEGIN;
SET LOCAL ROLE {quoted_role};
DO $guard$ BEGIN
 IF EXISTS(SELECT 1 FROM _sqlx_migrations WHERE NOT success OR version > 13)
 OR (SELECT max(version) FROM _sqlx_migrations) NOT IN (12,13)
 THEN RAISE EXCEPTION 'Expected successful schema 12 or 13'; END IF;
 IF EXISTS(SELECT 1 FROM _sqlx_migrations WHERE version=13 AND checksum <> decode('{checksum}','hex'))
 THEN RAISE EXCEPTION 'Migration 13 checksum mismatch'; END IF;
 IF NOT EXISTS(SELECT 1 FROM _sqlx_migrations WHERE version=13) THEN
{text}
 INSERT INTO _sqlx_migrations(version,description,success,checksum,execution_time)
 VALUES(13,'canonical actor keys',true,decode('{checksum}','hex'),0);
 END IF;
END $guard$;
{body}
COMMIT;'''


def retained_data():
    result = {}
    for table in ['waveform_account_preferences', 'waveform_provider_keys', 'waveform_jobs',
                  'waveform_idempotency_records', 'waveform_bug_reports']:
        result[table] = sql(f"SELECT count(*) || ':' || coalesce(md5(string_agg(row_to_json(t)::text,E'\\n' ORDER BY row_to_json(t)::text)),'empty') FROM {table} t")
    return result


def pause_apply(backup, migrate, activate, verify, restore):
    upgrade.run('systemctl', 'stop', 'waveform-api')
    try:
        backup()
        migrate()
        activate()
        upgrade.run('systemctl', 'start', 'waveform-api')
        verify()
    except BaseException:
        subprocess.run(['systemctl', 'stop', 'waveform-api'], check=False)
        restore()
        upgrade.run('systemctl', 'reset-failed', 'waveform-api')
        upgrade.run('systemctl', 'start', 'waveform-api')
        verify()
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--archive', required=True, type=Path)
    parser.add_argument('--sha256', required=True)
    parser.add_argument('--source-revision', required=True)
    parser.add_argument('--backup-bucket', required=True)
    parser.add_argument('--identity-map', required=True, type=Path)
    parser.add_argument('--migration', required=True, type=Path)
    parser.add_argument('--importer', required=True, type=Path)
    args = parser.parse_args()
    upgrade.require(os.geteuid() == 0 and platform.machine() == 'aarch64', 'Run on the ARM64 host as root')
    upgrade.require(re.fullmatch('[0-9a-f]{40}', args.source_revision), 'Full source revision required')
    upgrade.require(re.fullmatch('[0-9a-f]{64}', args.sha256), 'Full archive checksum required')
    upgrade.require(upgrade.digest(args.archive) == args.sha256, 'Archive digest mismatch')
    upgrade.require(args.identity_map.stat().st_mode & 0o077 == 0, 'Identity map must be private')
    importer = load_importer(args.importer)
    rows = json.loads(args.identity_map.read_text())
    os.umask(0o077)
    with Path('/run/lock/waveform-upgrade.lock').open('w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        deploy(args, rows, importer)


def deploy(args, rows, importer):
    current = Path('/opt/waveform/current')
    old_release = current.resolve(strict=True)
    upgrade.require(current.is_symlink() and old_release.parent == Path('/opt/waveform/releases'), 'Unexpected native release path')
    old_link = os.readlink(current)
    env_path = Path('/etc/waveform/native.env')
    old_env = env_path.read_bytes()
    unit = Path('/etc/systemd/system/waveform-api.service')
    old_unit = unit.read_bytes()
    env = lambda key: upgrade.env_value(old_env, key)
    upgrade.require(env('WAVEFORM_IAM_APP_ID') == 'tos>waveform', 'Unexpected app identity')
    iam_url = env('WAVEFORM_IAM_BASE_URL').rstrip('/')
    with urllib.request.urlopen(iam_url + '/api/v1/version', timeout=20) as response:
        version = json.load(response)
    upgrade.require(version['service'] == 'silicon-iam' and int(version['build'].split('.')[0]) < 3,
                    'Deploy this adapter before IAM 3; old backend rollback requires old IAM')
    basic = base64.b64encode((env('WAVEFORM_IAM_APP_ID') + ':' + env('WAVEFORM_IAM_APP_SECRET')).encode()).decode()
    request = urllib.request.Request(iam_url + '/api/v1/oauth/introspect',
        data=urllib.parse.urlencode({'token': 'oat_' + 'A' * 43}).encode(),
        headers={'authorization': 'Basic ' + basic, 'content-type': 'application/x-www-form-urlencoded'})
    with urllib.request.urlopen(request, timeout=20) as response:
        upgrade.require(json.load(response).get('active') is False, 'IAM application authentication failed')
    local_ready = 'http://' + env('WAVEFORM_BIND_ADDR') + '/health/ready'
    public_ready = 'https://backend.waveform.teamofsilicons.com/health/ready'
    verify = lambda: (upgrade.ready(local_ready), upgrade.ready(public_ready, 30))
    verify()
    release = Path('/opt/waveform/releases') / args.source_revision
    upgrade.stage(args.archive, release, args.source_revision)
    upgrade.run('systemd-analyze', 'verify', str(release / 'waveform-api.service'))
    role = urllib.parse.unquote(urllib.parse.urlsplit(env('WAVEFORM_DATABASE_URL')).username)
    migration = migration_sql(args.migration.read_bytes(), rows, role, importer)
    stamp = time.strftime('%Y%m%dT%H%M%SZ', time.gmtime())
    backup_dir = Path('/var/backups/waveform') / f'iam3-{stamp}-{args.source_revision[:12]}'
    backup_dir.mkdir(parents=True, mode=0o700)
    shutil.copyfile(env_path, backup_dir / 'native.env')
    shutil.copyfile(unit, backup_dir / 'waveform-api.service')
    receipt_path = Path('/etc/waveform/native-release.json')
    if receipt_path.exists():
        shutil.copyfile(receipt_path, backup_dir / 'native-release.json')
    (backup_dir / 'current-link.txt').write_text(old_link + '\n')
    baseline = {}

    def backup():
        baseline.update(retained_data())
        (backup_dir / 'retained-data.json').write_text(json.dumps(baseline, indent=2))
        dump_path = backup_dir / 'postgres.dump'
        with dump_path.open('wb') as dump:
            upgrade.run('docker', 'exec', 'waveform-postgres', 'pg_dump', '-U', 'postgres', '-d', 'waveform', '-Fc', stdout=dump)
        upgrade.require(0 < dump_path.stat().st_size < 5 * 1024**3, 'Invalid backup size')
        with dump_path.open('rb') as dump:
            upgrade.run('docker', 'exec', '-i', 'waveform-postgres', 'pg_restore', '--list', stdin=dump, stdout=subprocess.DEVNULL)
        checksum = base64.b64encode(bytes.fromhex(upgrade.digest(dump_path))).decode()
        uploaded = json.loads(subprocess.check_output(['aws', 's3api', 'put-object', '--region', 'us-east-1',
            '--bucket', args.backup_bucket, '--key', f'backups/{backup_dir.name}.dump', '--body', str(dump_path),
            '--server-side-encryption', 'AES256', '--checksum-sha256', checksum, '--output', 'json']))
        upgrade.require(uploaded.get('ServerSideEncryption') == 'AES256' and uploaded.get('ChecksumSHA256') == checksum,
                        'Remote encrypted backup verification failed')

    def migrate():
        sql(migration)
        upgrade.require(retained_data() == baseline, 'Existing rows or provider ciphertext changed')

    def activate():
        upgrade.atomic_write(unit, (release / 'waveform-api.service').read_bytes(), 0o644)
        upgrade.switch_current(current, str(release))
        upgrade.run('systemctl', 'daemon-reload')
        upgrade.run('systemctl', 'reset-failed', 'waveform-api')

    def restore():
        if current.resolve() != old_release:
            upgrade.switch_current(current, old_link)
        upgrade.atomic_write(unit, old_unit, 0o644)
        upgrade.require(env_path.read_bytes() == old_env, 'Unexpected environment mutation')
        upgrade.run('systemctl', 'daemon-reload')

    pause_apply(backup, migrate, activate, verify, restore)
    upgrade.require(env_path.read_bytes() == old_env, 'Runtime keys or configuration changed')
    receipt = {'source_revision': args.source_revision, 'archive_sha256': args.sha256, 'release': str(release),
        'previous_release': str(old_release), 'backup': str(backup_dir),
        'database_backup_s3': f's3://{args.backup_bucket}/backups/{backup_dir.name}.dump',
        'runtime': 'native-systemd', 'deployed_at': stamp, 'migration': 13,
        'retained_rows_and_ciphertext_unchanged': True, 'runtime_configuration_unchanged': True,
        'canonical_actor_bindings': int(sql('SELECT count(*) FROM waveform_actor_keys'))}
    upgrade.atomic_write(receipt_path, (json.dumps(receipt, indent=2) + '\n').encode(), 0o600)
    print(json.dumps(receipt, indent=2))


if __name__ == '__main__':
    main()
