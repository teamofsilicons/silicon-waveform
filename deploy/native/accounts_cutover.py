#!/usr/bin/env python3
"""Move the native Waveform API to Silicon Accounts, once, at cutover (run as root on the Waveform host).

    python3 accounts_cutover.py --archive waveform-backend-REV-linux-aarch64.tar.gz --sha256 HEX \\
        --source-revision REV --backup-bucket BUCKET --accounts-env /root/waveform-accounts.env \\
        [--mapping /root/waveform-identity-map.csv]

Checks first and changes nothing until every check passes: the bundle (exactly as upgrade.py checks it), the
accounts env file (owned by root, mode 0600, only known keys, WAVEFORM_APP_SECRET and
WAVEFORM_ACCOUNTS_WEBHOOK_SECRET present), that native.env still holds the earlier sign-in settings, that Silicon
Accounts accepts Waveform's app credentials, and that the running API is ready. Then it backs up native.env, the unit,
the release link and the database (a local dump checked with pg_restore --list, plus an encrypted S3 copy).

With the API stopped it writes the new native.env (earlier settings removed, Accounts settings added, every other byte
kept), switches the release, applies the migrations (`waveform-api migrate`) and, given --mapping, links the earlier
data to accounts (`waveform-api link-identities`: a dry run first, refused on any conflict) before anyone can sign in.
It starts the API, waits for local and public readiness, then asks Silicon Accounts to send a test webhook delivery
and waits for it to arrive (exit 3 when it does not: the API stays up; fix the webhook and check again).

Any failure after the API stopped restores the previous release, native.env and unit, and the database exactly as it
was dumped: the migrated database is renamed waveform_cutover_<stamp> (kept for inspection, never dropped), the dump
is restored into a new `waveform` database, and the previous API is started and checked again.
"""
import argparse
import base64
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import platform
import pwd
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request

_spec = importlib.util.spec_from_file_location("upgrade", Path(__file__).with_name("upgrade.py"))
upgrade = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(upgrade)
require = upgrade.require

# Keys the accounts env file may set, and the ones it must.
ACCOUNTS_KEYS = (
    "ACCOUNTS_URL", "ACCOUNTS_API_URL", "WAVEFORM_APP_ID", "WAVEFORM_APP_SECRET", "WAVEFORM_ACCOUNTS_WEBHOOK_SECRET",
    "WAVEFORM_ACCOUNTS_WEBHOOK_SECRET_PREVIOUS", "WAVEFORM_ACCOUNTS_TIMEOUT_SECONDS", "WAVEFORM_PROOF_ISSUERS",
    "WAVEFORM_PUBLIC_URL", "WAVEFORM_BRIEFCASE_APP_ID", "WAVEFORM_BRIEFCASE_APP_FOLDER", "WAVEFORM_BRIEFCASE_BASE_URL",
    "WAVEFORM_BRIEFCASE_PERMANENT_ORIGIN",
)
REQUIRED_KEYS = ("WAVEFORM_APP_SECRET", "WAVEFORM_ACCOUNTS_WEBHOOK_SECRET")
DEFAULTS = (("ACCOUNTS_URL", upgrade.DEFAULT_ACCOUNTS_URL), ("WAVEFORM_BRIEFCASE_APP_ID", "briefcase"))
KEY_LINE = re.compile(rb"^([A-Z][A-Z0-9_]*)=")


