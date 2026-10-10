#!/usr/bin/env python3
"""Upgrade an existing native Waveform API; supporting containers stay untouched."""
import argparse
import base64
import fcntl
import hashlib
import ipaddress
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import struct
import subprocess
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request


BODY_LIMIT = "WAVEFORM_JSON_BODY_LIMIT_BYTES"
APP_ID = "waveform"
DEFAULT_ACCOUNTS_URL = "https://accounts.teamofsilicons.com"
# Settings of the earlier sign-in and distribution systems. A native.env holding any of them has not been through
# accounts_cutover.py yet; the Accounts-era service ignores them (with a warning) or refuses them at start.
RETIRED_KEYS = (
    "WAVEFORM_IAM_BASE_URL", "WAVEFORM_IAM_APP_ID", "WAVEFORM_IAM_APP_SECRET", "WAVEFORM_IAM_TOKEN_INTROSPECTION_PATH",
    "WAVEFORM_IAM_OBO_VERIFY_PATH", "WAVEFORM_IAM_AUDIENCE", "WAVEFORM_IAM_TTS_ACTION", "WAVEFORM_IAM_STT_ACTION",
    "WAVEFORM_IAM_TIMEOUT_SECONDS", "WAVEFORM_WEBHOOK_SIGNING_SECRET", "WAVEFORM_WEBHOOK_KEY_VERSION",
    "WAVEFORM_WEBHOOK_PUBLIC_URL", "WAVEFORM_HONEYCOMB_SERVICE_TOKEN", "SILICON_IAM_CLIENT_AUTO_UPDATE",
    "WAVEFORM_BRIEFCASE_CDN_ORIGIN", "WAVEFORM_BRIEFCASE_AUDIENCE", "BRIEFCASE_CLIENT_AUTO_UPDATE",
)


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def digest(path):
    checksum = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            checksum.update(block)
    return checksum.hexdigest()


def env_value(content, key):
    values = []
    for line in content.decode("utf-8").splitlines():
        if line.startswith(key + "="):
            value = line.split("=", 1)[1].strip()
            if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
                value = value[1:-1]
            values.append(value)
    require(len(values) <= 1, f"Duplicate {key} in native.env")
    return values[0] if values else None


def upgrade_env(content):
    """Change only the explicit former body limit, retaining every other byte."""
    if env_value(content, BODY_LIMIT) != "65536":
        return content
    return re.sub(rb"(?m)^" + BODY_LIMIT.encode() + rb"=[^\r\n]*",
                  BODY_LIMIT.encode() + b"=327680", content)


def accounts_identity(content):
    """Require an Accounts-era native.env for app `waveform`: no retired settings, an app secret present."""
    retired = [key for key in RETIRED_KEYS if env_value(content, key) is not None]
    require(not retired, "native.env still has settings of the earlier sign-in system (" + ", ".join(retired)
            + "); run deploy/native/accounts_cutover.py once instead of upgrade.py")
    require((env_value(content, "WAVEFORM_APP_ID") or APP_ID) == APP_ID, "native.env names another app than waveform")
    require(env_value(content, "WAVEFORM_APP_SECRET"), "native.env has no WAVEFORM_APP_SECRET")


