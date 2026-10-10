#!/usr/bin/env python3
"""Package one Waveform CLI binary as a Silicon Apps archive.

    scripts/package-apps.sh <version> <target> <binary>

Renders packaging/apps.yaml.in for exactly one target and stages apps.yaml plus bin/waveform (bin/waveform.exe on
Windows). It refuses a binary that is not a native executable for the target (Linux binaries must also be static),
runs the three discovery commands Silicon Apps requires in an empty home whenever this machine can run the binary,
runs `silicon-apps validate` and `silicon-apps pack`, checks and re-validates what the archive contains, and writes
dist/apps/waveform-<version>-<target>.tar.gz with a .sha256 file beside it. It never uploads or publishes anything.

Discovery (--discovery, or PACKAGE_DISCOVERY): `auto` runs the commands when the binary can run here and says so
when it cannot (the Silicon Apps validation worker for that target runs them at upload); `require` fails instead,
which is what the release workflow uses on each target's own runner.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]
APP_ID = "waveform"
COMMAND = "waveform"
TEMPLATE = ROOT / "packaging" / "apps.yaml.in"
PACKER_SERIES = "0.2."

# Silicon Apps target -> (operating system, ELF/Mach-O/PE machine description).
TARGETS = {
    "linux-x86_64": "linux",
    "linux-i686": "linux",
    "linux-aarch64": "linux",
    "linux-armv7hf": "linux",
    "windows-x86_64": "windows",
    "windows-i686": "windows",
    "windows-aarch64": "windows",
    "macos-x86_64": "macos",
    "macos-aarch64": "macos",
}
ELF_MACHINE = {"x86_64": (2, 62), "i686": (1, 3), "aarch64": (2, 183), "armv7hf": (1, 40)}
MACHO_CPU = {"x86_64": 0x01000007, "aarch64": 0x0100000C}
PE_MACHINE = {"x86_64": 0x8664, "i686": 0x014C, "aarch64": 0xAA64}


class PackageError(Exception):
    """A refusal with the exact reason; the script prints it and exits 1."""


def binary_name(target):
    return f"{COMMAND}.exe" if target.startswith("windows-") else COMMAND


def cli_version(root=ROOT):
    """The version in cli/Cargo.toml's [package] table."""
    text = (root / "cli" / "Cargo.toml").read_text(encoding="utf-8")
    package = re.search(r"(?ms)^\[package\]\s*$(.*?)(?=^\[|\Z)", text)
    match = package and re.search(r'(?m)^\s*version\s*=\s*"([^"]+)"', package.group(1))
    if not match:
        raise PackageError("cli/Cargo.toml has no [package] version")
    return match.group(1)


def render_manifest(template, version, target):
    """apps.yaml for one target: placeholders filled in, comment lines dropped."""
    text = "".join(line for line in template.splitlines(keepends=True) if not line.lstrip().startswith("#"))
    for placeholder, value in (("@VERSION@", version), ("@TARGET@", target), ("@BINARY@", f"bin/{binary_name(target)}")):
        if placeholder not in text:
            raise PackageError(f"{TEMPLATE.relative_to(ROOT)} lost its {placeholder} placeholder")
        text = text.replace(placeholder, value)
    if "@" in text:
        raise PackageError(f"{TEMPLATE.relative_to(ROOT)} has a placeholder this script does not know")
    return text