def read_accounts_env(path):
    """The new settings, from a root-only file (never from arguments, which land in shell history and SSM logs)."""
    info = path.lstat()
    require(path.is_file() and not path.is_symlink(), f"{path} must be a regular file")
    require(info.st_uid == os.geteuid() and info.st_mode & 0o077 == 0,
            f"{path} must belong to the user running this (root) with mode 0600")
    values = {}
    for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        require("=" in line, f"{path} line {number}: expected KEY=value")
        key, value = (part.strip() for part in line.split("=", 1))
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        require(key in ACCOUNTS_KEYS, f"{path} line {number}: {key} is not a Silicon Accounts setting this tool writes")
        require(key not in values, f"{path} line {number}: {key} is set twice")
        require(value and not re.search(r"[\s\x00-\x1f\x7f]", value),
                f"{path} line {number}: {key} must be one word without spaces or control characters")
        values[key] = value
    for key in REQUIRED_KEYS:
        require(key in values, f"{path} must set {key}")
    require(len(values["WAVEFORM_APP_SECRET"]) >= 32, "WAVEFORM_APP_SECRET must have at least 32 characters")
    for key in ("WAVEFORM_ACCOUNTS_WEBHOOK_SECRET", "WAVEFORM_ACCOUNTS_WEBHOOK_SECRET_PREVIOUS"):
        require(key not in values or values[key].startswith("whsec_"),
                f"{key} must be the whsec_ secret Silicon Accounts generated for Waveform's webhook")
    for key in ("ACCOUNTS_URL", "ACCOUNTS_API_URL", "WAVEFORM_PUBLIC_URL", "WAVEFORM_BRIEFCASE_BASE_URL",
                "WAVEFORM_BRIEFCASE_PERMANENT_ORIGIN"):
        require(key not in values or values[key].startswith("https://"), f"{key} must be an https:// origin")
    require(values.get("WAVEFORM_APP_ID", upgrade.APP_ID) == upgrade.APP_ID, "WAVEFORM_APP_ID must be waveform")
    require(values.get("WAVEFORM_BRIEFCASE_APP_ID", "briefcase") != upgrade.APP_ID,
            "WAVEFORM_BRIEFCASE_APP_ID is Briefcase's app id now (briefcase), never Waveform's own")
    return values


def cutover_env(old, accounts):
    """native.env for Silicon Accounts: earlier settings dropped, the new ones set, every other line byte for byte."""
    newline = b"\r\n" if b"\r\n" in old else b"\n"
    wanted = dict(DEFAULTS)
    wanted.update(accounts)
    seen, lines = set(), []
    for line in old.splitlines(keepends=True):
        match = KEY_LINE.match(line)
        key = match.group(1).decode() if match else None
        if key in upgrade.RETIRED_KEYS:
            continue
        if key in wanted:
            require(key not in seen, f"native.env sets {key} twice")
            seen.add(key)
            ending = line[len(line.rstrip(b"\r\n")):] or newline
            line = f"{key}={wanted[key]}".encode() + ending
        lines.append(line)
    if lines and not lines[-1].endswith(b"\n"):
        lines[-1] += newline
    lines += [f"{key}={value}".encode() + newline for key, value in wanted.items() if key not in seen]
    content = upgrade.upgrade_env(b"".join(lines))
    upgrade.accounts_identity(content)
    for key in ("WAVEFORM_ACCOUNTS_WEBHOOK_SECRET", "WAVEFORM_ENCRYPTION_KEY"):
        require(upgrade.env_value(content, key), f"the new native.env would have no {key}")
    return content


def earlier_env(content):
    """native.env still configured for the earlier sign-in system (what this one-time tool replaces)."""
    require(upgrade.env_value(content, "WAVEFORM_IAM_APP_ID") == upgrade.APP_ID,
            "native.env is not the earlier sign-in configuration (no WAVEFORM_IAM_APP_ID=waveform); if the cutover "
            "already ran, deploy later releases with upgrade.py")


def psql(sql, database="waveform"):
    return subprocess.check_output(["docker", "exec", "waveform-postgres", "psql", "-U", "postgres", "-d", database,
                                    "-v", "ON_ERROR_STOP=1", "-tAc", sql], text=True).strip()


POSTGRES = ("docker", "exec", "waveform-postgres")
POSTGRES_STDIN = ("docker", "exec", "-i", "waveform-postgres")