def accounts_preflight(content, timeout=20):
    """Prove Silicon Accounts accepts Waveform's app credentials before anything stops: introspecting a token that
    does not exist must answer exactly {"active": false}; wrong credentials answer 401 invalid_client."""
    base = (env_value(content, "ACCOUNTS_API_URL") or env_value(content, "ACCOUNTS_URL") or DEFAULT_ACCOUNTS_URL)
    app_id = env_value(content, "WAVEFORM_APP_ID") or APP_ID
    basic = base64.b64encode(f"{app_id}:{env_value(content, 'WAVEFORM_APP_SECRET')}".encode()).decode("ascii")
    request = urllib.request.Request(base.rstrip("/") + "/v1/oauth/introspect",
                                     data=urllib.parse.urlencode({"token": "waveform-deploy-preflight"}).encode(),
                                     headers={"authorization": "Basic " + basic,
                                              "content-type": "application/x-www-form-urlencoded"})
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            answer = json.loads(response.read() or b"null")
            require(response.status == 200 and answer == {"active": False},
                    f"Silicon Accounts answered the credential check unexpectedly ({response.status})")
    except urllib.error.HTTPError as error:
        raise RuntimeError(f"Silicon Accounts refused Waveform's app credentials ({error.code}); check "
                           "WAVEFORM_APP_SECRET and WAVEFORM_APP_ID") from None
    except (urllib.error.URLError, TimeoutError, OSError, ValueError) as error:
        raise RuntimeError(f"Silicon Accounts could not be reached at {base} for the credential check: {error}") from None


def backup_database(backup, bucket, region, kms_key_id=None):
    """Dump PostgreSQL into the backup directory (checked with pg_restore --list), upload it encrypted with an
    explicit SHA-256 checksum, and return (local dump, S3 URL)."""
    dump_path = backup / "postgres.dump"
    with dump_path.open("wb") as dump:
        run("docker", "exec", "waveform-postgres", "pg_dump", "-U", "postgres", "-d", "waveform", "-Fc", stdout=dump)
        dump.flush()
        os.fsync(dump.fileno())
    require(dump_path.stat().st_size > 0, "PostgreSQL backup is empty")
    with dump_path.open("rb") as dump:
        run("docker", "exec", "-i", "waveform-postgres", "pg_restore", "--list", stdin=dump, stdout=subprocess.DEVNULL)
    backup_key = f"backups/{backup.name}.dump"
    require(dump_path.stat().st_size < 5 * 1024**3, "Database backup exceeds single-object upload limit")
    checksum = base64.b64encode(bytes.fromhex(digest(dump_path))).decode("ascii")
    encryption = (["--server-side-encryption", "aws:kms", "--ssekms-key-id", kms_key_id]
                  if kms_key_id else ["--server-side-encryption", "AES256"])
    remote = json.loads(subprocess.check_output([
        "aws", "s3api", "put-object", "--bucket", bucket, "--key", backup_key,
        "--body", str(dump_path), "--checksum-sha256", checksum,
        "--region", region, "--output", "json", *encryption]))
    require(remote.get("ServerSideEncryption") in ("AES256", "aws:kms", "aws:kms:dsse")
            and remote.get("ChecksumSHA256") == checksum,
            "Encrypted remote database backup verification failed")
    return dump_path, f"s3://{bucket}/{backup_key}"


def atomic_write(path, data, mode):
    descriptor, name = tempfile.mkstemp(prefix="." + path.name + "-", dir=path.parent)
    temporary = Path(name)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
            os.fchmod(output.fileno(), mode)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def switch_current(current, target):
    temporary = current.with_name(".current-upgrade")
    require(not temporary.exists() and not temporary.is_symlink(),
            "Stale .current-upgrade entry; inspect the interrupted upgrade first")
    try:
        temporary.symlink_to(target, target_is_directory=True)
        os.replace(temporary, current)
    finally:
        temporary.unlink(missing_ok=True)


def ready(url, seconds=90):
    deadline = time.monotonic() + seconds
    # Do not send private readiness traffic through a host HTTP proxy.
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    while time.monotonic() < deadline:
        try:
            with opener.open(url, timeout=3) as response:
                if response.status == 200:
                    return
        except (urllib.error.URLError, TimeoutError, OSError):
            pass
        time.sleep(2)
    raise RuntimeError("API readiness deadline exceeded")


