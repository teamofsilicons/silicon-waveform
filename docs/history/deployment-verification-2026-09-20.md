# Waveform 0.2.0 deployment and publication — September 20, 2026

Release source `9b504e0ceea59ffa353db8144d80e50bbda6d20e` is committed and pushed
to `main` and tagged `v0.2.0`. Subsequent documentation commits record deployment
evidence; the immutable runtime artifacts remain built from this release source.

## Production services

The API is deployed as a native ARM64 process under `waveform-api.service`, from
`/opt/waveform/releases/9b504e0ceea59ffa353db8144d80e50bbda6d20e/bin/waveform-api`.
The backend archive SHA-256 is
`4e8f9bd892178b1d12c68c0b2b0936699202666ec5e73ce91c66c9d7ac0a1900`.
SSM deployment `07de17de-a774-48dc-a6b1-fd191bbb4be0` succeeded.

The frontend is deployed at the immutable ECR digest
`sha256:227e821677a82aa6fbe9c70fb6591cce7df80bff3bbe14d1b4224bb9d85e3f66`.
Its image label records the same source revision. Frontend deployment
`0d19ea0e-279b-4f05-99cb-988cf0871d32` completed at 16:21:53 UTC.
The existing frontend container hosting was retained; PostgreSQL and Caddy
were not replaced or restarted by this release.

The previous native release, frontend image, service units and environment files
were retained for rollback. The consistent PostgreSQL dump is stored locally at
`/var/backups/waveform/upgrade-20260920T162027Z-9b504e0ceea5/postgres.dump` and in
the existing backup bucket at
`backups/upgrade-20260920T162027Z-9b504e0ceea5.dump`. The 6,665,656-byte object has
AES256 server-side encryption and a verified SHA-256 checksum. Deployment receipts
are `/etc/waveform/native-release.json` and `/etc/waveform/frontend-release.json`.

Live verification established:

- API, frontend, PostgreSQL and proxy are active with zero service restarts.
- The API executable resolves to the new native release; migrations 1–12 remain
  successful. This release introduced no database migration.
- All existing runtime configuration values are preserved except the explicit
  JSON request limit, increased from 65,536 to 327,680 bytes.
- IAM accepts the deployed application credential. Verification used an inactive
  synthetic token and did not issue a new user credential.
- Public liveness, readiness, website and gateway capabilities return HTTP 200.
  Anonymous identity access correctly returns 401.
- Capabilities advertise default-off TTS fallback, provider controls and saved
  plus request-only BYOK. Contract discovery accepts 0.2.x clients.
- The actual production browser shows Automatic fallback unchecked. Its served
  JavaScript contains the new controls and corrected Voice profile result label.

## Published packages and docs

- [GitHub release v0.2.0](https://github.com/teamofsilicons/silicon-waveform/releases/tag/v0.2.0)
  is public and stable, with the native backend archive and validated six-platform
  Honeycomb archive, manifests and checksums.
- Honeycomb application `tos>waveform` is public and active at revision 1, with
  latest version 0.2.0. Release ID: `d47400ab-4ab3-4e59-afef-e569c3946b68`.
  The 24,357,252-byte archive SHA-256 is
  `7604e2e73b512e526846fdb1c89c9236b1517bd9017fbb3b4f121f26c3f5790a`.
- A fresh signed-out Honeycomb home installed the latest version successfully.
  Its native macOS ARM64 CLI reports 0.2.0, exposes fallback/provider-options/key
  flags and reads the live API contract.
- [silicon-waveform-client 0.2.0](https://crates.io/crates/silicon-waveform-client/0.2.0)
  and [waveform-cli 0.2.0](https://crates.io/crates/waveform-cli/0.2.0) are available
  and not yanked. CLI packaging compiled against the published client version.
- [Documentation](https://docs.waveform.teamofsilicons.com) is published with
  updated API examples, migration guidance, OpenAPI and installable source. The
  public source download was checked against the local archive and contains the
  0.2.0 client and CLI.

## Validation and limits

[CI](https://github.com/teamofsilicons/silicon-waveform/actions/runs/35522039450),
[native backend/frontend build](https://github.com/teamofsilicons/silicon-waveform/actions/runs/35522041485)
and [six-platform release](https://github.com/teamofsilicons/silicon-waveform/actions/runs/35522041395)
all passed. This includes Rust formatting/Clippy/tests, PostgreSQL integration,
SDK/CLI checks, dependency policy, frontend/docs and container builds, each
native CLI target's tests, and Honeycomb archive validation. The native upgrade
workflow also passed eight focused tests, including rollback after failed public
readiness. Maximum-length Unicode provider controls now have CLI file/stdin
regression coverage.

[Local E2E verification](e2e-provider-controls-2026-09-20.md) passed 32 automated
HTTP/CLI checks and 13 browser workflows through the real local gateway, API,
database and audio runtime with mocked external services. Deployment verification
did not perform paid speech generation or establish live provider acceptance of
every control. It did not change IAM permissions or test lifecycle registration.