def restore_database(dump, stamp, database="waveform", owner="waveform", postgres=POSTGRES, postgres_stdin=POSTGRES_STDIN):
    """Put the database back exactly as dumped; the migrated one is kept as <database>_cutover_<stamp>, never dropped."""
    require(re.fullmatch(r"[a-z][a-z0-9_]{0,40}", database) and re.fullmatch(r"[a-z][a-z0-9_]{0,40}", owner),
            "Unexpected database or owner name")
    kept = f"{database}_cutover_{stamp.lower()}"
    upgrade.run(*postgres, "psql", "-U", "postgres", "-d", "postgres", "-v", "ON_ERROR_STOP=1",
                "-c", f"SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = '{database}' "
                      "AND pid <> pg_backend_pid()",
                "-c", f"ALTER DATABASE {database} RENAME TO {kept}",
                "-c", f"CREATE DATABASE {database} OWNER {owner}", stdout=subprocess.DEVNULL)
    with dump.open("rb") as source:
        upgrade.run(*postgres_stdin, "pg_restore", "-U", "postgres", "-d", database, "--exit-on-error",
                    "--single-transaction", stdin=source, stdout=subprocess.DEVNULL)
    return kept


def prepare_runtime(release):
    """What systemd and the unit's ExecStartPre give the service: /run/waveform (0750, the service user) with the
    private resolver entry and the database CA, so operator commands reach PostgreSQL exactly as the API does."""
    account = pwd.getpwnam("waveform")
    runtime = Path("/run/waveform")
    runtime.mkdir(mode=0o750, exist_ok=True)
    runtime.chmod(0o750)
    os.chown(runtime, account.pw_uid, account.pw_gid)
    upgrade.run("/usr/bin/python3", str(release / "prepare.py"))


def operator(release, *arguments):
    """Run `waveform-api ARGUMENTS` as the service would: its user, native.env and private resolver."""
    return subprocess.run([
        "systemd-run", "--quiet", "--wait", "--pipe", "--collect", "--uid=waveform", "--gid=waveform",
        "--property=EnvironmentFile=/etc/waveform/native.env",
        "--property=BindReadOnlyPaths=/run/waveform/hosts:/etc/hosts",
        "--property=PrivateTmp=yes", "--property=NoNewPrivileges=yes",
        str(release / "bin/waveform-api"), *arguments], capture_output=True, text=True, timeout=900, check=False)


def link_identities(release, mapping, backup):
    """Link the earlier data to accounts before anyone can sign in: dry run, refuse any conflict, then apply."""
    staged = Path("/run/waveform/identity-map.csv")
    shutil.copyfile(mapping, staged)
    os.chown(staged, 0, pwd.getpwnam("waveform").pw_gid)
    staged.chmod(0o640)
    try:
        reports = {}
        for label, extra in (("dry_run", ["--dry-run"]), ("applied", [])):
            result = operator(release, "link-identities", "--file", str(staged), *extra)
            (backup / f"link-identities-{label}.json").write_text(result.stdout)
            (backup / f"link-identities-{label}.json").chmod(0o600)
            try:
                report = json.loads(result.stdout)
            except json.JSONDecodeError:
                report = None
            require(result.returncode == 0 and report is not None and not report.get("conflicts"),
                    f"link-identities ({label.replace('_', ' ')}) refused the mapping; nothing was linked. See "
                    f"{backup}/link-identities-{label}.json and: {result.stderr.strip()[-600:]}")
            reports[label] = {key: (len(value) if isinstance(value, list) else value) for key, value in report.items()}
        return reports["applied"]
    finally:
        staged.unlink(missing_ok=True)


def accounts_request(content, method, path, body=None, idempotency_key=None):
    base = (upgrade.env_value(content, "ACCOUNTS_API_URL") or upgrade.env_value(content, "ACCOUNTS_URL")
            or upgrade.DEFAULT_ACCOUNTS_URL).rstrip("/")
    basic = base64.b64encode(f"{upgrade.APP_ID}:{upgrade.env_value(content, 'WAVEFORM_APP_SECRET')}".encode()).decode()
    headers = {"authorization": "Basic " + basic, "accept": "application/json"}
    if body is not None:
        headers["content-type"] = "application/json"
    if idempotency_key:
        headers["idempotency-key"] = idempotency_key
    request = urllib.request.Request(base + path, method=method, headers=headers,
                                     data=None if body is None else json.dumps(body).encode())
    try:
        with upgrade.accounts_open(request, timeout=20) as response:
            return response.status, json.loads(response.read() or b"{}")
    except urllib.error.HTTPError as error:
        try:
            return error.code, json.loads(error.read() or b"{}")
        except ValueError:
            return error.code, {}


