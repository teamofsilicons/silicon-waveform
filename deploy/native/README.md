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
Subsequent upgrades should stage a new verified bundle and preserve the prior
native release before switching the `current` symlink; this initial migration
tool deliberately refuses an already-native host. The legacy Docker installer
also refuses to overwrite a native deployment.
