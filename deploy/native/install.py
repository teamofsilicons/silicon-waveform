#!/usr/bin/env python3
"""Install a verified native bundle on the existing Waveform host (run as root)."""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import pwd
import shutil
import subprocess
import tarfile
import time
import urllib.error
import urllib.parse
import urllib.request


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def env_file(path):
    return dict(line.split("=", 1) for line in path.read_text().splitlines()
                if line and not line.startswith("#") and "=" in line)


def write_env(path, values):
    assert all(not any(c in str(v) for c in "\r\n\x00") for v in values.values())
    path.write_text("".join(f"{key}={value}\n" for key, value in values.items()))
    path.chmod(0o600)


def ready(url, seconds=90):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=3) as response:
                if response.status == 200:
                    return
        except (urllib.error.URLError, TimeoutError):
            pass
        time.sleep(2)
    raise RuntimeError("Native API readiness deadline exceeded")


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--archive", required=True, type=Path)
    p.add_argument("--sha256", required=True)
    p.add_argument("--source-revision", required=True)
    p.add_argument("--secret-arn", required=True)
    p.add_argument("--backup-bucket", required=True)
    p.add_argument("--region", default="us-east-1")
    a = p.parse_args()
    assert os.geteuid() == 0, "Run as root"
    os.umask(0o077)
    assert hashlib.sha256(a.archive.read_bytes()).hexdigest() == a.sha256, "Archive checksum mismatch"
    config = Path("/etc/waveform")
    old_env = env_file(config / "api.env")
    assert old_env["WAVEFORM_IAM_APP_ID"] == "tos>waveform"
    secret = json.loads(json.loads(subprocess.check_output([
        "aws", "secretsmanager", "get-secret-value", "--region", a.region,
        "--secret-id", a.secret_arn, "--output", "json"]))["SecretString"])
    new_key = secret["WAVEFORM_IAM_APP_SECRET"]
    # Read-only credential preflight; the deliberately unknown token is inactive.
    basic = base64.b64encode(("tos>waveform:" + new_key).encode()).decode()
    request = urllib.request.Request(old_env["WAVEFORM_IAM_BASE_URL"].rstrip("/") + "/api/v1/oauth/introspect",
        data=urllib.parse.urlencode({"token": "oat_" + "A" * 43}).encode(),
        headers={"authorization": "Basic " + basic, "content-type": "application/x-www-form-urlencoded"})
    with urllib.request.urlopen(request, timeout=20) as response:
        assert response.status == 200, "IAM rejected configured application credential"
    release = Path("/opt/waveform/releases") / a.source_revision
    assert not release.exists(), "Release already staged; inspect existing state before retrying"
    release.mkdir(parents=True, mode=0o755)
    release.parent.chmod(0o755)
    release.parent.parent.chmod(0o755)
    with tarfile.open(a.archive, "r:gz") as package:
        names = set()
        for member in package.getmembers():
            name = Path(member.name)
            assert not name.is_absolute() and ".." not in name.parts
            assert member.isfile() or member.isdir(), "Links and special entries are forbidden"
            assert member.name not in names, "Duplicate archive entry"
            names.add(member.name)
        package.extractall(release)
    manifest = json.loads((release / "build.json").read_text())
    assert manifest["source_revision"] == a.source_revision
    assert manifest["app_id"] == "tos>waveform" and manifest["architecture"] == "aarch64"
    expected = set()
    for line in (release / "SHA256SUMS").read_text().splitlines():
        digest, name = line.split("  ", 1)
        assert ".." not in Path(name).parts and not Path(name).is_absolute()
        assert hashlib.sha256((release / name).read_bytes()).hexdigest() == digest, name
        expected.add(name)
    assert {str(f.relative_to(release)) for f in release.rglob("*") if f.is_file()} == expected | {"SHA256SUMS"}
    for directory in [release, *[f for f in release.rglob("*") if f.is_dir()]]:
        directory.chmod(0o755)
    run(str(release / "bin/ffmpeg"), "-v", "error", "-f", "lavfi", "-i", "sine=frequency=440:duration=0.1", "-f", "null", "-")
    try:
        pwd.getpwnam("waveform")
    except KeyError:
        run("useradd", "--system", "--home-dir", "/var/lib/waveform-native", "--shell", "/sbin/nologin", "waveform")
    account = pwd.getpwnam("waveform")
    runtime = Path("/run/waveform")
    runtime.mkdir(exist_ok=True, mode=0o750)
    runtime.chmod(0o750)
    os.chown(runtime, account.pw_uid, account.pw_gid)
    run("python3", str(release / "prepare.py"))
    gateway = json.loads(subprocess.check_output(["docker", "network", "inspect", "waveform"]))[0]["IPAM"]["Config"][0]["Gateway"]
    assert gateway.startswith("172."), "Unexpected private bridge; review bind address"
    native_env = dict(old_env)
    native_env["WAVEFORM_IAM_APP_SECRET"] = new_key
    native_env["WAVEFORM_BIND_ADDR"] = gateway + ":8080"
    native_env["WAVEFORM_FFMPEG_PATH"] = "/opt/waveform/current/bin/ffmpeg"
    native_env["SPACE_STATION_HOME"] = "/var/lib/waveform-native/telemetry"
    database = urllib.parse.urlsplit(native_env["WAVEFORM_DATABASE_URL"])
    assert database.hostname == "postgres", "Unexpected database host"
    query = dict(urllib.parse.parse_qsl(database.query))
    assert query.get("sslmode") == "verify-full", "Database TLS must remain verified"
    query["sslrootcert"] = "/run/waveform/db-ca.crt"
    native_env["WAVEFORM_DATABASE_URL"] = urllib.parse.urlunsplit(database._replace(query=urllib.parse.urlencode(query)))
    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    backup = Path("/var/backups/waveform") / ("native-" + stamp)
    backup.mkdir(parents=True)
    unit = Path("/etc/systemd/system/waveform-api.service")
    shutil.copyfile(unit, backup / "waveform-api.service")
    shutil.copyfile(config / "api.env", backup / "api.env")
    caddy_path = config / "Caddyfile"
    caddy = caddy_path.read_text()
    assert caddy.count("waveform-api:8080") == 1, "Unexpected proxy config; refusing broad replacement"
    shutil.copyfile(caddy_path, backup / "Caddyfile")
    with (backup / "postgres.dump").open("wb") as dump:
        run("docker", "exec", "waveform-postgres", "pg_dump", "-U", "postgres", "-d", "waveform", "-Fc", stdout=dump)
    assert (backup / "postgres.dump").stat().st_size > 0
    run("aws", "s3", "cp", str(backup / "postgres.dump"),
        f"s3://{a.backup_bucket}/backups/native-{stamp}.dump", "--region", a.region, "--only-show-errors")
    # Retain the valid credential even if the native rollout must revert to the old image.
    old_env["WAVEFORM_IAM_APP_SECRET"] = new_key
    write_env(config / "api.env", old_env)
    write_env(config / "native.env", native_env)
    current = Path("/opt/waveform/current")
    assert not current.exists(), "Existing native deployment requires an explicit upgrade workflow"
    current.symlink_to(release, target_is_directory=True)
    maintenance = caddy.replace("reverse_proxy waveform-api:8080", 'header Retry-After 60\n    respond "Service temporarily unavailable" 503')

    def proxy(text):
        caddy_path.write_text(text)  # Keep the bind-mounted inode.
        run("docker", "exec", "waveform-proxy", "caddy", "validate", "--config", "/etc/caddy/Caddyfile", "--adapter", "caddyfile", stdout=subprocess.DEVNULL)
        run("docker", "exec", "waveform-proxy", "caddy", "reload", "--config", "/etc/caddy/Caddyfile", "--adapter", "caddyfile", stdout=subprocess.DEVNULL)

    try:
        proxy(maintenance)
        run("systemctl", "stop", "waveform-api")
        shutil.copyfile(release / "waveform-api.service", unit)
        unit.chmod(0o644)
        run("systemd-analyze", "verify", str(unit))
        run("systemctl", "daemon-reload")
        run("systemctl", "start", "waveform-api")
        ready(f"http://{gateway}:8080/health/ready")
        proxy(caddy.replace("waveform-api:8080", gateway + ":8080"))
        ready("https://backend.waveform.teamofsilicons.com/health/ready", 30)
    except BaseException:
        subprocess.run(["systemctl", "stop", "waveform-api"], check=False)
        shutil.copyfile(backup / "waveform-api.service", unit)
        run("systemctl", "daemon-reload")
        run("systemctl", "start", "waveform-api")
        proxy(caddy)
        raise
    receipt = {"source_revision": a.source_revision, "archive_sha256": a.sha256,
               "release": str(release), "backup": str(backup), "runtime": "native-systemd",
               "deployed_at": stamp, "bind": gateway + ":8080"}
    (config / "native-release.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(receipt, indent=2))


if __name__ == "__main__":
    main()
