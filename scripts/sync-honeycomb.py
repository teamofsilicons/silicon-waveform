#!/usr/bin/env python3
"""Keep isolated frontend build contexts in sync with the canonical manifest."""

import argparse
from pathlib import Path
import re
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    manifest = (ROOT / "honeycomb.yaml").read_bytes()
    version = tomllib.loads((ROOT / "cli/Cargo.toml").read_text())["package"]["version"]
    for field, expected in (("app_id", "tos>waveform"), ("version", version)):
        match = re.search(rf"^{field}:\s*(\S+)\s*$", manifest.decode(), re.MULTILINE)
        if not match or match.group(1) != expected:
            raise SystemExit(f"honeycomb.yaml {field} must be {expected}")
    destination = ROOT / "frontend/public/honeycomb.yaml"
    if args.check:
        if not destination.is_file() or destination.read_bytes() != manifest:
            raise SystemExit("Run python3 scripts/sync-honeycomb.py to refresh the frontend manifest")
    else:
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(manifest)
    print("Honeycomb manifest is synchronized (tos>waveform)")


if __name__ == "__main__":
    main()
