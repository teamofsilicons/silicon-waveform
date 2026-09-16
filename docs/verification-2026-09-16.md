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

The six-platform release workflow was added but not executed. A real distributable
archive requires all six native binaries and successful `honeycomb validate` /
`honeycomb pack`; no placeholder binaries or synthetic release archive were used.
The new participant credential and Honeycomb registry entry are deployment
configuration, and live shared-environment readiness still depends on IAM and
Honeycomb. Controlled HTTP fixtures do not establish live paid speech, real email,
upstream deployment compatibility or a successful production rollout.
