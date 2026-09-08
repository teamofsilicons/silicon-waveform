# Frontend verification — 2026-09-08

## Implemented

`frontend/` contains the SolidJS application, typed request layer, Node gateway,
production static server, development launcher, container definition, and
separate disposable test harness. No frontend was hosted or DNS changed.

The visual reference was the authenticated live IAM console at
https://iam.teamofsilicons.com, not its documentation site. The interface shares
IAM's Plex fonts, Silicon mark, gray sidebar/background, thin borders, compact
blue buttons, typography hierarchy, and production indicator.

The feature-to-screen mapping and exact hosting commands are in
[`frontend/README.md`](../frontend/README.md). All user-facing backend operations
are represented: speech, history, preferences, provider keys, IAM sessions, and
the complete testing-environment lifecycle. Backend-only webhook and OBO proof
operations are not exposed in browser forms.

## Automated checks

- TypeScript strict checking and production Vite build pass.
- 14 gateway tests pass: server-only credentials, logout, origin/CSRF validation,
  proxy allowlist, production/test identity isolation, failed connection safety,
  idempotent retries and cursor forwarding, provider ordering and key lifecycle,
  all environment management operations, root-scoped cleanup, callback nonce
  and replay checks, concurrent refresh, safe upstream errors, stale-tab context
  rejection, public-probe cookie isolation, and decoded HTTP response framing.
- `git diff --check` passes.
- Production bundle is approximately 63 kB JavaScript (20 kB gzip) and 19 kB CSS
  (5 kB gzip), plus locally served font/brand assets.

## Live integration

The real browser completed IAM's `Continue to application` flow for the
existing Waveform application and returned to a clean localhost URL signed in.
The AWS backend supplied the real account authorization and provider
preferences. The live job-history screen loaded its authenticated empty state.
The production readiness, liveness and capabilities endpoints also
returned HTTP 200 and valid JSON through the final local gateway.

No billable production TTS/STT request, provider-key change, or production
testing-environment mutation was performed during frontend browser validation.
The pre-existing ElevenLabs billing restriction remains outside this UI work.

## Browser workflows

Against the disposable fixture backend, the actual browser verified:

- Short-lived code login and authenticated state.
- TTS input, per-request ordering and completed Briefcase result link.
- STT URL submission, transcript, language, duration and download/copy controls.
- Both jobs in history and an individual job's complete metadata dialog.
- Saved account ordering reflected in settings.
- Connecting a synthetic provider key, configured status and removal.
- The full environment creation form, returned access key and connection action.
- Existing-key connection, test-only sign-in and independent test identity.
- Test-mode labeling and successful speech from the selected test context.
- Desktop layout and a 390 × 844 mobile viewport, including navigation,
  sign-in dialog, speech form, environment controls and absence of horizontal
  overflow/clipped footer buttons. The viewport override was reset afterward.

Environment rotate/delete/restore/clean and refresh are covered by automated
contract tests. These fixture tests exercise the browser gateway and API shapes;
they do not substitute for real paired IAM/Briefcase provider integration. The
prior backend's paired-environment test evidence is recorded separately in the
existing backend test report.

## Issues found and fixed

1. Language datalist IDs were made operation-specific so TTS and STT do not
   collide while both composer components remain mounted.
2. Account preference saves refresh the composers' provider order.
3. Mobile environment action buttons now stack, and closed navigation is hidden
   from keyboard/accessibility traversal.
4. Session context IDs prevent stale tabs from submitting into a different
   selected environment or identity.
5. Public service probes no longer create/overwrite authentication cookies.
6. Fetch-decoded upstream responses strip stale compression and HTTP framing
   headers. This fixes a real compressed capabilities response that otherwise
   broke the development proxy. It has a dedicated regression test.

## Limits and next step

The current backend returns protected permanent Briefcase links and no temporary
audio delivery URL. The UI therefore opens audio in Briefcase; inline playback
is conditional on an actual temporary media URL. History provides summaries,
not full historical content. The app preserves current speech results during
navigation but clears them on reload or context change.

The frontend is ready for the separate hosting step. Run the Node gateway behind
HTTPS with the exact public frontend origin configured. The initial runtime
uses bounded in-memory sessions and should remain a single process; restarting
it requires signing in again.