def check_native(data, target):
    """Refuse anything but a native executable for the target; Linux executables must be static."""
    system = TARGETS[target]
    arch = target.split("-", 1)[1]
    if system == "linux":
        expected_class, machine = ELF_MACHINE[arch]
        if data[:4] != b"\x7fELF" or len(data) < 52 or data[5] != 1:
            raise PackageError(f"{target} needs a little-endian ELF executable; this file is not one")
        if data[4] != expected_class or struct.unpack_from("<H", data, 18)[0] != machine:
            raise PackageError(f"the ELF executable is not built for {target}")
        if expected_class == 2:
            offset = struct.unpack_from("<Q", data, 32)[0]
            size, count = struct.unpack_from("<HH", data, 54)
        else:
            offset = struct.unpack_from("<I", data, 28)[0]
            size, count = struct.unpack_from("<HH", data, 42)
        if count == 0 or offset + size * count > len(data):
            raise PackageError("the ELF executable has no readable program headers")
        for index in range(count):
            if struct.unpack_from("<I", data, offset + size * index)[0] == 3:  # PT_INTERP
                raise PackageError(
                    f"the {target} executable needs a dynamic loader; build it static (the musl target, as the "
                    "release workflow does) so it runs on every Linux the Silicons use"
                )
    elif system == "macos":
        if data[:4] != b"\xcf\xfa\xed\xfe" or len(data) < 8:
            raise PackageError(f"{target} needs a 64-bit Mach-O executable for one architecture; this file is not one")
        if struct.unpack_from("<I", data, 4)[0] != MACHO_CPU[arch]:
            raise PackageError(f"the Mach-O executable is not built for {target}")
    else:
        if data[:2] != b"MZ" or len(data) < 64:
            raise PackageError(f"{target} needs a Windows PE executable; this file is not one")
        offset = struct.unpack_from("<I", data, 60)[0]
        if len(data) < offset + 6 or data[offset:offset + 4] != b"PE\0\0":
            raise PackageError(f"{target} needs a Windows PE executable; this file has no PE header")
        if struct.unpack_from("<H", data, offset + 4)[0] != PE_MACHINE[arch]:
            raise PackageError(f"the Windows executable is not built for {target}")


def host_system():
    if sys.platform.startswith("linux"):
        return "linux"
    if sys.platform == "darwin":
        return "macos"
    if sys.platform in ("win32", "cygwin", "msys"):
        return "windows"
    return sys.platform


def clean_environment(home):
    """What a validation worker gives the binary: an empty home and nothing of this shell's own settings."""
    environment = {"HOME": str(home), "SILICON_HOME": str(home), "USERPROFILE": str(home), "TMPDIR": str(home / "tmp"),
                   "PATH": os.pathsep.join(["/usr/bin", "/bin"])}
    if host_system() == "windows":
        # Windows needs these to start any program; PATH is the system directories only.
        system_root = os.environ.get("SYSTEMROOT", r"C:\Windows")
        environment.update({"SYSTEMROOT": system_root, "WINDIR": os.environ.get("WINDIR", system_root),
                            "PATH": os.pathsep.join([os.path.join(system_root, "System32"), system_root])})
        environment["TEMP"] = environment["TMP"] = environment["TMPDIR"]
    return environment


def run_clean(binary, arguments, home):
    """Run the binary in the empty home; (exit code, stdout, stderr). Raises OSError when it cannot start here."""
    (home / "tmp").mkdir(exist_ok=True)
    result = subprocess.run([str(binary), *arguments], cwd=home, env=clean_environment(home), capture_output=True,
                            timeout=60, check=False)
    return result.returncode, result.stdout.decode("utf-8", "replace"), result.stderr.decode("utf-8", "replace")


def json_object(text, command):
    try:
        value = json.loads(text)
    except json.JSONDecodeError as error:
        raise PackageError(f"`{command}` must print one JSON object; it printed {text.strip()[:200]!r} ({error})")
    if not isinstance(value, dict):
        raise PackageError(f"`{command}` must print a JSON object; it printed {text.strip()[:200]!r}")
    return value


