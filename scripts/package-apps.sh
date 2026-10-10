#!/usr/bin/env bash
# Package one Waveform CLI binary for Silicon Apps.
#
#   scripts/package-apps.sh <version> <target> <binary>
#   scripts/package-apps.sh 0.6.0 macos-aarch64 cli/target/release/waveform
#
# Writes dist/apps/waveform-<version>-<target>.tar.gz (apps.yaml listing only <target>, plus bin/waveform or
# bin/waveform.exe) and its .sha256, after `silicon-apps validate` and `silicon-apps pack`. When this machine can run
# the binary, it first runs `waveform --help`, `waveform accounts --json` and `waveform login status --json` in an
# empty home and refuses to pack a binary that answers them wrongly. PACKAGE_DISCOVERY=require makes "cannot run it
# here" an error too (the release workflow sets it on each target's own runner). Publishes nothing.
#
# Needs Python 3.9+ and silicon-apps 0.2 (`cargo install --locked silicon-apps-cli --version 0.2.0`; or set
# SILICON_APPS to its path). Run scripts/package-apps.sh --help for every option.
set -euo pipefail

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
for candidate in "${PYTHON:-}" python3 python; do
  [ -n "$candidate" ] || continue
  if command -v "$candidate" >/dev/null 2>&1 &&
    "$candidate" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 9) else 1)' >/dev/null 2>&1; then
    exec "$candidate" "$here/package_apps.py" "$@"
  fi
done
echo "package-apps: Python 3.9 or newer is required (set PYTHON to its path)" >&2
exit 1
