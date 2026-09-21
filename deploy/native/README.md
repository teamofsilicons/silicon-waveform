# Native backend deployment

The API runs directly as `/opt/waveform/current/bin/waveform-api` under
`waveform-api.service`, using the dedicated `waveform` account. Docker is not
the API runtime. Existing PostgreSQL, frontend and shared Caddy containers remain
separate infrastructure; the API unit follows the PostgreSQL service lifecycle.

Run the **Build native backend bundle** GitHub workflow. It compiles a static
ARM64 musl backend from the selected revision and packages FFmpeg with its native
loader and shared libraries. The audio wrapper gives FFmpeg its own library path
without changing host libraries. The bundle includes `honeycomb.yaml`, source
revision metadata, a systemd unit and per-file SHA-256 checksums. It is a server
deployment artifact; the Honeycomb CLI package remains the six-platform release.

`install.py` performs the first migration from the existing container deployment.
Supply the downloaded archive, its SHA-256, full source revision, existing secret
ARN and backup bucket. The operator must first place the accepted IAM credential
in the existing Secrets Manager record while retaining all unrelated values.
The installer reads secrets on the host and does not accept secret values in
command-line arguments.

Before stopping the API it verifies the archive and all members, runs an FFmpeg
audio probe, authenticates the app with IAM, and writes a consistent PostgreSQL
dump locally and into the encrypted backup bucket. It preserves the old systemd
unit, proxy configuration and private environment file for rollback. The native
API binds only the private Docker bridge gateway, where the HTTPS proxy can reach
it; it does not expose another public listener.

Database TLS remains `verify-full`. A privileged preparation step discovers the
current PostgreSQL container address, creates a service-private hosts file and
copies the public CA into `/run/waveform`. The API itself runs unprivileged with
read-only system directories, a private temporary directory, and bounded memory.
It has no permission to inspect Docker or read the root-owned environment files.

After a short maintenance response, the installer stops the old backend, starts
the native unit, verifies private readiness, reloads only the backend upstream in
the existing Caddy configuration, and verifies public readiness. Failure restores
the previous service and route; the valid IAM credential remains available to
the restored backend. The old container and database are not deleted.

Inspect `/etc/waveform/native-release.json` for the deployed artifact and backup.
For subsequent native upgrades, use `upgrade.py` on the host as root:

```sh
python3 upgrade.py \
  --archive /path/to/waveform-backend-REVISION-linux-aarch64.tar.gz \
  --sha256 ARCHIVE_SHA256 \
  --source-revision FULL_40_CHARACTER_GIT_REVISION \
  --backup-bucket EXISTING_ENCRYPTED_BACKUP_BUCKET
```

The upgrade requires a healthy existing native deployment. It verifies the
archive digest, full source revision, every file checksum, ARM64 ELF format,
static backend linkage and FFmpeg audio processing before stopping the API.
It retains the old release, symlink target, unit, private environment and receipt
under `/var/backups/waveform/upgrade-*`. A PostgreSQL custom-format dump is checked
with `pg_restore --list` and uploaded with an explicit SHA-256 checksum and S3
server-side encryption before cutover. The upload response must confirm both;
the host only needs its existing backup `PutObject` permission. Dumps must be
smaller than 5 GiB. Use `--backup-kms-key-id` if the bucket requires a particular
KMS key and the host already has permission to use it.

All existing `native.env` credentials, database settings, encryption keys and
other values remain byte-for-byte unchanged. The sole automatic configuration
update replaces an explicit old `WAVEFORM_JSON_BODY_LIMIT_BYTES=65536` setting
with `327680`; absent or custom limits are preserved. The existing private bind
and FFmpeg path must already match the native deployment layout.

The script atomically switches `/opt/waveform/current`, installs the verified
bundle's unit, and checks both private and public readiness. Failure restores
the previous symlink, environment and unit, restarts the old release, and checks
readiness again. Supporting containers and Caddy configuration remain unchanged.
This rollback does not restore the database: use this workflow only for releases
with no schema changes or separately verified backward-compatible migrations.
The provider-controls/BYOK release introduces no new migrations.

Run `python3 deploy/native/test_upgrade.py` for local checks, including simulated
public-readiness failure and restoration of the prior release and private files.
The initial `install.py` and legacy Docker installer continue to refuse an
already-native host.

## IAM 3 identity cutover

For migration 0013 use `canonical_upgrade.py` beside the pinned `upgrade.py`,
with the same archive, checksum, source revision and backup bucket arguments,
plus `--identity-map`, `--migration` and `--importer` paths. Supply the private
refreshed IAM export, exact `migrations/0013_canonical_actor_keys.sql` from the
runtime source, and `scripts/import-iam-identities.py` from this deployment.

This bounded upgrade requires the old IAM contract while rollback is possible.
It verifies application authentication, stops only Waveform API, creates and
verifies a local/encrypted remote backup, then applies migration 0013 and the
private actor bindings in one transaction before starting the new API. It records
the exact SQLx migration checksum and retains all existing account rows and
provider ciphertext. Runtime configuration is unchanged. Failure restores the old
native release; the additive schema and imported private keys may remain and are
safe for old IAM. Caddy, PostgreSQL, Interface and frontend containers are untouched.

Run `test_canonical_upgrade.py` for pause/restore ordering and
`scripts/test-iam-canonical-migration.py --database-url ...` for PostgreSQL checks
of atomic rollback, SQLx checksum validation and exact replay.
