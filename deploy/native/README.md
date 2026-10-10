# Native API deployment

The API runs directly as `/opt/waveform/current/bin/waveform-api` under `waveform-api.service`, using the dedicated
`waveform` account. PostgreSQL, the website and the shared Caddy run as containers beside it; the API unit follows
the PostgreSQL service lifecycle and binds only the private Docker bridge gateway, where Caddy reaches it. There is no
other public listener.

| Tool | Use it for |
| --- | --- |
| `accounts_cutover.py` | Once: move the running API to Silicon Accounts (the 0.6.0 release). |
| `upgrade.py` | Every later release of the API. |
| `install-frontend.py` | Every release of the website container. |
| `install.py` | Once per host: move a container-run API (a host rebuilt with `deploy/aws`) to this native service. |

Run them as root on the host, through SSM (`deploy/aws/ssm.py`). Never put a secret on a command line: SSM records
command text and output. Run the local checks before using a changed tool:

```sh
python3 deploy/native/test_upgrade.py
python3 deploy/native/test_accounts_cutover.py
```

## Build the bundle

Run the **Build native backend bundle** workflow (`backend-release.yml`). It compiles a static ARM64 musl
`waveform-api` from the selected revision and packages FFmpeg with its native loader and shared libraries, behind a
wrapper that gives FFmpeg its own library path without changing the host's. The bundle also holds `build.json` (app
id, source revision, architecture, linkage), the systemd unit, `prepare.py` and per-file SHA-256 checksums. It is a
server artifact; the CLI ships separately through Silicon Apps.

Database TLS stays `verify-full`. Before each start the unit's privileged `prepare.py` finds the PostgreSQL
container's address, writes a service-private hosts file and copies the CA into `/run/waveform`. The API itself runs
unprivileged with read-only system directories, a private temporary directory and bounded memory, and cannot inspect
Docker or read the root-owned environment files.

## Move to Silicon Accounts (once)

The full runbook, with what to prepare and how to verify, is [`docs/migration/cutover.md`](../../docs/migration/cutover.md).
The tool's part:

```sh
python3 accounts_cutover.py \
  --archive /path/to/waveform-backend-REVISION-linux-aarch64.tar.gz \
  --sha256 ARCHIVE_SHA256 \
  --source-revision FULL_40_CHARACTER_GIT_REVISION \
  --backup-bucket EXISTING_ENCRYPTED_BACKUP_BUCKET \
  --accounts-env /root/waveform-accounts.env \
  --mapping /root/waveform-identity-map.csv
```

`--accounts-env` is a file owned by root with mode `0600`:

```sh
WAVEFORM_APP_SECRET=sa_app_…
WAVEFORM_ACCOUNTS_WEBHOOK_SECRET=whsec_…
WAVEFORM_PROOF_ISSUERS=waveform.tts=interface;waveform.stt=interface
```

It may also set `ACCOUNTS_URL`, `ACCOUNTS_API_URL`, `WAVEFORM_PUBLIC_URL`, `WAVEFORM_ACCOUNTS_TIMEOUT_SECONDS`,
`WAVEFORM_ACCOUNTS_WEBHOOK_SECRET_PREVIOUS` and the `WAVEFORM_BRIEFCASE_*` settings; anything else is refused.
`--mapping` (rows `iam_principal_id,accounts_uuid`) is optional: without it, link earlier data later with
`waveform-api link-identities`.

What it does, in order:

1. **Checks, changing nothing:** the bundle exactly as `upgrade.py` checks it; the accounts env file; that
   `native.env` still holds the earlier sign-in settings (otherwise use `upgrade.py`); that Silicon Accounts accepts
   Waveform's app credentials (introspecting a token that does not exist must answer `{"active": false}`); that the
   running API is ready.
2. **Backs up** `native.env`, the unit, the release link, the receipt and the database (a local dump checked with
   `pg_restore --list`, and an S3 copy with server-side encryption and a verified SHA-256) under
   `/var/backups/waveform/accounts-cutover-*`.
3. **With the API stopped:** writes the new `native.env` (the earlier sign-in, webhook and distribution settings
   removed, the Silicon Accounts settings added, `WAVEFORM_BRIEFCASE_APP_ID=briefcase`, every other byte kept),
   installs the unit, switches the release, applies the migrations (`waveform-api migrate`) and, with `--mapping`,
   links earlier data (`waveform-api link-identities`: a dry run first, refused on any conflict), so nobody signs in
   before their data is linked.
