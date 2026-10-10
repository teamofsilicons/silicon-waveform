# Parallel Accounts production service

The 2026-10-10 release creates an independent Accounts service. Existing IAM Silicons, credentials, workers and resources stay on the old `backend.waveform.teamofsilicons.com` service. No IAM principal is enrolled or adopted. New Accounts resources live only in the new store.

- New public API: `https://api.waveform.teamofsilicons.com`.
- API unit: `waveform-accounts`; configuration `/etc/waveform-accounts/runtime.env` (root0600), data `/var/lib/waveform-accounts`, releases `/opt/waveform-accounts/releases`.
- Accounts runtime secret: `silicon-waveform/accounts-production/runtime`. The webhook secret must come from Accounts; a locally generated placeholder does not configure Accounts deliveries.
- Daily backup: `waveform-accounts-backup.timer`, private manifests under `/var/backups/waveform-accounts`, SSE-encrypted S3 objects under `parallel-accounts-20261010/waveform/backups/` in the existing `silicon-hook-standalone-artifacts-lxpfsbc0jpuk` bucket.

`bootstrap.py waveform` runs as root once, refuses existing namespaces, creates a fresh store/key/configuration and appends only the new API maintenance vhost. It never stops an old writer. The service user needs directory traversal on public executable and certificate directories; environment/key files remain private. Provider credentials are copied only for Waveform's global speech providers; no account records or account encryption keys are reused.

`install.py waveform --archive PATH --sha256 HASH --revision FULL_COMMIT --validator PATH` validates the native backend bundle with the existing repository validator (`deploy/native/upgrade.py` for Waveform; `deploy/install.py` for MCPort), performs a genuine Accounts app credential preflight, stages a separate release, starts only the new API, and opens the new API vhost only after readiness. `--resume-staged` requires the exact selected release and rechecks every staged byte against the candidate before retrying readiness. Never run the legacy in-place installer for this parallel deployment.

The application website uses Accounts and the new API. Existing IAM resources are intentionally absent from this UI and remain available through the unchanged legacy API and Honeycomb CLI. Roll back the website by restoring its saved proxy/project deployment; roll back the new API independently. Do not point the Accounts binary at the legacy store or replace legacy keys.

Backups contain database snapshots plus keys and runtime settings, with no secrets printed. For recovery, stop only the new service, retain a copy of its current files, restore the database with its matching keys/configuration, verify integrity and restore the recorded release. PostgreSQL backups use pg_dump; SQLite backups use the online backup API and an integrity check. The old IAM service is outside these commands.

Production evidence and private receipts are retained in the operator workspace `.migration/live/waveform-accounts`; secrets are intentionally excluded from Git. All production application binaries are optimized release builds. Local debug archives are not release candidates.

Waveform PostgreSQL is the independent `waveform-accounts-postgres` container, bound only at127.0.0.1:5434 with a fresh private TLS CA and SCRAM credentials. The API verifies the database certificate. `install-web.py` selects an exact-revision ARM64 Next.js image, starts `waveform-accounts-frontend`, checks `/auth/session` returns `signed_in:false`, and changes only the website upstream. The old frontend and Interface containers remain running.