def validate_elf(path, static=False):
    with path.open("rb") as source:
        header = source.read(64)
        require(len(header) == 64 and header[:6] == b"\x7fELF\x02\x01",
                "Bundle contains a non-ELF64 little-endian executable")
        require(struct.unpack_from("<H", header, 18)[0] == 183,
                "Bundle executable architecture is not AArch64")
        if static:
            offset = struct.unpack_from("<Q", header, 32)[0]
            size, count = struct.unpack_from("<HH", header, 54)
            require(size >= 56 and 0 < count < 1024, "Invalid ELF program headers")
            source.seek(offset)
            for _ in range(count):
                program_header = source.read(size)
                require(len(program_header) == size, "Truncated ELF program header")
                require(struct.unpack_from("<I", program_header)[0] != 3,
                        "Backend must be static; an ELF interpreter was found")


def safe_name(name):
    path = PurePosixPath(name)
    require(name and not path.is_absolute() and ".." not in path.parts
            and str(path) == name and "\\" not in name,
            "Invalid archive or checksum path")
    return path


def stage(archive, release, revision):
    require(not release.exists(), "Release already staged; inspect it before retrying")
    release.mkdir(mode=0o755)
    try:
        with tarfile.open(archive, "r:gz") as package:
            names = set()
            members = package.getmembers()
            for member in members:
                name = str(safe_name(member.name.rstrip("/")))
                require(member.isfile() or member.isdir(), "Archive links and special files are forbidden")
                require(name not in names, "Duplicate archive entry")
                names.add(name)
            for member in members:
                destination = release / member.name
                if member.isdir():
                    destination.mkdir(parents=True, exist_ok=True, mode=0o755)
                else:
                    destination.parent.mkdir(parents=True, exist_ok=True, mode=0o755)
                    with package.extractfile(member) as source, destination.open("xb") as output:
                        shutil.copyfileobj(source, output)
                    destination.chmod(0o755 if member.mode & 0o111 else 0o644)
        manifest = json.loads((release / "build.json").read_text())
        require(manifest.get("source_revision") == revision, "Bundle source revision mismatch")
        require(manifest.get("app_id") == "waveform"
                and manifest.get("architecture") == "aarch64"
                and manifest.get("backend_linkage") == "static-musl", "Unsupported bundle identity")
        expected = set()
        for line in (release / "SHA256SUMS").read_text().splitlines():
            match = re.fullmatch(r"([0-9a-f]{64})  (.+)", line)
            require(match is not None, "Malformed bundle checksum entry")
            checksum, name = match.groups()
            safe_name(name)
            require(name not in expected and name != "SHA256SUMS", "Duplicate or recursive checksum")
            require(digest(release / name) == checksum, "Bundle member checksum mismatch")
            expected.add(name)
        actual = {str(path.relative_to(release)) for path in release.rglob("*") if path.is_file()}
        require(actual == expected | {"SHA256SUMS"}, "Bundle checksum coverage mismatch")
        for name in ("build.json", "prepare.py", "waveform-api.service", "bin/ffmpeg", "bin/ffmpeg.real", "bin/waveform-api"):
            require(name in expected, "Required bundle member is missing")
        for directory in [release, *(p for p in release.rglob("*") if p.is_dir())]:
            directory.chmod(0o755)
        validate_elf(release / "bin/waveform-api", static=True)
        validate_elf(release / "bin/ffmpeg.real")
        run(str(release / "bin/ffmpeg"), "-v", "error", "-f", "lavfi", "-i",
            "sine=frequency=440:duration=0.1", "-f", "null", "-")
    except BaseException:
        shutil.rmtree(release)
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--source-revision", required=True)
    parser.add_argument("--backup-bucket", required=True)
    parser.add_argument("--backup-kms-key-id")
    parser.add_argument("--region", default="us-east-1")
    parser.add_argument("--public-ready-url", default="https://backend.waveform.teamofsilicons.com/health/ready")
    parser.add_argument("--skip-accounts-check", action="store_true",
                        help="do not check Waveform's app credentials with Silicon Accounts first (an outage there)")
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
        upgrade(args)