def discovery(binary, target, version, mode):
    """Run the three commands Silicon Apps runs at upload, signed out, in an empty home. False when it cannot run."""
    if TARGETS[target] != host_system():
        if mode == "require":
            raise PackageError(f"discovery is required, but a {target} binary cannot run on this {host_system()} machine")
        print(f"note: a {target} binary cannot run here; the Silicon Apps validation worker for {target} runs the "
              "discovery commands at upload")
        return False
    with tempfile.TemporaryDirectory(prefix="waveform-discovery-") as directory:
        home = Path(directory)
        try:
            code, out, err = run_clean(binary, ["--help"], home)
        except OSError as error:  # Exec format error, Bad CPU type: right system, other processor.
            if mode == "require":
                raise PackageError(f"discovery is required, but this machine cannot run the {target} binary: {error}")
            print(f"note: this machine cannot run the {target} binary ({error}); the Silicon Apps validation worker "
                  f"for {target} runs the discovery commands at upload")
            return False
        if code != 0 or not out.strip():
            raise PackageError(f"`{COMMAND} --help` must exit 0 and print help; it exited {code}: {(err or out)[:400]}")
        code, out, err = run_clean(binary, ["accounts", "--json"], home)
        if code != 0:
            raise PackageError(f"`{COMMAND} accounts --json` must exit 0; it exited {code}: {(err or out)[:400]}")
        accounts = json_object(out, f"{COMMAND} accounts --json")
        if accounts.get("app_id") != APP_ID:
            raise PackageError(f'`{COMMAND} accounts --json` must contain "app_id":"{APP_ID}"; it printed {out.strip()[:400]}')
        code, out, err = run_clean(binary, ["login", "status", "--json"], home)
        if code != 0:
            raise PackageError(f"`{COMMAND} login status --json` must exit 0; it exited {code}: {(err or out)[:400]}")
        status = json_object(out, f"{COMMAND} login status --json")
        if status.get("authenticated") is not False:
            raise PackageError(f"`{COMMAND} login status --json` must report authenticated false in an empty home; "
                               f"it printed {out.strip()[:400]}")
        code, out, err = run_clean(binary, ["--version"], home)
        if code != 0 or out.split() != [COMMAND, version]:
            raise PackageError(f"`{COMMAND} --version` must print `{COMMAND} {version}` to match apps.yaml; it printed "
                               f"{(out or err).strip()[:200]!r}")
    print(f"discovery: --help, accounts --json and login status --json answered as Silicon Apps requires ({target})")
    return True


def find_packer(explicit):
    candidates = [explicit, os.environ.get("SILICON_APPS"), shutil.which("silicon-apps"),
                  str(Path.home() / ".cargo" / "bin" / "silicon-apps"), str(Path.home() / ".apps" / "bin" / "silicon-apps")]
    for candidate in candidates:
        if candidate and Path(candidate).is_file():
            return candidate
    raise PackageError("silicon-apps is not installed: run `cargo install --locked silicon-apps-cli --version 0.2.0` "
                       "or pass --silicon-apps PATH")


def packer(command, arguments, apps_home):
    """Run silicon-apps with an empty home of its own: validate and pack are local and must not see any sign-in."""
    environment = {key: value for key, value in os.environ.items()
                   if key not in ("APPS_TOKEN", "APPS_TELEMETRY_TABLE_KEY", "APPS_TELEMETRY_KEY", "SILICON_HOME")}
    environment["SILICON_APPS_NO_DAEMON"] = "1"
    return subprocess.run([command, *arguments, "--home", str(apps_home), "--json"], env=environment,
                          capture_output=True, text=True, timeout=300, check=False)


def validate(command, directory, apps_home):
    result = packer(command, ["validate", str(directory)], apps_home)
    try:
        report = json.loads(result.stdout)
    except json.JSONDecodeError:
        report = {}
    if result.returncode != 0 or report.get("valid") is not True:
        raise PackageError(f"silicon-apps validate refused the package: {(result.stdout or result.stderr).strip()[:2000]}")