4. **Starts** the API and waits for private and public readiness.
5. **Tests the webhook:** asks Silicon Accounts for a test delivery to `/webhooks/accounts` and waits for it.

Any failure in steps 3 and 4 restores the previous release, `native.env` and unit, and the database exactly as it
was dumped: the migrated database is renamed `waveform_cutover_<stamp>` (kept for inspection, never dropped) and the
dump is restored into a new `waveform` database owned by `waveform`. The previous API is started and checked. This
is needed because the previous binary refuses to start on a database that has migrations it does not know.

Exit 0 means released and the webhook delivered; exit 3 means released but the webhook test failed (the API stays
up: fix the webhook URL or secret and test again). The receipt in `/etc/waveform/native-release.json` records the
release, the backup, the migrations before and after, the linking counts and the webhook result.

## Later releases

```sh
python3 upgrade.py \
  --archive /path/to/waveform-backend-REVISION-linux-aarch64.tar.gz \
  --sha256 ARCHIVE_SHA256 \
  --source-revision FULL_40_CHARACTER_GIT_REVISION \
  --backup-bucket EXISTING_ENCRYPTED_BACKUP_BUCKET
```

It requires a healthy native deployment whose `native.env` is on Silicon Accounts, and checks Waveform's app
credentials with Silicon Accounts before stopping anything (`--skip-accounts-check` skips that during an outage of
Silicon Accounts). It verifies the archive digest, the full source revision, every file checksum, the ARM64 ELF
format, static backend linkage and FFmpeg audio processing. It keeps the old release, link target, unit, `native.env`
and receipt under `/var/backups/waveform/upgrade-*`, and uploads a PostgreSQL dump (checked with `pg_restore --list`)
with an explicit SHA-256 checksum and server-side encryption before the switch; the host needs only its existing
backup `PutObject` permission, dumps must be under 5 GiB, and `--backup-kms-key-id` selects a KMS key.

`native.env` stays byte for byte, except that an explicit old `WAVEFORM_JSON_BODY_LIMIT_BYTES=65536` becomes
`327680`. The script switches `/opt/waveform/current` atomically, installs the bundle's unit and checks private and
public readiness. A failure restores the previous link, environment and unit, restarts the old release and checks
readiness again. It does not restore the database, so use it only for releases with no migrations or migrations the
previous release tolerates; a release with migrations needs a database restore in its rollback, as the cutover has.

## The website

```sh
python3 install-frontend.py --image 234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-waveform-production@sha256:DIGEST
```

It replaces only the `waveform-frontend` container with the Next.js website built from `web/` (a standalone Next.js
server listening on port 4325, so Caddy's route is unchanged), and does not touch the native API, Caddy, PostgreSQL
or Interface. The website signs Carbons in with Silicon Accounts and keeps each session in a sealed cookie, so the
container holds no state. Its secrets are in `/etc/waveform/web.env` (root, `0600`): `APP_SECRET` is copied from
`native.env`'s `WAVEFORM_APP_SECRET` on every run, and `SESSION_SECRET` is generated once and kept (a new one signs
every browser out). The other settings are passed as plain values: `APP_ID=waveform`, `ACCOUNTS_URL`,
`APP_API_URL=https://backend.waveform.teamofsilicons.com` and `PUBLIC_URL=https://waveform.teamofsilicons.com`.

The container runs read-only with `/tmp` and `/app/.next/cache` as small memory filesystems and a 256 MiB memory
limit. The script checks `GET /auth/session` inside the container and restores the previous unit if it does not
answer. The earlier website's session store, `/var/lib/waveform/frontend-sessions`, is no longer mounted; it is left
in place until a Carbon decides to delete it.

## First native install

`install.py` moves a container-run API to this native service, once per host. It takes the downloaded bundle, its
SHA-256, the full source revision, the secret ARN and the backup bucket, reads `WAVEFORM_APP_SECRET` from the secret
on the host, checks it with Silicon Accounts, dumps the database locally and to the backup bucket, puts the public
route into maintenance, starts the native unit and switches the route. A failure restores the previous unit and
route. The old container and database are not deleted. It refuses a host that already has a native release.
