# Implementation and validation

The human-owned `UNDERSTANDING.md` defines the product requirements. The current source implements IAM app-secret discovery, test-only public-ID login, isolated sessions and user permissions, downstream Briefcase testing credentials from OBO exchange, deterministic speech clips, durable report submission, diagnostic opt-outs, and an independent hourly CLI updater.

## What the sandbox integration verifies

Local integration tests run the actual Waveform router, PostgreSQL migrations, and official IAM/Briefcase SDKs against controlled upstream HTTP servers. They cover automatic discovery without IAM/Briefcase root keys, live revocation, clean generations, subject and plane authorization, exact-byte delegated uploads, per-voice clips, authorized replay, file reads, signed webhook routing and duplicate delivery.

Production and sandbox credentials remain separate. Test reports simulate delivery. Reports persist before acknowledgment and use scoped idempotency keys; mail retries happen in a background worker. Postmark delivery is at least once: a transport failure after acceptance may cause a duplicate notification. Account and machine telemetry preferences are independent.

The website gateway keeps tokens server-side and preserves production/test sessions. Its regressions cover app-secret selection, public-ID restrictions, cross-origin rejection, stale-tab protection, token refresh, settings and voice profiles. CLI tests exercise the compiled binary and separate session files.

## Deployment and verification boundaries

The backend and matching frontend were deployed on September 13, 2026. Migration `0010_iam_discovery.sql` is applied and readiness checks pass. Production browser login, sandbox app-secret discovery, test public-ID login and simulated reports were verified live. Space Station event delivery is verified. The existing Team of Silicons Postmark server is configured and its credential and sending domain are verified; no production test email was sent.

Live speech storage remains blocked by an upstream IAM/Briefcase contract mismatch: IAM proof verification returns only the delegated endpoint scope and no role/tag disclosure, which Briefcase requires. Controlled-fixture speech tests pass, but they do not establish live speech success. See [deployment evidence](verification-2026-09-13.md).

The docs installer builds the source snapshot distributed with the documentation. The updater checks for newer stable registry releases; it does not publish packages. Live paid speech, real email delivery, and an actual newer-version automatic installation are not exercised by local regression tests.

Historical [2026-09-08 test evidence](test-report-2026-09-08.md) applies to the older explicitly paired environment protocol. Current deployment evidence is recorded separately. Inbound OBO speech remains unavailable because IAM does not provide the downstream subject-token handoff needed for that entry mode.