def verify_archive(command, archive, stage, target, apps_home):
    """The archive holds exactly apps.yaml and the binary, byte for byte, and validates again once extracted."""
    expected = {"apps.yaml", f"bin/{binary_name(target)}"}
    with tarfile.open(archive, "r:gz") as package:
        members = {member.name.removeprefix("./"): member for member in package.getmembers()}
        if set(members) != expected or not all(member.isfile() for member in members.values()):
            raise PackageError(f"the archive must contain exactly {sorted(expected)}; it contains {sorted(members)}")
        if TARGETS[target] != "windows" and not members[f"bin/{COMMAND}"].mode & 0o111:
            raise PackageError(f"bin/{COMMAND} lost its executable mode in the archive")
        with tempfile.TemporaryDirectory(prefix="waveform-extracted-") as directory:
            extracted = Path(directory)
            for name, member in members.items():
                destination = extracted / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes(package.extractfile(member).read())
                if destination.read_bytes() != (stage / name).read_bytes():
                    raise PackageError(f"{name} in the archive differs from the staged file")
            validate(command, extracted, apps_home)


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def package(version, target, binary, output_dir, mode, explicit_packer=None):
    if target not in TARGETS:
        raise PackageError(f"unknown target {target!r}; Silicon Apps targets are {', '.join(TARGETS)}")
    if not re.fullmatch(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", version):
        raise PackageError(f"version {version!r} must be strict x.y.z (no prerelease or build suffix)")
    if version != cli_version():
        raise PackageError(f"version {version} differs from cli/Cargo.toml ({cli_version()}); apps.yaml must match the binary")
    if not binary.is_file():
        raise PackageError(f"{binary} is not a file")
    check_native(binary.read_bytes(), target)
    command = find_packer(explicit_packer)
    packer_version = subprocess.run([command, "--version"], capture_output=True, text=True, check=False).stdout.split()
    if len(packer_version) != 2 or not packer_version[1].startswith(PACKER_SERIES):
        raise PackageError(f"{command} reports {' '.join(packer_version) or 'no version'}; this script needs silicon-apps "
                           f"{PACKER_SERIES}x (`cargo install --locked silicon-apps-cli --version 0.2.0`)")
    output_dir.mkdir(parents=True, exist_ok=True)
    archive = output_dir / f"{APP_ID}-{version}-{target}.tar.gz"
    with tempfile.TemporaryDirectory(prefix="waveform-package-") as directory:
        work = Path(directory)
        stage, apps_home = work / "package", work / "apps-home"
        (stage / "bin").mkdir(parents=True)
        apps_home.mkdir()
        (stage / "apps.yaml").write_text(render_manifest(TEMPLATE.read_text(encoding="utf-8"), version, target),
                                         encoding="utf-8", newline="\n")
        staged_binary = stage / "bin" / binary_name(target)
        shutil.copyfile(binary, staged_binary)
        staged_binary.chmod(0o755)
        ran = discovery(staged_binary, target, version, mode)
        validate(command, stage, apps_home)
        temporary = work / archive.name
        result = packer(command, ["pack", str(stage), "--output", str(temporary)], apps_home)
        if result.returncode != 0 or not temporary.is_file():
            raise PackageError(f"silicon-apps pack failed: {(result.stdout or result.stderr).strip()[:2000]}")
        verify_archive(command, temporary, stage, target, apps_home)
        shutil.move(str(temporary), archive)
    digest = sha256(archive)
    Path(f"{archive}.sha256").write_text(f"{digest}  {archive.name}\n", encoding="utf-8", newline="\n")
    print(f"packaged {archive} ({archive.stat().st_size} bytes)\nsha256 {digest}"
          + ("" if ran else f"\ndiscovery commands not run on this machine (target {target})"))
    return archive


def main(argv=None):
    parser = argparse.ArgumentParser(prog="scripts/package-apps.sh", description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("version", help="the release version, equal to cli/Cargo.toml (for example 0.6.0)")
    parser.add_argument("target", help="one Silicon Apps target, for example linux-x86_64 or macos-aarch64")
    parser.add_argument("binary", type=Path, help="the built waveform executable for that target")
    parser.add_argument("--output-dir", type=Path, default=ROOT / "dist" / "apps", help="default: dist/apps")
    parser.add_argument("--discovery", choices=("auto", "require"), default=os.environ.get("PACKAGE_DISCOVERY", "auto"))
    parser.add_argument("--silicon-apps", help="the silicon-apps executable (default: SILICON_APPS, then PATH)")
    args = parser.parse_args(argv)
    if args.discovery not in ("auto", "require"):
        parser.error(f"PACKAGE_DISCOVERY must be auto or require, not {args.discovery!r}")
    try:
        package(args.version, args.target, args.binary, args.output_dir.resolve(), args.discovery, args.silicon_apps)
    except (PackageError, OSError, subprocess.SubprocessError) as error:
        print(f"package-apps: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