def upgrade(args):
    require(digest(args.archive) == args.sha256, "Archive checksum mismatch")
    current = Path("/opt/waveform/current")
    releases = Path("/opt/waveform/releases")
    env_path = Path("/etc/waveform/native.env")
    unit = Path("/etc/systemd/system/waveform-api.service")
    require(current.is_symlink(), "An existing native current symlink is required")
    old_link = os.readlink(current)
    old_release = current.resolve(strict=True)
    require(old_release.parent == releases.resolve(), "Current release is outside the releases directory")
    require(env_path.is_file() and not env_path.is_symlink(), "A regular native.env is required")
    require(unit.is_file() and not unit.is_symlink(), "A regular native service unit is required")
    old_env = env_path.read_bytes()
    accounts_identity(old_env)
    bind = env_value(old_env, "WAVEFORM_BIND_ADDR")
    require(bind and bind.count(":") == 1, "An explicit private IPv4 bind is required")
    host, port = bind.rsplit(":", 1)
    require(ipaddress.ip_address(host).is_private and port.isdigit()
            and 0 < int(port) <= 65535, "Native bind must remain private")
    require(env_value(old_env, "WAVEFORM_FFMPEG_PATH") == "/opt/waveform/current/bin/ffmpeg",
            "Unexpected FFmpeg path; review the existing native environment")
    local_ready = f"http://{bind}/health/ready"
    if not args.skip_accounts_check:
        accounts_preflight(old_env)
    run("systemctl", "is-active", "--quiet", "waveform-api")
    ready(local_ready, 15)
    ready(args.public_ready_url, 15)
    release = releases / args.source_revision
    stage(args.archive, release, args.source_revision)
    run("systemd-analyze", "verify", str(release / "waveform-api.service"))
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    backup = Path("/var/backups/waveform") / f"upgrade-{stamp}-{args.source_revision[:12]}"
    backup.mkdir(parents=True, mode=0o700)
    shutil.copyfile(env_path, backup / "native.env")
    shutil.copyfile(unit, backup / "waveform-api.service")
    (backup / "current-link.txt").write_text(old_link + "\n")
    receipt_path = Path("/etc/waveform/native-release.json")
    if receipt_path.is_file():
        shutil.copyfile(receipt_path, backup / "native-release.json")
    _, database_backup = backup_database(backup, args.backup_bucket, args.region, args.backup_kms_key_id)
    new_env = upgrade_env(old_env)
    old_unit = (backup / "waveform-api.service").read_bytes()
    receipt = {"source_revision": args.source_revision, "archive_sha256": args.sha256,
               "release": str(release), "previous_release": str(old_release), "backup": str(backup),
               "database_backup_s3": database_backup,
               "runtime": "native-systemd", "deployed_at": stamp, "bind": bind,
               "json_body_limit_updated": new_env != old_env}
    try:
        run("systemctl", "stop", "waveform-api")
        atomic_write(env_path, new_env, 0o600)
        atomic_write(unit, (release / "waveform-api.service").read_bytes(), 0o644)
        switch_current(current, str(release))
        run("systemctl", "daemon-reload")
        run("systemctl", "reset-failed", "waveform-api")
        run("systemctl", "start", "waveform-api")
        ready(local_ready)
        ready(args.public_ready_url, 30)
        atomic_write(receipt_path, (json.dumps(receipt, indent=2) + "\n").encode(), 0o600)
    except BaseException:
        subprocess.run(["systemctl", "stop", "waveform-api"], check=False)
        switch_current(current, old_link)
        atomic_write(env_path, old_env, 0o600)
        atomic_write(unit, old_unit, 0o644)
        run("systemctl", "daemon-reload")
        run("systemctl", "reset-failed", "waveform-api")
        run("systemctl", "start", "waveform-api")
        ready(local_ready)
        ready(args.public_ready_url, 30)
        print("Upgrade failed; previous native release and configuration restored.")
        raise
    print(json.dumps(receipt, indent=2))


if __name__ == "__main__":
    main()
