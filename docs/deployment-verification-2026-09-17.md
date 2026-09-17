# Native production backend — September 17, 2026

Production now runs the native ARM64 backend directly under systemd. The source
revision is `e26f86a5467209fc31e3d3012a47f385a4a52870`; the migration installer
is from `4a0e849ea5879a8cb996cb6889c0fc851f833ba5`.

The [native bundle build](https://github.com/teamofsilicons/silicon-waveform/actions/runs/35158306148)
passed, including static-musl backend compilation and the packaged FFmpeg audio
probe. [CI](https://github.com/teamofsilicons/silicon-waveform/actions/runs/35189468477)
passed quality, container and web-build checks. The deployed archive SHA-256 is
`164e40a2f81628520b723e93c55c1230492550cdaee9ca3626d4379dc0ee927c`.

The installer authenticated the corrected `tos>waveform` application credential
with IAM before the cutover. It preserved unrelated Secrets Manager values,
created a PostgreSQL dump locally and in the encrypted backup bucket, and saved
the previous service, environment and proxy configuration. The old API container
is stopped and retained for rollback. PostgreSQL, frontend and shared Caddy
continue in their existing containers.

SSM deployment `da36ab2b-1b08-4c88-bda7-4af05d5936da` succeeded. The maintenance
route was applied at 06:29:03 UTC and the native upstream at 06:29:06 UTC.
Host verification established:

- `waveform-api.service` is active, runs as `waveform`, and has zero restarts.
- Its executable is the verified release's `bin/waveform-api`; its process is
  directly in `/system.slice/waveform-api.service`.
- The old `waveform-api` container is exited. PostgreSQL, proxy and frontend
  services remain active.
- Bundled FFmpeg generates an audio signal successfully as the service user.
- Database migrations 10–12 report success; database TLS remains `verify-full`.
- Public readiness and contract discovery return HTTP 200. Anonymous identity
  access returns HTTP 401, as required.

The receipt is `/etc/waveform/native-release.json`; the recoverable backup is
`/var/backups/waveform/native-20260917T062901Z`. See
[the native deployment guide](../deploy/native/README.md) for runtime and rollback
details.

The application credential preflight succeeded, but the reported `testsi` login
has not yet been repeated: its Silicon/IAM home was not supplied. An attempt to
obtain a token using the selected local Carbon session stopped at IAM's scope
consent prompt; no additional scopes were approved. This deployment does not
establish paid speech-provider success or Honeycomb public-release approval.
