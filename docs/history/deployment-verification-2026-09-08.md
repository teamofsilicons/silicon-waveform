# Production publication — 2026-09-08

The complete Waveform backend and SolidJS frontend were committed and pushed to
`main`, then published on the existing standalone AWS instance. The frontend is
at https://waveform.teamofsilicons.com and the API is at
https://backend.waveform.teamofsilicons.com. Both use trusted HTTPS through Caddy.
No load balancer was added. Immutable image references, source revisions and
previous images are recorded in `deploy/aws/deployment.json`.

## Deployment checks

- Created and checked PostgreSQL backups before and after the migration.
- Applied migration 0009 successfully; all nine migrations report success.
- Confirmed 30 production voice profiles and the Kore workspace default.
- Verified API, frontend, PostgreSQL, proxy, and backup timer are active.
- Verified only the HTTPS proxy publishes host ports; frontend and API stay on
  the private Docker network. Existing secrets and database were retained.
- Added the Namecheap `waveform` A record, TTL 300, at the existing Elastic IP.
  A before/after comparison confirmed zero removed or changed unrelated records.
- Public frontend, API liveness/readiness and capabilities return 200; anonymous
  voice-catalog requests return 401. Cross-origin login returns 403, stale-context
  mutation returns 409, and invalid code with a valid context returns 400.
- Confirmed `__Host-waveform`, Secure, HttpOnly, SameSite=Lax session cookies,
  no-store session responses, CSP, and HSTS.

## Browser and provider checks

Production IAM login completed using the account's existing Team of Silicons
application authorization. The live check found and fixed an obsolete `org_id`
parameter in the IAM redirect. IAM now controls organization consent; Waveform
retains its requested workspace server-side and verifies access after login.
The fix has a regression assertion in the passing 15-test gateway suite.

The authenticated composer displayed all 30 profiles. A 59-character deployment
sample using the per-request Puck profile completed through the real Gemini
provider, producing 4.8 seconds of audio stored in Briefcase. History showed
`puck · revision 1`, older production jobs remained visible, and account settings
retained Kore after the per-request override.

A transcription attempt using that same generated file failed closed before
provider processing: production IAM's Briefcase catalog currently contains only
`briefcase.files.create`. The deployed Briefcase read route exists and rejects
unauthenticated requests with 401, but these documented registrations are absent:

- `briefcase.entries.list` → `/api/v1/obo/entries/list`, metadata `{}`
- `briefcase.files.read` → `/api/v1/obo/files/read`, metadata `{}`

Both additions are prepared in IAM, preserving the existing upload definition,
and are awaiting explicit confirmation to expand the delegated-access catalog.
No authorization bypass or direct private-file URL fetch was introduced.

## Validation and remaining limits

The previous implementation verification passed 190 automated tests. This
publication rebuilt both ARM64 images; the frontend's 15 tests, TypeScript check,
and production build passed again after the IAM redirect correction. Local
`cargo deny check` passed all advisory, dependency, license and source policies.
The initial GitHub workflow failed its two real FFmpeg tests because the runner
lacked FFmpeg; a CI-only follow-up installs that runtime before testing.
[CI run 34230225293](https://github.com/teamofsilicons/silicon-waveform/actions/runs/34230225293)
then passed both quality and production-container jobs for commit `56bc1a4`.
The final documentation/metadata-only commit does not change either deployed image.

Cross-provider acoustic similarity is still awaiting listening review. This
release did not retest the previously reported ElevenLabs billing restriction.
