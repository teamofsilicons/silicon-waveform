# Waveform API

The backend base is `https://backend.waveform.teamofsilicons.com/api/v1`; local runs
normally use `http://127.0.0.1:8080`. Speech calls use `X-Org-ID`,
`Idempotency-Key`, and exactly one IAM bearer or OBO credential. `POST /tts`
accepts `{text,lang?,voice_profile?,provider_order?}` and `POST /stt` accepts
`{file_url,language?,provider_order?}`. `provider_order` is a preferred
provider prefix; omitted providers follow the caller's account preference.
Both routes are synchronous and return normalized provider-independent results.

`GET /iam` is public discovery: it returns the configured `app_id`,
`iam_base_url`, and `testing_environment_id` (the IAM sandbox UUID, or null in
production). It never exposes application secrets or environment keys, sends
`Cache-Control: no-store`, and validates any supplied test selector.

Control routes include `POST /auth/login` (an IAm SLT only),
`POST /auth/refresh`, `POST /auth/logout`, `GET /auth/me`, provider preference and
write-only personal-key routes, and `GET /jobs`. The signed IAM receiver is
`POST https://backend.waveform.teamofsilicons.com/webhooks/`; it verifies the
four `X-Silicon-IAM-*` headers over the exact raw bytes, deduplicates by event ID,
and never persists event payloads or secrets.

A test plane is selected explicitly with `X-Testing-Environment-Key`. Its root
key is distinct from the IAM and Briefcase test keys, and omission always means
production. `POST /testing-environments` stores upstream keys encrypted under an
independent deployment key; `GET /testing-environment` and `/clean` require the
selected root key.

Bearer TTS and test-plane TTS use the published SDK upload and return a
permanent `file_url` with `temporary_url: null`. STT reads and replay access
checks use Briefcase delegated listings and reads. Inbound OBO speech remains
unavailable; refer to
`IMPLEMENTATION.md` before treating any route as deployed-ready. `GET /auth/me`
may omit `X-Org-ID`, in which case the organization comes from IAM.

The specification spelling `POST /webhook/` is also accepted as an alias for
`POST /webhooks/`, using the same signature verification.

Speech callers may provide a non-nil UUID in `X-Request-ID` to know the initial
job identifier before the synchronous response. Once authorization succeeds and
an idempotency lease is acquired, `/jobs/{id}` exposes the running row. A retry
uses the original canonical job ID even if the transport request ID differs.
An initial polling 404 can therefore mean that authorization is still pending;
the client and CLI wait helpers retry it within their configured timeout.

Job state and idempotency completion are committed together. Provider failures
mark the owned attempt failed; cancellation releases its lease. Crashed attempts
become `failed` with `request_interrupted` once their database lease expires.
History reads and scheduled maintenance perform that recovery. Old, unleased
running rows from previous development builds are recovered after one day.

STT measures decoded source duration locally before invoking providers, so job
history includes `duration_ms` even when the selected provider omits it. Invalid
audio is rejected before provider work. Test planes use the same audio inspector
and report the uploaded source duration while returning the fixed transcript.

Voice profiles: `GET /voice-profiles` lists mappings; `PATCH /preferences` with
`{"voice_profile":"puck"}` sets the account default. See [voice profiles](voice-profiles.md).
