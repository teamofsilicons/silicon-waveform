# Implementation and validation

The human-owned `UNDERSTANDING.md` defines the product requirements. The current source implements IAM app-secret discovery, test-only public-ID login, isolated sessions and user permissions, downstream Briefcase testing credentials from OBO exchange, deterministic speech clips, durable report submission, diagnostic opt-outs, and Honeycomb-owned CLI installation and updates.

## What the sandbox integration verifies

Local integration tests run the actual Waveform router, PostgreSQL migrations, and official IAM/Briefcase SDKs against controlled upstream HTTP servers. They cover automatic discovery without IAM/Briefcase root keys, live revocation, clean generations, subject and plane authorization, exact-byte delegated uploads, per-voice clips, authorized replay, file reads, signed webhook routing and duplicate delivery.

Production and sandbox credentials remain separate. Test reports simulate delivery. Reports persist before acknowledgment and use scoped idempotency keys; mail retries happen in a background worker. Postmark delivery is at least once: a transport failure after acceptance may cause a duplicate notification. Account and machine telemetry preferences are independent.

The website gateway keeps tokens server-side and preserves production/test sessions. Its regressions cover app-secret selection, public-ID restrictions, cross-origin rejection, stale-tab protection, token refresh, settings and voice profiles. CLI tests exercise the compiled binary and separate session files.

## Deployment and verification boundaries

The backend and matching frontend were deployed on September 13, 2026. Migration `0010_iam_discovery.sql` is applied and readiness checks pass. Production browser login, sandbox app-secret discovery, test public-ID login and simulated reports were verified live. Space Station event delivery is verified. The existing Team of Silicons Postmark server is configured and its credential and sending domain are verified; no production test email was sent.

Live speech storage remains blocked by an upstream IAM/Briefcase contract mismatch: IAM proof verification returns only the delegated endpoint scope and no role/tag disclosure, which Briefcase requires. Controlled-fixture speech tests pass, but they do not establish live speech success. See [deployment evidence](verification-2026-09-13.md).

The compatibility installer delegates to Honeycomb. Rust client requests never update dependencies. The release workflow builds six native CLIs and validates one Honeycomb archive; local checks do not establish that every cross-platform binary has been built or published.

The September 16 source changes add the [Honeycomb participant](honeycomb-lifecycle.md), remove autonomous environment retirement and public lifecycle mutations, and add [API contracts](api-contracts.md). These changes have not been deployed. Lifecycle credentials and the Honeycomb participant registration must be provisioned by the deployment operator before live coordination can work.

Historical [2026-09-08 test evidence](test-report-2026-09-08.md) applies to the older explicitly paired environment protocol. Current deployment evidence is recorded separately. Inbound OBO speech remains unavailable because IAM does not provide the downstream subject-token handoff needed for that entry mode.

## September 20 TTS controls and BYOK source update

TTS now defaults to one selected provider, with explicit automatic fallback.
Provider-specific controls, actionable safe failure details, and request-only BYOK
are available in the API, Rust client, CLI and website. Existing encrypted saved
keys remain supported with request → saved → deployment precedence. STT retains
automatic fallback. These source changes require a backend/client/frontend release;
local verification is not evidence that they have been deployed.
