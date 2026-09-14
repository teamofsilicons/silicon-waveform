# App-secret discovery verification · 2026-09-13

The updated implementation passes **207 automated tests**: 151 backend unit tests, 17 explicitly enabled PostgreSQL/HTTP tests, 10 Rust client tests, 13 CLI tests and 16 website gateway tests. All three Rust crates pass strict Clippy (`--all-targets -- -D warnings`). The website passes TypeScript checking and a production build.

## New behavior exercised

- A Waveform `ask_…` secret discovers its IAM environment online, without any stored IAM or Briefcase root key. Metadata updates and clean generations apply inside the selected plane; production preferences remain intact. Stale metadata and revoked secrets are rejected.
- TTS runs through the actual API router, PostgreSQL, official IAM SDK proof exchange and Briefcase SDK upload. IAM returns the downstream Briefcase test credential. The caller supplies only Waveform’s app_secret. Distinct uploads, selected prerecorded clips and authorized replay are checked.
- Existing active sandbox public IDs are forwarded to IAM for login. Unknown/inactive identities and production public-ID login are rejected. Missing speech scope and a bearer from the wrong plane remain rejected despite possession of the app_secret.
- Reports are simulated in testing. Identical idempotent retries replay the stored result, changed payloads conflict, credentials are rejected before storage, and the ten-per-hour limit is enforced.
- Complete raw webhook signatures are verified. Altered bodies and wrong-world keys fail. Duplicate event delivery produces one environment-scoped metadata row; raw payloads and keys are not retained.
- Website tests verify independent production/test sessions, public-ID restrictions, context switching, stale tabs, same-origin requests, refresh handling and settings.

## Additional checks

The compiled daemon was exercised in an isolated temporary home with automatic updates disabled: start, single-instance status, private directory permissions and stop. Documentation builds verify local links, heading anchors and canonical URLs. The source archive uses an explicit allowlist and an accompanying SHA-256 checksum.

PostgreSQL concurrency tests used a fresh test database because an older local database contained a historical migration checksum. No existing application data was changed to resolve that fixture mismatch.

## Limits

The automated suite uses controlled IAM and Briefcase HTTP fixtures. Its successful speech checks are separate from the live results below. Paid speech, actual Postmark delivery and replacement with a newer published CLI release were not invoked.

## Hosted deployment · September 13

The matching AWS backend and frontend are deployed. PostgreSQL migration 10 succeeded; migration checksums 1–9 matched the deployed database before activation. A database backup completed before each backend activation. Backend readiness and the production website return successful responses.

- Backend image: `sha256:428eb593a3201bef8dc8bbb5c4069befe8ba42d14b00434ceeda8b5ab06daf90`.
- Frontend image: `sha256:95d6cb9483b973a9c29538da0faddd3c0ec25166cb0207e619d4e09cc7959e11`.
- Deployment completed: `2026-09-13T12:26:22Z`.
- Release: `app-secret-20260913T114822Z-tls-v3`.

Production IAM browser sign-in and the authenticated provider-settings page work. In an isolated live IAM sandbox, Waveform discovers the environment and logs in an existing public ID using only its own app secret. No Briefcase key is supplied to Waveform. IAM returns the Briefcase app secret inside the fresh delegated exchange, and Waveform carries it only for that downstream request. Test reports return `notification: simulated`. Live negative checks reject unknown sandbox IDs (401), public-ID login in production (400), and a Briefcase secret presented as Waveform’s selector (401).

### Live speech blocker

IAM registration and server permission checks now use `obo:tos>briefcase:briefcase.files.create` for TTS and `obo:tos>briefcase:briefcase.files.read` for STT, replacing obsolete `obo.issue`. Waveform declares the exact list/read/create endpoints and identity, profile, membership and tag disclosure. A fresh login includes those permissions.

The live TTS attempt reached the simulated speech provider successfully, then failed at storage. A separate fresh exact-byte proof confirmed that IAM verification returns only `obo:tos>briefcase:briefcase.files.create`, with `org_role: null` and `tags: null`, even though the caller and receiving application declare the required disclosure. Briefcase rejects that delegated request with HTTP 403. Waveform reports the downstream failure as HTTP 503. This upstream contract needs correction before live upload, replay and source transcription can be verified; no authorization checks were bypassed.

### Telemetry and mail configuration

Space Station recording is configured with a dedicated Waveform table and a writable private spool. Five live events were visible after the TLS correction. The existing Team of Silicons Postmark server is configured in Secrets Manager; its live server credential and the domain’s verified DKIM and return path were checked. The account’s ten-server limit prevented a dedicated new server, so the existing team server’s transactional stream is used. No production email was sent during verification. Configured report delivery retries are durable. No new registry packages were published.

The telemetry rollout also identified conflicting Rustls provider features from the HTTP and database dependencies. The backend and Rust client now choose an explicit provider before starting Space Station, while respecting a provider already installed by an embedding application. Regression tests exercise the mixed-feature configuration; the CLI uses the corrected client path.
