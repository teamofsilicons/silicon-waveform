# Canonical IAM compatibility deployment — September 21, 2026

Waveform's native backend 0.3.0 is deployed from `2065f5af2c71004989882bdb73bf16e0c612d403`.
Rust client 0.3.0 and CLI 0.3.1 are published; the final CLI source is
`023858ecc3f1eb22dcc4b998b9c475c0c3594d8c`. These versions accept old IAM responses
and canonical public identities without separate principal UUIDs.

## Production state and retained data

SSM deployment `9195aa39-8d1f-43b5-836d-cd69b30418a9` succeeded on the existing
Waveform host. Only the native API was paused and replaced. PostgreSQL, Caddy,
Waveform frontend and Interface containers were not changed.

The paused-writer backup is
`/var/backups/waveform/iam3-20260921T091033Z-2065f5af2c71/postgres.dump`, also stored
with AES256 encryption and a verified checksum at
`s3://silicon-waveform-standalone-backups-c9ap0zqahvyr/backups/iam3-20260921T091033Z-2065f5af2c71.dump`.
The prior native release, service unit and environment were retained.

Migration 0013 and all 20 retained actor bindings were applied in one transaction
before the new API started. Five account-data tables matched their pre-migration
row hashes exactly. All bytes of `native.env`, including encryption keys and
database credentials, were preserved. There were no saved personal provider keys
in the live database; separate PostgreSQL regression fixtures verify ciphertext
and AAD key preservation across production and two isolated test environments.

The native archive SHA-256 is
`7a6999164eec0e5ed348cc64e383723d03309fd07aabc5fad2d0d86cd8b6ee24`.
The runtime path is
`/opt/waveform/releases/2065f5af2c71004989882bdb73bf16e0c612d403/bin/waveform-api`.
API, PostgreSQL, proxy and frontend are active with zero restart counts.
Migration history reports version 13 with every entry successful.

## Live verification

Maharaj's saved `chef:bricks` session refreshed automatically using the new CLI,
without signing in again. After deployment and installation of CLI 0.3.1,
authentication succeeded and the two existing history jobs, preferences and
provider-key listing matched the pre-upgrade responses exactly. Production
readiness, liveness, capabilities and contract discovery return 200; anonymous
identity and history requests return 401. No paid speech request was made.

The consumer deployment checks above preceded the IAM canonical cutover.
After IAM 3.0.0 (`deea75e3d8f9b331bf9ef25e5d39c6546ed5a9fd`) went live,
Maharaj's same pre-cutover session authenticated and refreshed normally through
CLI 0.3.1 without another login. The actor remained `chef:bricks` in `bricks`.
Both existing jobs matched every captured response field; preferences and the
provider-key listing were unchanged. Readiness returned 200. These checks used
the existing saved refresh credentials and made no speech or configuration
mutation. No personal BYOK record was present, so live provider-key verification
covered listing and account ownership; ciphertext preservation was covered by
the earlier local database tests.

Canonical-only response handling, retained account-key resolution, revocation,
testing isolation and retry behavior also passed the local API/database and
native CLI integration tests.

## Published artifacts

[GitHub v0.3.1](https://github.com/teamofsilicons/silicon-waveform/releases/tag/v0.3.1)
and Honeycomb publish the validated six-platform archive, 24,578,414 bytes with
SHA-256 `04e5e4db376a16b4b6f10fdc8a61628518b7be87b0b2a16c3561dec9b3545143`.
Honeycomb release ID is `74f98b5d-9cbc-42f3-82d0-70542eb79b38`; a fresh anonymous
installation and Maharaj's existing installation both report `waveform 0.3.1`.
Rust packages `silicon-waveform-client 0.3.0` and `waveform-cli 0.3.1` are available.

All six native builds and package validation passed in
[release CI](https://github.com/teamofsilicons/silicon-waveform/actions/runs/35581460319).
Both exact Linux artifacts also ran on Amazon Linux 2023 with GLIBC 2.34, executing
version output and live IAM discovery in the
[baseline smoke](https://github.com/teamofsilicons/silicon-waveform/actions/runs/35582492950).
The earlier 0.3.0 GNU CLI artifacts require GLIBC 2.39; 0.3.1 fixes this with static
musl linkage. Existing immutable 0.3.0 artifacts were retained and annotated.

The native backend build, source CI, 169 backend unit tests, 13 database/control
integration tests, 14 client tests and 21 CLI tests passed. Strict Clippy and
formatting passed. Deployment tests cover pause-before-backup ordering,
restoration after failures, atomic schema/import rollback, exact replay and SQLx
checksum drift. The docs site, canonical OpenAPI and downloadable source archive
were deployed and their public content and checksum verified.
