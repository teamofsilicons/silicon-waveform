#!/usr/bin/env python3
"""Local checks for scripts/package_apps.py: manifest rendering, native-format refusals and discovery.

Run: python3 scripts/test_package_apps.py
"""
import importlib.util
from pathlib import Path
import stat
import struct
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("package_apps", Path(__file__).with_name("package_apps.py"))
package_apps = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package_apps)

HOST_TARGET = {"linux": "linux-x86_64", "macos": "macos-aarch64", "windows": "windows-x86_64"}.get(package_apps.host_system())


def elf(bits, machine, interpreter=False):
    """A minimal little-endian ELF header with one program header (PT_INTERP or PT_LOAD)."""
    header_type = 3 if interpreter else 1
    if bits == 64:
        data = bytearray(64 + 56)
        data[:6] = b"\x7fELF\x02\x01"
        struct.pack_into("<H", data, 18, machine)
        struct.pack_into("<Q", data, 32, 64)
        struct.pack_into("<HH", data, 54, 56, 1)
        struct.pack_into("<I", data, 64, header_type)
    else:
        data = bytearray(52 + 32)
        data[:6] = b"\x7fELF\x01\x01"
        struct.pack_into("<H", data, 18, machine)
        struct.pack_into("<I", data, 28, 52)
        struct.pack_into("<HH", data, 42, 32, 1)
        struct.pack_into("<I", data, 52, header_type)
    return bytes(data)


def macho(cpu):
    return b"\xcf\xfa\xed\xfe" + struct.pack("<I", cpu) + bytes(24)


def pe(machine):
    data = bytearray(128)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 60, 64)
    data[64:68] = b"PE\0\0"
    struct.pack_into("<H", data, 68, machine)
    return bytes(data)


class ManifestTests(unittest.TestCase):
    def test_one_target_with_its_binary_and_no_comments(self):
        template = package_apps.TEMPLATE.read_text()
        linux = package_apps.render_manifest(template, "0.6.0", "linux-x86_64")
        self.assertEqual(linux, "schema_version: 1\napp_id: waveform\nversion: 0.6.0\ncommand: waveform\n"
                                "targets:\n  linux-x86_64:\n    binary: bin/waveform\n")
        windows = package_apps.render_manifest(template, "0.6.0", "windows-aarch64")
        self.assertIn("  windows-aarch64:\n    binary: bin/waveform.exe\n", windows)
        self.assertNotIn("#", windows)

    def test_lost_or_unknown_placeholders_are_refused(self):
        with self.assertRaisesRegex(package_apps.PackageError, "@TARGET@"):
            package_apps.render_manifest("version: @VERSION@\nbinary: @BINARY@\n", "0.6.0", "linux-x86_64")
        with self.assertRaisesRegex(package_apps.PackageError, "does not know"):
            package_apps.render_manifest("@VERSION@ @TARGET@ @BINARY@ @CHANNEL@\n", "0.6.0", "linux-x86_64")

    def test_version_comes_from_the_cli_manifest(self):
        self.assertRegex(package_apps.cli_version(), r"^\d+\.\d+\.\d+$")


class NativeFormatTests(unittest.TestCase):
    def test_each_target_accepts_its_own_format(self):
        samples = {
            "linux-x86_64": elf(64, 62), "linux-aarch64": elf(64, 183), "linux-i686": elf(32, 3),
            "linux-armv7hf": elf(32, 40), "macos-x86_64": macho(0x01000007), "macos-aarch64": macho(0x0100000C),
            "windows-x86_64": pe(0x8664), "windows-aarch64": pe(0xAA64), "windows-i686": pe(0x014C),
        }
        self.assertEqual(set(samples), set(package_apps.TARGETS))
        for target, data in samples.items():
            with self.subTest(target=target):
                package_apps.check_native(data, target)

    def test_wrong_system_or_processor_is_refused(self):
        cases = [
            (elf(64, 183), "linux-x86_64", "not built for linux-x86_64"),
            (elf(32, 3), "linux-x86_64", "not built for linux-x86_64"),
            (macho(0x01000007), "macos-aarch64", "not built for macos-aarch64"),
            (pe(0x8664), "windows-aarch64", "not built for windows-aarch64"),
            (elf(64, 62), "macos-x86_64", "Mach-O"),
            (macho(0x0100000C), "linux-aarch64", "ELF"),
            (b"#!/bin/sh\necho waveform\n", "windows-x86_64", "PE"),
            (b"\xca\xfe\xba\xbe" + bytes(60), "macos-aarch64", "one architecture"),
        ]
        for data, target, message in cases:
            with self.subTest(target=target, message=message), self.assertRaisesRegex(package_apps.PackageError, message):
                package_apps.check_native(data, target)

    def test_dynamic_linux_executables_are_refused(self):
        for bits, machine, target in ((64, 62, "linux-x86_64"), (32, 40, "linux-armv7hf")):
            with self.subTest(target=target), self.assertRaisesRegex(package_apps.PackageError, "dynamic loader"):
                package_apps.check_native(elf(bits, machine, interpreter=True), target)


