#!/usr/bin/env bash
# Waveform end to end against a Silicon Accounts development stack, with real Silicon Accounts tokens.
#
#   scripts/e2e-accounts.sh [--only 1,2,…] [--keep-running] [--no-start]
#
# Starts the local API and Briefcase stub with scripts/dev-accounts.sh (fresh database), runs the eight scenarios
# (a Carbon on the API, a Silicon on the CLI, the device flow, custodians and outsiders, webhooks, proofs in and out,
# discovery from a packaged archive, restart safety), prints PASS/FAIL per check and a summary, and stops what it
# started. Exits 1 when any check fails. Needs WAVEFORM_DEV_STACK, WAVEFORM_E2E_MINT, WAVEFORM_E2E_TSX and
# WAVEFORM_E2E_ACCOUNTS_CLI (environment or .mig/dev-accounts.env); run scripts/e2e-accounts.sh --help for details.
# Uses unique identities per run on the shared stack and never touches another app's setup.
set -euo pipefail

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
for candidate in "${PYTHON:-}" python3 python; do
  [ -n "$candidate" ] || continue
  if command -v "$candidate" >/dev/null 2>&1 &&
    "$candidate" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 9) else 1)' >/dev/null 2>&1; then
    exec "$candidate" -I "$here/e2e_accounts.py" "$@"
  fi
done
echo "e2e-accounts: Python 3.9 or newer is required (set PYTHON to its path)" >&2
exit 1
