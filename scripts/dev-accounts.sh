#!/usr/bin/env bash
# Run Waveform on this machine against a Silicon Accounts development stack.
#
#   WAVEFORM_DEV_STACK=<stack.json> scripts/dev-accounts.sh start [--fresh-db] [--no-stub]
#   scripts/dev-accounts.sh stop [--keep-webhook] | restart-api | status | env
#
# `start` is idempotent: it creates the database (waveform_e2e on 127.0.0.1:5460) and migrates it, keeps encryption
# keys under .mig/dev, makes Waveform's sign-in setup on the stack offer email codes, the device flow, the public client
# and the local callbacks, points Waveform's webhook at the API (POST /webhooks/accounts) with a fresh secret, starts a
# Briefcase stub on port base+2 (unless WAVEFORM_BRIEFCASE_BASE_URL names a running Briefcase) and the API on base+1
# (4161) with speech fixtures, then waits for a signed ping to arrive. `stop` stops both and hands the webhook back to
# the stack's fake app. Pids go to .mig/pids, logs to .mig/logs. Development secrets come from the stack description
# only; nothing is written to the repository. Run scripts/dev-accounts.sh --help for every setting.
set -euo pipefail

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
for candidate in "${PYTHON:-}" python3 python; do
  [ -n "$candidate" ] || continue
  if command -v "$candidate" >/dev/null 2>&1 &&
    "$candidate" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 9) else 1)' >/dev/null 2>&1; then
    exec "$candidate" -I "$here/dev_accounts.py" "$@"
  fi
done
echo "dev-accounts: Python 3.9 or newer is required (set PYTHON to its path)" >&2
exit 1
