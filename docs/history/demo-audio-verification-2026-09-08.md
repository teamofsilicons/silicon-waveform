# Demo audio and unscoped login release — 2026-09-08

Application commit `1d7816b4645753dec542704969a4e8fb3e885ca7` was pushed to
`main` and deployed at 13:36:06 UTC on the existing standalone AWS server.
Immutable API/frontend images and rollback references are in
`deploy/aws/deployment.json`. A PostgreSQL backup completed before activation.

## Prerecorded test audio

Generated one Gemini recording for each of the 30 voice profiles using the fixed
test-environment message from `UNDERSTANDING.md`. Each file is mono MP3 at
44.1 kHz and 96 kbps. All files decoded successfully and were transcribed with
Deepgram for content verification. Normalized transcript agreement was at least
98.79%. The catalog occupies 5,724,642 bytes; individual clips are approximately
14–18 seconds long. Kore, the default, is 15,230 ms.

The checked-in catalog, generation metadata, hashes, durations and QA transcripts
are in `src/infrastructure/test-audio`. Test requests select the prerecorded
clip for their voice profile from PostgreSQL; they do not generate new provider
audio. Startup seeds the 30 `tts-v2:<voice>` rows atomically. Unknown voices fail
closed. The existing test STT transcript is unchanged.

After deployment, all 30 PostgreSQL audio hashes and byte lengths matched the
checked-in manifest. API, frontend, PostgreSQL, proxy and backup timer were active.

## Login

The production sign-in dialog has only **Continue with IAM**. Its redirect,
token exchange and initial identity lookup are unscoped. Subsequent workspace
requests use the organization returned by verified IAM authorization. The
test environment retains its separate short-lived test-token login.

A live browser check completed IAM login using the account's existing
application access and returned to the authenticated Team of Silicons workspace.
The composer displayed all 30 profiles and the Kore account default. The IAM
redirect contained only the application ID and callback URI, without an
organization parameter.

## Automated verification

- 155 backend/control tests and 8 PostgreSQL integration tests passed, including
  normally ignored database tests, against a fresh disposable database.
- 15 frontend gateway tests, strict TypeScript and the production build passed.
- Formatting, Clippy with warnings denied and both ARM64 image builds passed.
- New coverage verifies the complete MP3 catalog, profile-specific uploads,
  idempotent replay, unscoped identity and rejection of an incorrect audience.

The first release CI run exposed a test polling race: PostgreSQL activity
statistics were cached in the blocking transaction before a competing query
started. The polling helper now clears its statistics snapshot before reading.
The regression primes an early snapshot deliberately. All 163 backend/database
tests and Clippy passed again after this test-only correction.

This verification did not execute a new paired live test-environment speech
round trip. The separate missing Briefcase read registrations described in
`deployment-verification-2026-09-08.md` remain pending. These Gemini samples do
not establish acoustic similarity with OpenAI or ElevenLabs fallback voices.
