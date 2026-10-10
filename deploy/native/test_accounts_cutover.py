#!/usr/bin/env python3
"""Local-only checks for accounts_cutover.py: settings, the native.env rewrite, and the cutover transaction."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("accounts_cutover", Path(__file__).with_name("accounts_cutover.py"))
cutover = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cutover)
upgrade = cutover.upgrade

SECRET = "sa_app_" + "s" * 49
WEBHOOK = "whsec_" + "w" * 43
EARLIER = (b"# private\nWAVEFORM_ENVIRONMENT=production\nWAVEFORM_BIND_ADDR=172.18.0.1:8080\n"
           b"WAVEFORM_IAM_BASE_URL=https://backend.iam.example\nWAVEFORM_IAM_APP_ID=waveform\n"
           b"WAVEFORM_IAM_APP_SECRET=old-secret\nWAVEFORM_IAM_AUDIENCE=waveform\n"
           b"WAVEFORM_BRIEFCASE_APP_ID=waveform\nWAVEFORM_BRIEFCASE_AUDIENCE=briefcase\n"
           b"WAVEFORM_BRIEFCASE_CDN_ORIGIN=https://briefcase.example\nWAVEFORM_WEBHOOK_SIGNING_SECRET=old-webhook\n"
           b"WAVEFORM_WEBHOOK_PUBLIC_URL=https://backend.waveform.example/webhook/\nSILICON_IAM_CLIENT_AUTO_UPDATE=false\n"
           b"BRIEFCASE_CLIENT_AUTO_UPDATE=false\nWAVEFORM_ENCRYPTION_KEY=" + b"e" * 64 + b"\n"
           b"WAVEFORM_DATABASE_URL=postgres://waveform:p%40ss@postgres/waveform?sslmode=verify-full\n"
           b"WAVEFORM_JSON_BODY_LIMIT_BYTES=65536\nWAVEFORM_GEMINI_API_KEY=keep 'exactly'=\n")


class AccountsEnvFileTests(unittest.TestCase):
    def write(self, text, mode=0o600):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        path = Path(directory.name) / "waveform-accounts.env"
        path.write_text(text)
        path.chmod(mode)
        return path

    def test_a_root_only_file_with_the_required_secrets_is_read(self):
        values = cutover.read_accounts_env(self.write(
            f"# Waveform at Silicon Accounts\nWAVEFORM_APP_SECRET={SECRET}\nWAVEFORM_ACCOUNTS_WEBHOOK_SECRET='{WEBHOOK}'\n"
            "WAVEFORM_PROOF_ISSUERS=waveform.tts=interface;waveform.stt=interface\n"))
        self.assertEqual(values, {"WAVEFORM_APP_SECRET": SECRET, "WAVEFORM_ACCOUNTS_WEBHOOK_SECRET": WEBHOOK,
                                  "WAVEFORM_PROOF_ISSUERS": "waveform.tts=interface;waveform.stt=interface"})

    def test_each_unsafe_or_wrong_file_is_refused_with_its_reason(self):
        good = f"WAVEFORM_APP_SECRET={SECRET}\nWAVEFORM_ACCOUNTS_WEBHOOK_SECRET={WEBHOOK}\n"
        cases = [
            (good, 0o644, "mode 0600"),
            (good + "WAVEFORM_IAM_APP_ID=waveform\n", 0o600, "not a Silicon Accounts setting"),
            (good + f"WAVEFORM_APP_SECRET={SECRET}\n", 0o600, "set twice"),
            (f"WAVEFORM_APP_SECRET={SECRET}\n", 0o600, "must set WAVEFORM_ACCOUNTS_WEBHOOK_SECRET"),
            (good.replace(SECRET, "sa_app_short"), 0o600, "at least 32 characters"),
            (good.replace(WEBHOOK, "w" * 40), 0o600, "whsec_"),
            (good + "ACCOUNTS_URL=http://accounts.example\n", 0o600, "https://"),
            (good + "WAVEFORM_APP_ID=remind\n", 0o600, "must be waveform"),
            (good + "WAVEFORM_BRIEFCASE_APP_ID=waveform\n", 0o600, "Briefcase's app id"),
            (good + "WAVEFORM_PROOF_ISSUERS=waveform.tts=interface waveform.stt=dm\n", 0o600, "one word"),
            ("WAVEFORM_APP_SECRET\n", 0o600, "expected KEY=value"),
        ]
        for text, mode, message in cases:
            with self.subTest(message=message), self.assertRaisesRegex(RuntimeError, message):
                cutover.read_accounts_env(self.write(text, mode))


class NativeEnvRewriteTests(unittest.TestCase):
    ACCOUNTS = {"WAVEFORM_APP_SECRET": SECRET, "WAVEFORM_ACCOUNTS_WEBHOOK_SECRET": WEBHOOK,
                "WAVEFORM_PROOF_ISSUERS": "waveform.tts=interface;waveform.stt=interface"}

    def test_earlier_settings_go_new_ones_come_and_everything_else_stays_byte_for_byte(self):
        new = cutover.cutover_env(EARLIER, self.ACCOUNTS)
        for retired in upgrade.RETIRED_KEYS:
            self.assertIsNone(upgrade.env_value(new, retired), retired)
        self.assertEqual(upgrade.env_value(new, "WAVEFORM_BRIEFCASE_APP_ID"), "briefcase")
        self.assertEqual(upgrade.env_value(new, "ACCOUNTS_URL"), "https://accounts.teamofsilicons.com")
        self.assertEqual(upgrade.env_value(new, "WAVEFORM_APP_SECRET"), SECRET)
        self.assertEqual(upgrade.env_value(new, "WAVEFORM_JSON_BODY_LIMIT_BYTES"), "327680")
        kept = [line for line in EARLIER.splitlines(keepends=True)
                if not line.startswith((b"WAVEFORM_IAM", b"WAVEFORM_BRIEFCASE", b"WAVEFORM_WEBHOOK", b"SILICON_IAM",
                                        b"BRIEFCASE_CLIENT", b"WAVEFORM_JSON_BODY"))]
        self.assertEqual([line for line in new.splitlines(keepends=True) if line in kept], kept)
        upgrade.accounts_identity(new)

    def test_crlf_files_keep_their_line_endings(self):
        new = cutover.cutover_env(EARLIER.replace(b"\n", b"\r\n"), self.ACCOUNTS)
        self.assertNotIn(b"\n", new.replace(b"\r\n", b""))

    def test_a_file_set_value_replaces_an_existing_line_in_place(self):
        earlier = EARLIER + b"ACCOUNTS_URL=https://old.example\n"
        new = cutover.cutover_env(earlier, dict(self.ACCOUNTS, ACCOUNTS_URL="https://accounts.example"))
        self.assertEqual(new.count(b"ACCOUNTS_URL="), 1)
        self.assertEqual(upgrade.env_value(new, "ACCOUNTS_URL"), "https://accounts.example")

    def test_ambiguous_or_incomplete_environments_are_refused(self):
        with self.assertRaisesRegex(RuntimeError, "twice"):
            cutover.cutover_env(EARLIER + b"WAVEFORM_BRIEFCASE_APP_ID=x\n", self.ACCOUNTS)
        with self.assertRaisesRegex(RuntimeError, "WAVEFORM_ENCRYPTION_KEY"):
            cutover.cutover_env(b"WAVEFORM_IAM_APP_ID=waveform\n", self.ACCOUNTS)

    def test_only_an_earlier_environment_is_cut_over(self):
        cutover.earlier_env(EARLIER)
        with self.assertRaisesRegex(RuntimeError, "upgrade.py"):
            cutover.earlier_env(cutover.cutover_env(EARLIER, self.ACCOUNTS))


class CutoverTransactionTests(unittest.TestCase):
    """The flow with every host command replaced: what runs, in which order, and what is put back on failure."""

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.releases = self.path("/opt/waveform/releases")
        self.previous = self.releases / ("a" * 40)
        self.previous.mkdir(parents=True)
        self.current = self.path("/opt/waveform/current")
        self.current.symlink_to(self.previous)
        self.env = self.path("/etc/waveform/native.env")
        self.env.parent.mkdir(parents=True)
        self.env.write_bytes(EARLIER)
        self.unit = self.path("/etc/systemd/system/waveform-api.service")
        self.unit.parent.mkdir(parents=True)
        self.unit.write_bytes(b"old unit\n")
        self.accounts_env = self.root / "accounts.env"
        self.accounts_env.write_text(f"WAVEFORM_APP_SECRET={SECRET}\nWAVEFORM_ACCOUNTS_WEBHOOK_SECRET={WEBHOOK}\n"
                                     "WAVEFORM_PROOF_ISSUERS=waveform.tts=interface\n")
        self.accounts_env.chmod(0o600)
        archive = self.root / "bundle.tar.gz"
        archive.write_bytes(b"bundle")
        self.args = SimpleNamespace(archive=archive, sha256=hashlib.sha256(b"bundle").hexdigest(),
                                    source_revision="b" * 40, backup_bucket="backups", backup_kms_key_id=None,
                                    region="us-east-1", accounts_env=self.accounts_env, mapping=None,
                                    public_ready_url="https://backend.example/health/ready")
        self.events = []

    def path(self, value):
        value = str(value)
        if value.startswith(("/opt/waveform", "/etc/waveform", "/etc/systemd", "/var/backups/waveform", "/run/waveform")):
            return self.root / value.lstrip("/")
        return Path(value)

    def run_cutover(self, fail_at=None, webhook="delivered"):
        def step(name, result=None):
            def action(*args, **kwargs):
                label = name if name != "run" else "run:" + " ".join(map(str, args[:2]))
                self.events.append(label)
                if fail_at == label:
                    raise RuntimeError(f"{label} failed")
                return result
            return action

        def stage(archive, release, revision):
            release.mkdir()
            (release / "waveform-api.service").write_bytes(b"new unit\n")

        def backup(directory, *args):
            self.events.append("backup")
            dump = directory / "postgres.dump"
            dump.write_bytes(b"dump")
            return dump, "s3://backups/backups/x.dump"

        def operator(release, *arguments):
            self.events.append("operator:" + arguments[0])
            failed = fail_at == "operator:" + arguments[0]
            return SimpleNamespace(returncode=1 if failed else 0, stdout="", stderr="boom" if failed else "")

        readiness = iter(range(100))

        def ready(url, seconds=90):
            label = f"ready:{next(readiness)}"
            self.events.append(label)
            if fail_at == label:
                raise RuntimeError("readiness failed")

        with patch.object(cutover, "Path", side_effect=self.path), \
             patch.object(upgrade, "accounts_preflight", side_effect=step("preflight")), \
             patch.object(upgrade, "stage", side_effect=stage), \
             patch.object(upgrade, "run", side_effect=step("run")), \
             patch.object(upgrade, "ready", side_effect=ready), \
             patch.object(upgrade, "backup_database", side_effect=backup), \
             patch.object(cutover, "psql", return_value="15"), \
             patch.object(cutover, "prepare_runtime", side_effect=step("prepare")), \
             patch.object(cutover, "operator", side_effect=operator), \
             patch.object(cutover, "restore_database", side_effect=step("restore", "waveform_cutover_x")), \
             patch.object(cutover, "test_webhook", side_effect=step("webhook", webhook)), \
             patch.object(cutover.subprocess, "run"), patch("builtins.print"):
            return cutover.cutover(self.args)

    def test_success_migrates_before_start_and_records_the_receipt(self):
        self.assertEqual(self.run_cutover(), 0)
        order = [event for event in self.events if not event.startswith("run:systemctl daemon")]
        self.assertLess(order.index("backup"), order.index("run:systemctl stop"))
        self.assertLess(order.index("run:systemctl stop"), order.index("operator:migrate"))
        self.assertLess(order.index("operator:migrate"), order.index("run:systemctl start"))
        self.assertNotIn("restore", self.events)
        self.assertEqual(self.current.resolve(), (self.releases / ("b" * 40)).resolve())
        self.assertEqual(upgrade.env_value(self.env.read_bytes(), "WAVEFORM_APP_SECRET"), SECRET)
        self.assertEqual(self.unit.read_bytes(), b"new unit\n")
        receipt = json.loads(self.path("/etc/waveform/native-release.json").read_text())
        self.assertEqual((receipt["identity"], receipt["webhook_test"]), ("silicon-accounts", "delivered"))
        backup_env = next(self.path("/var/backups/waveform").glob("accounts-cutover-*/native.env"))
        self.assertEqual(backup_env.read_bytes(), EARLIER)

    def test_a_failed_migration_restores_files_and_the_database(self):
        with self.assertRaisesRegex(RuntimeError, "migrate failed"):
            self.run_cutover(fail_at="operator:migrate")
        self.assertIn("restore", self.events)
        self.assertEqual(self.current.resolve(), self.previous.resolve())
        self.assertEqual(self.env.read_bytes(), EARLIER)
        self.assertEqual(self.unit.read_bytes(), b"old unit\n")
        self.assertLess(self.events.index("restore"), len(self.events) - 1 - self.events[::-1].index("run:systemctl start"))

    def test_public_readiness_failure_after_start_restores_the_database(self):
        # ready:0 and ready:1 are the checks before anything stops; ready:3 is public readiness of the new release.
        with self.assertRaisesRegex(RuntimeError, "readiness failed"):
            self.run_cutover(fail_at="ready:3")
        self.assertIn("restore", self.events)
        self.assertEqual(self.env.read_bytes(), EARLIER)

    def test_a_refused_preflight_changes_nothing(self):
        with self.assertRaisesRegex(RuntimeError, "preflight failed"):
            self.run_cutover(fail_at="preflight")
        self.assertNotIn("run:systemctl stop", self.events)
        self.assertNotIn("backup", self.events)
        self.assertEqual(self.env.read_bytes(), EARLIER)
        self.assertEqual(self.current.resolve(), self.previous.resolve())

    def test_an_undelivered_webhook_test_keeps_the_new_release_and_exits_3(self):
        self.assertEqual(self.run_cutover(webhook="failed: HTTP 401"), 3)
        self.assertNotIn("restore", self.events)
        self.assertEqual(upgrade.env_value(self.env.read_bytes(), "WAVEFORM_APP_SECRET"), SECRET)


if __name__ == "__main__":
    unittest.main()
