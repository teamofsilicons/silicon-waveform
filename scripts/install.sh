#!/bin/sh
# Install the exact source snapshot served with these docs; authentication is separate.
set -eu
case "$(uname -s)" in Darwin|Linux) ;; *) echo 'Waveform installer supports macOS and Linux.' >&2; exit 1;; esac
command -v curl >/dev/null || { echo 'Install curl first.' >&2; exit 1; }
command -v tar >/dev/null || { echo 'Install tar first.' >&2; exit 1; }
if ! command -v cargo >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.98.0
  . "$HOME/.cargo/env"
fi
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT HUP INT TERM
curl --proto '=https' --tlsv1.2 -fsSL https://docs.waveform.teamofsilicons.com/waveform-source.tar.gz -o "$work/source.tar.gz"
curl --proto '=https' --tlsv1.2 -fsSL https://docs.waveform.teamofsilicons.com/waveform-source.tar.gz.sha256 -o "$work/source.sha256"
expected=$(cut -d " " -f 1 "$work/source.sha256")
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$work/source.tar.gz" | cut -d " " -f 1)
else
  actual=$(shasum -a 256 "$work/source.tar.gz" | cut -d " " -f 1)
fi
[ "$actual" = "$expected" ] || { echo 'Source checksum mismatch.' >&2; exit 1; }
tar -xzf "$work/source.tar.gz" -C "$work"
cd "$work/waveform"
cargo install --path cli --locked --force
cargo_bin="${CARGO_HOME:-$HOME/.cargo}/bin"
if [ "${WAVEFORM_INSTALL_DAEMON:-1}" != 0 ]; then
  "$cargo_bin/waveform" daemon install
fi
printf '\nWaveform installed. Add %s to PATH if needed.\nRun waveform iam --json, then waveform login SLT.\n' "$cargo_bin"
