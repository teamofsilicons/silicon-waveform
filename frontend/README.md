# Silicon Waveform frontend

A minimal SolidJS + TypeScript application styled from the **live Silicon IAM
console** at https://iam.teamofsilicons.com, inspected on 2026-09-08. It uses the
same local Plex fonts, Silicon mark, pale sidebar, thin borders, and restrained
blue actions. The docs site was not used as a visual reference.

## Run locally

```sh
cd frontend
npm ci
npm run dev
```

Open **http://localhost:4325**. Vite serves the UI; a loopback gateway on 4326
connects to the already deployed AWS backend. Use `localhost` consistently:
mutations validate the browser origin, which defaults to `http://localhost:4325`.
No production API keys or IAM application secrets belong in this frontend.

`Continue with Silicon IAM` uses the live IAM authentication frontend. It sends
the Waveform app ID and a callback containing a session-bound, ten-minute nonce.
The callback exchanges the single-use code through the Waveform backend, then
redirects to a clean URL. Signup is delegated to IAM too. A short-lived `oac_`
code can also be entered manually, including for a paired testing environment.
The server validates the actor with `/api/v1/auth/me` before accepting login.
IAM owns organization consent; the redirect supplies only the application and
callback. Waveform keeps the requested workspace in its server-side session and
checks access to that workspace after login.

## Features

| Screen               | Supported operations                                                                                                                                                                                              |
| -------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Text to speech       | Text, optional BCP 47 language, account order or per-request ordering, synchronous progress, safe retries, Briefcase audio link, link copying, optional playback when a temporary media URL is returned           |
| Speech to text       | Permanent Briefcase file URL, optional language, per-request ordering, transcript, detected language, duration, copy and text download                                                                            |
| Job history          | TTS/STT filtering, opaque-cursor pagination, refresh, automatic polling of running jobs, job detail, duration, first line, provider, timestamps, error codes and request ID                                       |
| Provider settings    | Account TTS/STT ordering, restore workspace defaults, connect/replace/remove all four provider keys and configured status                                                                                         |
| Testing environments | Create with IAM ID/key, test app secret and Briefcase key; list/detail; retrieve and copy root key; rotate; soft delete; restore; connect an existing key; clear test data; switch between production and testing |
| Account              | Carbon/Silicon identity, organization, disclosed role, IAM sign-in/signup, short-lived code login, refresh, logout, account handoff to IAM                                                                        |
| Service status       | Live backend readiness and manual retry; capabilities supply language suggestions and supported media formats                                                                                                     |

Default provider order is read from the backend, never hard-coded as a UI
fallback. The backend remains authoritative for permissions. A test root alone
allows current-environment inspection and cleanup; speech and account actions
still require a login from the paired IAM environment. Production credentials
are never reused in a test plane. Management operations belong to production.

STT accepts a **previously uploaded Briefcase URL**, matching the backend
contract; the interface links to Briefcase for uploading. A permanent Briefcase
URL is a protected file reference, not necessarily a playable media URL. The
current backend returns `temporary_url: null`, so the result links to Briefcase;
the native audio player appears only when an HTTPS temporary URL is available.
The history API provides summaries, not full prior transcripts or audio URLs.
Results stay available while navigating between screens in the same context,
but are cleared on reload or identity/environment changes.

## Gateway and sessions

The small Node gateway uses only Node built-ins. Access tokens, refresh tokens,
and connected testing keys remain in a bounded, expiring server-side session
map. The browser gets an opaque `HttpOnly`, `SameSite=Lax` cookie (`Secure` and
`__Host-` prefixed when the public origin uses HTTPS). Neither localStorage nor
sessionStorage is used for secrets. Provider-key forms clear their values after
save/close. There is no analytics or credential logging.

All non-GET requests require the configured exact origin. The gateway only
forwards an allowlist of public Waveform routes to a fixed backend origin and
constructs authentication headers itself. It does not expose application/OBO
proof operations, webhook receivers, or arbitrary proxy destinations. Session
refresh is single-flight. Private requests include the current context ID, so a
stale tab cannot silently send a production request into a newly selected test
environment (or vice versa). Public health/capability probes do not replace
session cookies. Speech requests retain the same idempotency key and request ID
when retrying the same payload after an uncertain failure.

Sessions last at most 24 hours of inactivity and live in memory. Restarting the
frontend signs its sessions out locally. Run a **single process** for this small
deployment; use shared session storage before adding multiple replicas. Tokens
remain subject to the backend's online IAM validation. Server restarts do not
remove backend accounts, jobs, preferences, or environments.

## Build and host

```sh
npm run build
WAVEFORM_FRONTEND_ORIGIN=https://waveform.teamofsilicons.com \
HOST=0.0.0.0 PORT=4325 npm start
```

Serve the Node process behind HTTPS. The build is not a standalone static site:
`/api/*`, `/auth/*`, and `/health/*` need its gateway. The server serves the built
SPA and its assets, applies a content security policy, and caches only hashed
assets for a year. Forward the public host/path through your reverse proxy.
Set `WAVEFORM_FRONTEND_ORIGIN` to the exact public origin before hosting.

Configuration (server-side environment variables):

- `WAVEFORM_BACKEND_URL`: defaults to `https://backend.waveform.teamofsilicons.com`.
- `WAVEFORM_FRONTEND_ORIGIN`: defaults to `http://localhost:4325`.
- `WAVEFORM_IAM_AUTH_ORIGIN`: defaults to `https://auth.iam.teamofsilicons.com`.
- `WAVEFORM_APP_ID`: defaults to `tos>waveform`.
- `HOST`, `PORT`: production server bind address and port.

The `.env.example` is a reference; export variables in the shell or use Node's
`--env-file` option. The frontend does not read the repository's backend `.env`.
A Dockerfile is included. The AWS installer hosts the frontend on the existing
Waveform instance at https://waveform.teamofsilicons.com; see
[deployment instructions](../deploy/aws/README.md).

## Verification

```sh
npm test
npm run build
# Optional, separate local UI fixtures; never contacts production:
npm run test:ui
```

The disposable fixture UI runs at **http://localhost:4341**, with its fake
backend on loopback port 4340. It accepts the clearly synthetic sign-in code
`oac_waveform_ui_fixture`. It is a test harness, not an application demo mode;
none of its data or routes is part of the production gateway.

Automated tests cover private credential handling, CSRF, proxy restrictions,
production/test isolation, failed connections, idempotent retry, pagination
forwarding, provider settings, key lifecycle, environment lifecycle, cleanup,
callback replay rejection, refresh concurrency, upstream errors, stale-tab
protection, and public-probe cookie isolation. Browser verification and live
integration evidence are recorded in `../docs/frontend-verification-2026-09-08.md`.

## Voice profiles

Text to speech has a profile selector with an account-default option and
expandable provider mappings. Provider settings saves the account default;
request overrides leave it unchanged. Results and history show the profile
used. The initial catalog has 30 profiles; fallback mappings are visibly marked
as awaiting listening review. No raw provider tuning controls are exposed.
Deploy the backend's `0009_voice_profiles.sql` migration before this frontend.
See [profile API and catalog management](../docs/voice-profiles.md).
