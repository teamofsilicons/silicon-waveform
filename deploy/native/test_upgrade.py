#!/usr/bin/env python3
"""Local-only regression checks for the native upgrade transaction."""
import base64
import hashlib
import importlib.util
import io
import json
from pathlib import Path
from types import SimpleNamespace
import tarfile
import tempfile
import unittest
import urllib.error
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("upgrade", Path(__file__).with_name("upgrade.py"))
upgrade = importlib.util.module_from_spec(spec)
spec.loader.exec_module(upgrade)


class NativeUpgradeTests(unittest.TestCase):
    def test_environment_changes_only_former_body_limit(self):
        original = b"# private\nWAVEFORM_JSON_BODY_LIMIT_BYTES=65536\nSECRET=a=b '$x'\nDB=tls\n"
        self.assertEqual(upgrade.upgrade_env(original), original.replace(b"65536", b"327680"))

    def test_absent_and_custom_body_limits_are_preserved(self):
        for original in (b"SECRET=opaque\n", b"WAVEFORM_JSON_BODY_LIMIT_BYTES=400000\nSECRET=opaque\n"):
            self.assertEqual(upgrade.upgrade_env(original), original)

    def test_quoted_old_body_limit_and_line_endings(self):
        self.assertEqual(upgrade.upgrade_env(b'WAVEFORM_JSON_BODY_LIMIT_BYTES="65536"\r\nSECRET=untouched\r\n'),
                         b"WAVEFORM_JSON_BODY_LIMIT_BYTES=327680\r\nSECRET=untouched\r\n")

    def test_ambiguous_environment_is_rejected(self):
        with self.assertRaisesRegex(RuntimeError, "Duplicate"):
            upgrade.upgrade_env(b"WAVEFORM_JSON_BODY_LIMIT_BYTES=65536\nWAVEFORM_JSON_BODY_LIMIT_BYTES=400000\n")

    def test_archive_path_validation(self):
        for name in ("../secret", "/tmp/secret", "bin/../../secret", "bin//api", "bin/./api", "bin\\api"):
            with self.subTest(name=name), self.assertRaises(RuntimeError):
                upgrade.safe_name(name)
        self.assertEqual(str(upgrade.safe_name("bin/waveform-api")), "bin/waveform-api")

    def test_archive_link_is_rejected_and_stage_removed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "bundle.tar.gz"
            with tarfile.open(archive, "w:gz") as package:
                member = tarfile.TarInfo("bin/api")
                member.type = tarfile.SYMTYPE
                member.linkname = "/etc/passwd"
                package.addfile(member)
            release = root / "release"
            with self.assertRaisesRegex(RuntimeError, "links"):
                upgrade.stage(archive, release, "a" * 40)
            self.assertFalse(release.exists())

    def test_manifest_revision_mismatch_removes_stage(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "bundle.tar.gz"
            body = json.dumps({"source_revision": "b" * 40}).encode()
            with tarfile.open(archive, "w:gz") as package:
                member = tarfile.TarInfo("build.json")
                member.size = len(body)
                package.addfile(member, io.BytesIO(body))
            with self.assertRaisesRegex(RuntimeError, "revision mismatch"):
                upgrade.stage(archive, root / "release", "a" * 40)
            self.assertFalse((root / "release").exists())

    def test_public_readiness_failure_restores_previous_release_and_private_files(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            def local_path(value):
                value = str(value)
                if value.startswith(("/opt/waveform/", "/etc/waveform/", "/etc/systemd/", "/var/backups/waveform")):
                    return root / value.lstrip("/")
                return Path(value)

            releases = local_path("/opt/waveform/releases")
            previous = releases / ("a" * 40)
            previous.mkdir(parents=True)
            current = local_path("/opt/waveform/current")
            current.symlink_to(previous)
            env = local_path("/etc/waveform/native.env")
            env.parent.mkdir(parents=True)
            original_env = (b"WAVEFORM_APP_SECRET=sa_app_preserve-exactly\nWAVEFORM_BIND_ADDR=172.18.0.1:8080\n"
                            b"WAVEFORM_FFMPEG_PATH=/opt/waveform/current/bin/ffmpeg\n"
                            b"WAVEFORM_JSON_BODY_LIMIT_BYTES=65536\nSECRET=preserve-exactly\n")
            env.write_bytes(original_env)
            unit = local_path("/etc/systemd/system/waveform-api.service")
            unit.parent.mkdir(parents=True)
            unit.write_bytes(b"old service\n")
            receipt = env.with_name("native-release.json")
            receipt.write_bytes(b"old receipt\n")
            archive = root / "archive"
            archive.write_bytes(b"verified archive")
            args = SimpleNamespace(archive=archive, sha256=hashlib.sha256(archive.read_bytes()).hexdigest(),
                                   source_revision="b" * 40, backup_bucket="backup-bucket", region="us-east-1",
                                   backup_kms_key_id=None, public_ready_url="https://example.test/health/ready",
                                   skip_accounts_check=False)

            def stage_bundle(archive, release, revision):
                release.mkdir()
                (release / "waveform-api.service").write_bytes(b"new service\n")

            def run_command(*args, **kwargs):
                if "pg_dump" in args:
                    kwargs["stdout"].write(b"database dump")

            ready_results = [None, None, None, RuntimeError("public readiness failed"), None, None]
            remote = json.dumps({"ServerSideEncryption": "AES256", "ChecksumSHA256": base64.b64encode(hashlib.sha256(b"database dump").digest()).decode()}).encode()
            with patch.object(upgrade, "Path", side_effect=local_path), \
                 patch.object(upgrade, "accounts_preflight") as preflight, \
                 patch.object(upgrade, "stage", side_effect=stage_bundle), \
                 patch.object(upgrade, "run", side_effect=run_command), \
                 patch.object(upgrade.subprocess, "run"), \
                 patch.object(upgrade.subprocess, "check_output", return_value=remote), \
                 patch.object(upgrade, "ready", side_effect=ready_results), \
                 patch("builtins.print"):
                with self.assertRaisesRegex(RuntimeError, "public readiness failed"):
                    upgrade.upgrade(args)
            preflight.assert_called_once_with(original_env)
            self.assertEqual(current.resolve(), previous.resolve())
            self.assertEqual(env.read_bytes(), original_env)
            self.assertEqual(unit.read_bytes(), b"old service\n")
            self.assertEqual(receipt.read_bytes(), b"old receipt\n")



class FakeResponse(io.BytesIO):
    def __init__(self, status, body):
        super().__init__(body)
        self.status = status

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


class AccountsIdentityTests(unittest.TestCase):
    ENV = b"ACCOUNTS_URL=https://accounts.example\nWAVEFORM_APP_SECRET=sa_app_secret\nSECRET=x\n"

    def test_an_accounts_era_environment_passes(self):
        upgrade.accounts_identity(self.ENV)
        upgrade.accounts_identity(self.ENV + b"WAVEFORM_APP_ID=waveform\n")

    def test_earlier_settings_send_the_operator_to_the_cutover(self):
        for key in (b"WAVEFORM_IAM_APP_ID=waveform", b"WAVEFORM_BRIEFCASE_AUDIENCE=briefcase",
                    b"WAVEFORM_HONEYCOMB_SERVICE_TOKEN=x"):
            with self.subTest(key=key), self.assertRaisesRegex(RuntimeError, "accounts_cutover.py"):
                upgrade.accounts_identity(self.ENV + key + b"\n")

    def test_another_app_or_a_missing_secret_is_refused(self):
        with self.assertRaisesRegex(RuntimeError, "another app"):
            upgrade.accounts_identity(self.ENV + b"WAVEFORM_APP_ID=remind\n")
        with self.assertRaisesRegex(RuntimeError, "WAVEFORM_APP_SECRET"):
            upgrade.accounts_identity(b"ACCOUNTS_URL=https://accounts.example\n")


class AccountsPreflightTests(unittest.TestCase):
    def test_credentials_never_follow_redirects(self):
        import http.server
        import threading
        paths = []
        class Redirect(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                paths.append(self.path)
                self.send_response(302)
                self.send_header("Location", "/unexpected-target")
                self.end_headers()
            def do_GET(self):
                paths.append(self.path)
                self.send_response(200)
                self.end_headers()
            def log_message(self, *args):
                pass
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Redirect)
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            env = f"ACCOUNTS_URL=http://127.0.0.1:{server.server_port}\nWAVEFORM_APP_SECRET=private\n".encode()
            with self.assertRaisesRegex(RuntimeError, "302"):
                upgrade.accounts_preflight(env)
            self.assertEqual(paths, ["/v1/oauth/introspect"])
        finally:
            server.shutdown()
            server.server_close()
            worker.join()

    ENV = b"ACCOUNTS_URL=https://accounts.example\nACCOUNTS_API_URL=https://api.accounts.example/\nWAVEFORM_APP_SECRET=sa_app_s3cret\n"

    def test_inactive_answer_proves_the_credentials(self):
        with patch.object(upgrade, "accounts_open", return_value=FakeResponse(200, b'{"active":false}')) as call:
            upgrade.accounts_preflight(self.ENV)
        request = call.call_args.args[0]
        self.assertEqual(request.full_url, "https://api.accounts.example/v1/oauth/introspect")
        self.assertEqual(request.get_header("Authorization"),
                         "Basic " + base64.b64encode(b"waveform:sa_app_s3cret").decode())
        self.assertNotIn(b"sa_app_s3cret", request.data)

    def test_refused_credentials_unexpected_answers_and_outages_stop_the_deploy(self):
        refused = urllib.error.HTTPError("https://api.accounts.example", 401, "Unauthorized", {}, io.BytesIO(b"{}"))
        cases = [(refused, "refused Waveform's app credentials \\(401\\)"),
                 (FakeResponse(200, b'{"active":true}'), "unexpectedly"),
                 (urllib.error.URLError("connection refused"), "could not be reached")]
        for outcome, message in cases:
            with self.subTest(message=message), \
                 patch.object(upgrade, "accounts_open", side_effect=[outcome] if isinstance(outcome, Exception) else None,
                              return_value=outcome), \
                 self.assertRaisesRegex(RuntimeError, message):
                upgrade.accounts_preflight(self.ENV)


if __name__ == "__main__":
    unittest.main()
