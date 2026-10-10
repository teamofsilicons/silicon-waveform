# First AWS deployment (2026-09-08)

Moved from `deploy/aws/README.md` when Waveform moved to Silicon Accounts; it describes the first server activation
as it was then.

## First deployment status

The backend voice-profile release and frontend were published on 2026-09-08.
Production IAM sign-in, the 30-profile selector, live Gemini generation with
Puck, Briefcase storage, and profile revision history are verified. Transcription
currently awaits two delegated-read registrations in Briefcase's IAM catalog;
see the [publication report](../deployment-verification-2026-09-08.md).
The following notes describe the initial server activation before that release.

Infrastructure, DNS, a trusted HTTPS certificate, PostgreSQL, and all four
user-supplied provider keys were installed on 2026-09-08. Production IAM accepts the existing `tos>waveform` application credentials
after correcting a deployment import that retained dotenv quotation marks.
The API is active and public liveness, readiness, and capabilities checks return
200. Protected account, jobs, and provider-key routes reject unauthenticated
requests with 401 when the required organization header is present. A malformed
login code returns 400. All three services and the backup timer are active, and
the first database dumps were verified in S3. ElevenLabs also returned
`payment_required` for a short synthesis check. Its account-read permission is
restricted; successful synthesis remains unverified until the billing issue is resolved. Gemini, OpenAI and Deepgram credential checks succeeded.

The initial production readiness check exposed an FFmpeg stdin pipe that was
left open. Closing the pipe before waiting for FFmpeg fixed readiness; a real
FFmpeg regression test now covers the behavior. Validation after the fix passed
141 backend unit tests (5 database-dependent tests excluded from this run),
formatting, and Clippy. Earlier full integration results are recorded separately
in `docs/test-report-2026-09-08.md`. A successful real-user production login and
paid end-to-end provider generation have not been verified in this deployment.
