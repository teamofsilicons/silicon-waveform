# Waveform 0.1.0 publication and deployment — 2026-09-09

Source commit `fb9a95bf5d9084ee0827f4d961b4019bcadf59e9` is pushed to main
and published as [v0.1.0](https://github.com/teamofsilicons/silicon-waveform/releases/tag/v0.1.0).
Both [silicon-waveform-client](https://crates.io/crates/silicon-waveform-client/0.1.0)
and [waveform-cli](https://crates.io/crates/waveform-cli/0.1.0) are available at
version 0.1.0. A clean registry installation of the CLI passed version, help,
IAM discovery, and unauthenticated login-status checks. The CLI defaults to the
production backend; `--url` or `WAVEFORM_URL` selects another server.

The ARM64 backend image is deployed by immutable digest, recorded alongside its
previous image in `deploy/aws/deployment.json`. Activation completed at
2026-09-09 09:40:11 UTC on the existing standalone AWS instance. Database backups
succeeded before and after activation; all nine existing migrations were successful.
No new migration was included. The API service was restarted, while the frontend,
proxy and PostgreSQL services retained their images and running processes.
Rollback copies of the previous API unit and image configuration are retained at
`/var/backups/waveform/release-fb9a95b` on the host.

## Verification

- Public API liveness, readiness, capabilities, and IAM discovery returned 200.
- `GET /api/v1/iam` returned `app_id: tos>waveform`, the configured IAM base URL,
  and a null production test-environment ID, with `Cache-Control: no-store`.
- Anonymous `GET /api/v1/auth/me` returned 401.
- The frontend returned 200, and all application services and the backup timer
  were active after activation.
- The registry-installed `waveform iam --json` worked against production without
  URL overrides. `waveform login status --json` correctly reported an empty
  isolated local session as unauthenticated.
- Local validation passed 186 backend, database, client and CLI tests, plus
  formatting, Clippy, and dependency-policy checks. Both jobs in
  [GitHub CI run 34334304623](https://github.com/teamofsilicons/silicon-waveform/actions/runs/34334304623)
  passed for the source commit.

Authenticated carbon/silicon status and production/test isolation are covered by
automated protocol tests. This deployment did not create a fresh user session
or run paid speech generation.