@unittest.skipIf(HOST_TARGET is None or package_apps.host_system() == "windows", "needs a POSIX shell host")
class DiscoveryTests(unittest.TestCase):
    def fake(self, accounts='{"app_id":"waveform","version":"0.6.0"}', status='{"authenticated":false}',
             version="waveform 0.6.0", help_exit=0):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        directory = Path(temporary.name)
        script = directory / "waveform"
        script.write_text(f"""#!/bin/sh
[ -z "$WAVEFORM_URL" ] || exit 9
case "$*" in
  --help) echo "waveform: speech for Carbons and Silicons"; exit {help_exit} ;;
  "accounts --json") echo '{accounts}' ;;
  "login status --json") echo '{status}' ;;
  --version) echo '{version}' ;;
  *) exit 2 ;;
esac
""")
        script.chmod(script.stat().st_mode | stat.S_IEXEC)
        return script

    def test_a_correct_binary_passes_in_an_empty_home(self):
        self.assertTrue(package_apps.discovery(self.fake(), HOST_TARGET, "0.6.0", "require"))

    def test_each_wrong_answer_is_refused_with_its_reason(self):
        cases = [
            (dict(help_exit=1), "--help` must exit 0"),
            (dict(accounts='{"app_id":"remind"}'), '"app_id":"waveform"'),
            (dict(accounts="not json"), "one JSON object"),
            (dict(status='{"authenticated":true,"id":"c:ada"}'), "authenticated false"),
            (dict(version="waveform 0.5.1"), "must print `waveform 0.6.0`"),
        ]
        for arguments, message in cases:
            with self.subTest(message=message), self.assertRaisesRegex(package_apps.PackageError, message):
                package_apps.discovery(self.fake(**arguments), HOST_TARGET, "0.6.0", "auto")

    def test_the_callers_environment_does_not_reach_the_binary(self):
        import os
        os.environ["WAVEFORM_URL"] = "http://127.0.0.1:1"
        try:
            self.assertTrue(package_apps.discovery(self.fake(), HOST_TARGET, "0.6.0", "require"))
        finally:
            del os.environ["WAVEFORM_URL"]

    def test_a_binary_for_another_system_is_skipped_or_refused(self):
        other = "windows-x86_64" if package_apps.host_system() != "windows" else "linux-x86_64"
        self.assertFalse(package_apps.discovery(self.fake(), other, "0.6.0", "auto"))
        with self.assertRaisesRegex(package_apps.PackageError, "discovery is required"):
            package_apps.discovery(self.fake(), other, "0.6.0", "require")


class ArgumentTests(unittest.TestCase):
    def test_unknown_targets_and_loose_versions_are_refused(self):
        with self.assertRaisesRegex(package_apps.PackageError, "unknown target"):
            package_apps.package("0.6.0", "freebsd-x86_64", Path(sys.executable), Path("dist"), "auto")
        with self.assertRaisesRegex(package_apps.PackageError, "strict x.y.z"):
            package_apps.package("0.6.0-rc.1", "linux-x86_64", Path(sys.executable), Path("dist"), "auto")
        with self.assertRaisesRegex(package_apps.PackageError, "differs from cli/Cargo.toml"):
            package_apps.package("9.9.9", "linux-x86_64", Path(sys.executable), Path("dist"), "auto")


if __name__ == "__main__":
    unittest.main()
