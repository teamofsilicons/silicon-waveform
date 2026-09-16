#!/bin/sh
# Compatibility entry point; Honeycomb installs native prebuilt releases.
set -eu
command -v honeycomb >/dev/null 2>&1 || { echo "Install Honeycomb first: https://docs.honeycomb.teamofsilicons.com/" >&2; exit 1; }
exec honeycomb install 'tos>waveform'
