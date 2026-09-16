# September 16 local verification

This run implements the updated human-owned requirements without editing their
content. Existing source changes and the requirements were checkpointed first.
Changes are committed locally; no publication or deployment was performed.

## Validated

- Backend: 151 ordinary tests passed, plus 11 isolated PostgreSQL control,
  lifecycle and consumer-contract integrations.
- PostgreSQL idempotency and lease concurrency: all 8 tests passed against a fresh
  dedicated database. The existing local database had a migration-9 checksum from
  earlier development; it was preserved and was not reset or rewritten.
- Rust client: 11 tests passed, including unversioned contract discovery after
  retirement and the disabled runtime updater compatibility path.
- CLI: 4 unit and 8 compiled-binary integration tests passed.
- Frontend: all 16 gateway tests passed; TypeScript checking and production build
  passed. The environment page now directs lifecycle management to Honeycomb.
- Backend, client and CLI formatting and Clippy passed with warnings denied.
- Dependency policy passed after upgrading Rustls to 0.23.45 in all three lockfiles.
- Documentation built with local links, navigation anchors and canonical URLs
  checked. Bundled CLI guides match their canonical sources.
- OpenAPI, CI, release workflow and Honeycomb manifest parse as YAML.
- The release packager accepted the genuine local macOS aarch64 binary and rejected
  incorrect architectures/platforms and missing target executables.

Lifecycle integration covers service-token authentication, exact retries, changed
payload rejection, a durable pending barrier, draining active work before clean
completion, preserving the environment link, no repeated clean on replay,
disable/restore, key-version rejection, permanent removal and stale-operation
protection. Contract tests cover older header-free consumers, negotiation errors,
active-successor requirements, traffic resets, sandbox isolation and seven-day
sunset with persistent unversioned discovery.

## External verification still required

Six native CLI targets have now built and passed their tests. Downloaded artifacts
were checked for native OS/CPU formats and matching manifests, then assembled
with `honeycomb pack`; both the staging directory and final archive passed
`honeycomb validate`. No placeholder binaries were used.
The new participant credential and Honeycomb registry entry are deployment
configuration, and live shared-environment readiness still depends on IAM and
Honeycomb. Controlled HTTP fixtures do not establish live paid speech, real email,
upstream deployment compatibility or a successful production rollout.

## Push reconciliation

Before pushing, `origin/main` had advanced through PR #5. The merge preserves its
Briefcase 1.1.0 integration, verified private transcription-source targets, IAM
session mutation receipts, and published client 0.1.1 / CLI 0.1.2 versions. The
Honeycomb manifest now uses CLI release 0.1.2; backend and library versions remain
independent. Legacy test fixtures seed pre-existing planes directly because
creation now belongs to Honeycomb.

Merged verification passed: 154 backend unit tests, 2 Briefcase contract tests,
13 PostgreSQL control/lifecycle/source-target integrations, 8 PostgreSQL transaction
tests, 11 client tests, and 13 CLI tests. Backend strict Clippy and dependency policy
passed, and all 24 documentation pages passed the link/canonical checks.

## Honeycomb build artifacts

Build source: `9a8dece62e3b7f887a00b37bb93d4a043322bb7a`, CLI release 0.1.2,
application `tos>waveform`. The [six-platform release run](https://github.com/teamofsilicons/silicon-waveform/actions/runs/35083867470)
builds and tests Linux, Windows and macOS on x86_64 and aarch64. Each native
artifact contains `honeycomb.yaml`; the final archive includes the same manifest
and all six binaries, with archive and binary SHA-256 files supplied separately.

The initial Windows runs exposed a Unix-only Space Station dependency and calls.
They are now compiled only on Unix; Windows retains the speech/authentication
client and account preference APIs. Git attributes preserve LF manifest bytes
on Windows so cross-platform artifact identity checks remain exact.

Frontend tests (16), production build, and all 24 docs-page checks passed locally.
Downloaded frontend and docs artifacts include the canonical manifest. CI builds
both API and frontend images and compares their embedded manifests against the
root file. Production rollout and Honeycomb publication remain on hold.