def test_webhook(content, stamp, seconds=120):
    """Ask Silicon Accounts to send Waveform's webhook a ping and wait until it reports the delivery."""
    status, answer = accounts_request(content, "POST", f"/v1/apps/{upgrade.APP_ID}/webhook/test", {},
                                      idempotency_key=f"waveform-accounts-cutover-{stamp}")
    if status != 202 or not answer.get("delivery_id"):
        error = answer.get("error")
        return f"not queued ({status} {(error.get('code') if isinstance(error, dict) else error) or 'no reason given'})"
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        status, delivery = accounts_request(content, "GET",
                                            f"/v1/apps/{upgrade.APP_ID}/webhook/deliveries/{answer['delivery_id']}")
        if status == 200 and delivery.get("status") == "delivered":
            return "delivered"
        if status == 200 and delivery.get("status") == "failed":
            return f"failed: {delivery.get('last_error') or delivery.get('status')}"
        time.sleep(3)
    return "not delivered within two minutes"


def cutover(args):
    require(upgrade.digest(args.archive) == args.sha256, "Archive checksum mismatch")
    current = Path("/opt/waveform/current")
    releases = Path("/opt/waveform/releases")
    env_path = Path("/etc/waveform/native.env")
    unit = Path("/etc/systemd/system/waveform-api.service")
    receipt_path = Path("/etc/waveform/native-release.json")
    require(current.is_symlink(), "An existing native current symlink is required")
    old_link = os.readlink(current)
    old_release = current.resolve(strict=True)
    require(old_release.parent == releases.resolve(), "Current release is outside the releases directory")
    require(env_path.is_file() and not env_path.is_symlink(), "A regular native.env is required")
    require(unit.is_file() and not unit.is_symlink(), "A regular native service unit is required")
    old_env = env_path.read_bytes()
    earlier_env(old_env)
    new_env = cutover_env(old_env, read_accounts_env(args.accounts_env))
    if args.mapping:
        require(args.mapping.is_file(), f"{args.mapping} is not a file")
    bind = upgrade.env_value(new_env, "WAVEFORM_BIND_ADDR")
    require(bind and bind.count(":") == 1, "An explicit private IPv4 bind is required")
    local_ready = f"http://{bind}/health/ready"
    if not upgrade.env_value(new_env, "WAVEFORM_PROOF_ISSUERS"):
        print("warning: WAVEFORM_PROOF_ISSUERS is empty, so Waveform refuses every proof and other apps "
              "(the Silicon Interface) cannot call it for an account", file=sys.stderr)
    upgrade.accounts_preflight(new_env)
    upgrade.run("systemctl", "is-active", "--quiet", "waveform-api")
    upgrade.ready(local_ready, 15)
    upgrade.ready(args.public_ready_url, 15)
    migrations_before = psql("SELECT coalesce(max(version), 0) FROM _sqlx_migrations WHERE success")
    release = releases / args.source_revision
    upgrade.stage(args.archive, release, args.source_revision)
    upgrade.run("systemd-analyze", "verify", str(release / "waveform-api.service"))
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    backup = Path("/var/backups/waveform") / f"accounts-cutover-{stamp}-{args.source_revision[:12]}"
    backup.mkdir(parents=True, mode=0o700)
    shutil.copyfile(env_path, backup / "native.env")
    shutil.copyfile(unit, backup / "waveform-api.service")
    (backup / "current-link.txt").write_text(old_link + "\n")
    if receipt_path.is_file():
        shutil.copyfile(receipt_path, backup / "native-release.json")
    dump, database_backup = upgrade.backup_database(backup, args.backup_bucket, args.region, args.backup_kms_key_id)
    old_unit = (backup / "waveform-api.service").read_bytes()
    database_touched = False
    links = "not run: link later with waveform-api link-identities (docs/migration/cutover.md)"
    try:
        upgrade.run("systemctl", "stop", "waveform-api")
        upgrade.atomic_write(env_path, new_env, 0o600)
        upgrade.atomic_write(unit, (release / "waveform-api.service").read_bytes(), 0o644)
        upgrade.switch_current(current, str(release))
        upgrade.run("systemctl", "daemon-reload")
        prepare_runtime(release)
        database_touched = True
        migrated = operator(release, "migrate")
        require(migrated.returncode == 0, f"waveform-api migrate failed: {migrated.stderr.strip()[-600:]}")
        if args.mapping:
            links = link_identities(release, args.mapping, backup)
        upgrade.run("systemctl", "reset-failed", "waveform-api")
        upgrade.run("systemctl", "start", "waveform-api")
        upgrade.ready(local_ready)
        upgrade.ready(args.public_ready_url, 30)
    except BaseException as failure:
        subprocess.run(["systemctl", "stop", "waveform-api"], check=False)
        upgrade.switch_current(current, old_link)
        upgrade.atomic_write(env_path, old_env, 0o600)
        upgrade.atomic_write(unit, old_unit, 0o644)
        upgrade.run("systemctl", "daemon-reload")
        kept = restore_database(dump, stamp) if database_touched else None
        upgrade.run("systemctl", "reset-failed", "waveform-api")
        upgrade.run("systemctl", "start", "waveform-api")
        upgrade.ready(f"http://{upgrade.env_value(old_env, 'WAVEFORM_BIND_ADDR')}/health/ready")
        upgrade.ready(args.public_ready_url, 30)
        print(f"Cutover failed ({failure}); the previous release, native.env and unit are back"
              + (f", and the database was restored from {dump} (the migrated one is kept as {kept})." if kept else "."),
              file=sys.stderr)
        raise
    webhook = test_webhook(new_env, stamp)
    receipt = {"source_revision": args.source_revision, "archive_sha256": args.sha256, "release": str(release),
               "previous_release": str(old_release), "backup": str(backup), "database_backup_s3": database_backup,
               "runtime": "native-systemd", "deployed_at": stamp, "bind": bind, "identity": "silicon-accounts",
               "migrations_before": int(migrations_before),
               "migrations_after": int(psql("SELECT max(version) FROM _sqlx_migrations WHERE success")),
               "identity_links": links, "webhook_test": webhook}
    upgrade.atomic_write(receipt_path, (json.dumps(receipt, indent=2) + "\n").encode(), 0o600)
    print(json.dumps(receipt, indent=2))
    if webhook != "delivered":
        print(f"The API runs on Silicon Accounts, but the webhook test was {webhook}. Check the webhook URL and "
              "WAVEFORM_ACCOUNTS_WEBHOOK_SECRET (docs/migration/cutover.md), then test it again.", file=sys.stderr)
        return 3
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--source-revision", required=True)
    parser.add_argument("--backup-bucket", required=True)
    parser.add_argument("--backup-kms-key-id")
    parser.add_argument("--region", default="us-east-1")
    parser.add_argument("--accounts-env", required=True, type=Path,
                        help="root-only file with WAVEFORM_APP_SECRET, WAVEFORM_ACCOUNTS_WEBHOOK_SECRET and the other "
                             "Silicon Accounts settings")
    parser.add_argument("--mapping", type=Path, help="iam_principal_id,accounts_uuid rows to link before start")
    parser.add_argument("--public-ready-url", default="https://backend.waveform.teamofsilicons.com/health/ready")
    args = parser.parse_args()
    require(os.geteuid() == 0, "Run as root")
    require(platform.machine() == "aarch64", "This bundle requires an ARM64 host")
    require(re.fullmatch(r"[0-9a-f]{40}", args.source_revision), "Use the full 40-character source revision")
    require(re.fullmatch(r"[0-9a-f]{64}", args.sha256), "Use the full archive SHA-256")
    require(re.fullmatch(r"[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]", args.backup_bucket), "Invalid backup bucket")
    require(args.public_ready_url.startswith("https://"), "Public readiness must use HTTPS")
    os.umask(0o077)
    with Path("/run/lock/waveform-upgrade.lock").open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return cutover(args)


if __name__ == "__main__":
    sys.exit(main())
