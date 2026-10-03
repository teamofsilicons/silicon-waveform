#!/usr/bin/env python3
"""Package the static ARM64 API and FFmpeg's native shared-library closure."""
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def output(*args):
    return subprocess.check_output(args, text=True)


def main():
    binary = ROOT / "target/aarch64-unknown-linux-musl/release/waveform-api"
    headers = output("readelf", "-h", str(binary))
    assert "AArch64" in headers, "Expected ARM64 backend"
    assert "INTERP" not in output("readelf", "-l", str(binary)), "Backend must be static"
    revision = output("git", "rev-parse", "HEAD").strip()
    destination = ROOT / "dist/backend"
    destination.mkdir(parents=True, exist_ok=True)
    archive = destination / f"waveform-backend-{revision[:12]}-linux-aarch64.tar.gz"
    with tempfile.TemporaryDirectory(prefix="waveform-backend-") as directory:
        stage = Path(directory)
        for name in ("bin", "lib", "share"):
            (stage / name).mkdir()
        shutil.copyfile(binary, stage / "bin/waveform-api")
        (stage / "bin/waveform-api").chmod(0o755)
        ffmpeg = shutil.which("ffmpeg")
        assert ffmpeg
        shutil.copyfile(ffmpeg, stage / "bin/ffmpeg.real")
        (stage / "bin/ffmpeg.real").chmod(0o755)
        dependencies = output("ldd", ffmpeg)
        assert "not found" not in dependencies, dependencies
        paths = set(re.findall(r"(/[^\s()]+)", dependencies))
        loader = None
        for path in sorted(paths):
            source = Path(path)
            assert source.is_file(), source
            target = stage / "lib" / source.name
            if target.exists():
                assert target.read_bytes() == source.read_bytes(), source
            else:
                shutil.copyfile(source, target)
                target.chmod(0o755)
            if source.name.startswith("ld-linux-"):
                loader = source.name
        assert loader, "Missing FFmpeg runtime loader"
        wrapper = stage / "bin/ffmpeg"
        wrapper.write_text('#!/bin/sh\nset -eu\nroot=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)\n'
                           f'exec "$root/lib/{loader}" --library-path "$root/lib" "$root/bin/ffmpeg.real" "$@"\n')
        wrapper.chmod(0o755)
        subprocess.run([str(wrapper), "-v", "error", "-f", "lavfi", "-i", "sine=frequency=440:duration=0.1", "-f", "wav", "-y", str(stage / "audio-smoke.wav")], check=True)
        assert (stage / "audio-smoke.wav").stat().st_size > 100
        (stage / "audio-smoke.wav").unlink()
        shutil.copyfile(ROOT / "honeycomb.yaml", stage / "honeycomb.yaml")
        shutil.copyfile(ROOT / "deploy/native/waveform-api.service", stage / "waveform-api.service")
        shutil.copyfile(ROOT / "deploy/native/prepare.py", stage / "prepare.py")
        for package in ("ffmpeg", "libc6"):
            copyright = Path("/usr/share/doc") / package / "copyright"
            if copyright.is_file():
                shutil.copyfile(copyright, stage / "share" / f"{package}-copyright")
        metadata = {"app_id": "waveform", "source_revision": revision,
                    "architecture": "aarch64", "backend_linkage": "static-musl",
                    "ffmpeg_version": output(ffmpeg, "-version").splitlines()[0]}
        (stage / "build.json").write_text(json.dumps(metadata, indent=2) + "\n")
        checksums = []
        for file in sorted(stage.rglob("*")):
            if file.is_file():
                checksums.append(f"{hashlib.sha256(file.read_bytes()).hexdigest()}  {file.relative_to(stage)}")
        (stage / "SHA256SUMS").write_text("\n".join(checksums) + "\n")
        with tarfile.open(archive, "w:gz") as package:
            for file in sorted(stage.iterdir()):
                package.add(file, arcname=file.name)
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    Path(str(archive) + ".sha256").write_text(f"{digest}  {archive.name}\n")
    print(json.dumps({"archive": str(archive), "sha256": digest, **metadata}, indent=2))


if __name__ == "__main__":
    main()
