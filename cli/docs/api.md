# Waveform API

The backend base is `https://backend.waveform.teamofsilicons.com/api/v1`; local runs
normally use `http://127.0.0.1:8080`. Speech calls use `X-Org-ID`,
`Idempotency-Key`, and exactly one IAM bearer or OBO credential. `POST /tts`
accepts `{text,lang?,voice_profile?,provider_order?,auto_fallback?,provider_options?,provider_keys?}`
and `POST /stt` accepts `{file_url,language?,provider_order?,provider_keys?}`.
`provider_order` is a preferred provider prefix; omitted providers follow the
caller's account preference. TTS `auto_fallback` defaults to false, so only the
first provider is attempted. Set it to true to try the remaining providers. STT
retains its automatic provider fallback.
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

A test plane is selected with `X-Testing-Environment-Key: <Waveform IAM test app_secret>`.
IAM validates the secret and discovers the environment; no separately paired IAM or Briefcase key is needed.
`GET /testing-environment` returns safe environment metadata. A test `POST /auth/login`
also accepts an existing active sandbox identity public ID. Omission selects production,
where only issued SLTs are accepted. Failed test validation never falls back to production.
Manage discovered environment lifecycle through IAM; the old Waveform lifecycle routes apply to legacy environments.

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

## Reports and telemetry

`POST /reports` accepts `{message,pr?,client_version?}` with a bearer and `Idempotency-Key`. It returns 202 with the report ID and `notification` (`queued`, `sent`, or `simulated`). Retry identical content with the same key; changed content returns 409. The limit is ten reports per actor per hour. Sandbox delivery is always simulated.

`PATCH /preferences` accepts `telemetry_enabled: false` for account opt-out. `GET /preferences` reports the effective boolean, which defaults to true. Backend diagnostics honor it; the CLI also has a local opt-out.

## Contract negotiation

See [API compatibility](api-contracts.md) for `GET /api/contracts`, version headers, consumer compatibility and the seven-day sunset policy. Public environment lifecycle mutations return `409 manage_environment_in_honeycomb`; use the [Honeycomb participant](honeycomb-lifecycle.md) from the coordinator.
## Preparing an uploaded transcription source

`POST /stt/source-target` with `{}` accepts a bearer, selected `X-Org-ID`,
`Idempotency-Key`, and the same optional test selector as speech. It requires
current STT and `self.identity.read` scope, and IAM must approve the exact
Briefcase `briefcase.entries.list` delegation. It invokes no speech provider.
The response is `{org_id, app_id, actor_id, folder_id, folder_path,
testing_environment_id}`; `actor_id` is the IAM public identifier and the
world is null in production. Only an actual, writable, private folder owned by
that actor inside Waveform's configured app namespace is returned.

Briefcase intentionally hides files outside `apps/<originating-app>/…` from
OBO callers, even when the represented actor owns the original. For a normal
private upload, first read the original using that actor's ordinary Briefcase
session, prepare the target, and copy the bounded bytes with normal Briefcase
`POST /uploads` using `parent_id`. Preserve the original and transcribe the
copy's returned permanent URL. Verify all returned actor, organization, world,
folder and file identities. Use stable copy name/idempotency key/body across
uncertain responses and recheck the original access on retries. This endpoint
neither broadens the namespace fence nor grants another actor access.

This is an additive v1 operation; existing speech schemas and published client
operations remain unchanged. The authoritative operation catalog is OpenAPI;
there is no separate Waveform operation-version negotiation endpoint.


## Provider-specific controls and request keys

TTS `provider_options` accepts one provider object matching the selected first
provider, and only when `auto_fallback` is false:

```json
{"text":"Welcome home.","provider_order":["gemini"],"auto_fallback":false,"provider_options":{"gemini":{"scene":"A quiet evening at home","director_notes":"Warm, relaxed delivery"}}}
```

Gemini controls are `voice`, `scene`, `audio_profile`, `director_notes`, and
`sample_context`. ElevenLabs controls are `voice_id`, `model_id`, `stability`,
`similarity_boost`, `style`, `speed`, `use_speaker_boost`, `seed`, `previous_text`,
`next_text`, and `apply_text_normalization`. OpenAI controls are `voice`, `model`,
`instructions`, and `speed`; instructions require `gpt-4o-mini-tts` explicitly.
The OpenAPI schema describes supported values and ranges. Unknown fields,
controls for another provider, and controls combined with automatic fallback
are rejected. Omitted controls use the selected profile defaults.

TTS and STT `provider_keys` accept a map such as
`{"gemini":"<private-provider-key>"}`. Each request key overrides the saved
personal key and deployment key for that same provider. Request keys never
persist to preferences, history, or idempotency results. Without a request key,
existing saved-key selection remains in effect. Never include provider keys in
URLs or diagnostic output.

Provider failures return a safe `error.message` plus optional `provider`,
`reason`, and `provider_status` fields, alongside `code` and `request_id`.
With TTS automatic fallback disabled, the response explains the selected
provider's failure so the caller can choose a different provider.

This intentionally changes the omitted TTS fallback setting for existing HTTP
callers: send `auto_fallback: true` to retain the earlier behavior. Existing saved
keys/preferences/jobs remain valid. Replaying an older completed TTS operation
requires explicit `auto_fallback: true` without new controls or credentials;
changing those inputs requires a new idempotency key.
