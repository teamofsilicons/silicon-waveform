# Silicon Waveform API documentation

This document explains every operation in the Silicon Waveform OpenAPI contract. The machine-readable contract is in [`openapi.yaml`](./openapi.yaml).

## API conventions

### Base URL

```text
https://waveform.teamofsilicons.com/api/v1
```

Waveform provides a provider-independent interface for text-to-speech and speech-to-text. TTS uses the selected provider with automatic fallback off by default. STT keeps automatic fallback. Callers receive normalized response shapes and safe provider failure details.

### Authentication

- **Bearer authentication:** IAM access token for a Carbon or Silicon.
- **OBO Access:** `X-IAM-OBO-Access-Proof` and `X-App-ID` for applications acting for an actor.
- **Organization context:** Speech operations require `X-Org-ID`.
- **Idempotency:** Speech operations require `Idempotency-Key`.

Exactly one authentication mode is accepted. Sending both Bearer and OBO
credentials, or sending only one header from the OBO pair, is a malformed
request. Waveform verifies bearer tokens with IAM introspection. OBO speech is
currently fail-closed until the released adapter threads the exact request
method, path, and body digest required by IAM's current proof-verification
contract; Waveform never forwards an unverified proof downstream.

All current generation requests are synchronous and hold the connection until success or terminal failure.

Production bearer TTS and test-plane TTS uploads are connected to the official
IAM and Briefcase SDKs. STT delegated source reads and replay access checks use
Briefcase 1.1.0. Current testing uses Waveform’s IAM test app_secret, live IAM discovery, and downstream Briefcase credentials from OBO exchange. No separate Briefcase key is entered by the caller. See [the testing guide](docs/testing.md) and [implementation evidence](docs/IMPLEMENTATION.md).

## IAM discovery

Public IAM discovery is available at `GET /iam`. It returns `app_id`,
`iam_base_url`, and `testing_environment_id` (the selected upstream IAM test UUID,
or null for production) with `Cache-Control: no-store`. No login is required;
any supplied `X-Testing-Environment-Key` must select a valid Waveform sandbox.
Application secrets and environment keys are never returned. The Rust client
exposes `iam()` and the CLI exposes `waveform iam --json`.

## Text to speech

### `POST /tts`

Converts text into speech and stores the final MP3 in Briefcase.

- **Authentication:** Bearer or OBO Access.
- **Required input:** `text`.
- **Optional input:** `voice_profile` as a catalog ID, `lang` as a BCP 47 language hint and `provider_order` as
  a provider-name array. The array is a preferred prefix; unlisted providers
  follow the caller's account order for bearer requests. OBO requests use the
  configured default order until IAM-backed account lookup is available.
  `auto_fallback` is false by default; `provider_options` controls the selected provider.
  `provider_keys` supplies request-only BYOK credentials.
- **Required header:** `Idempotency-Key`.
- **Returns:** Request ID, permanent Briefcase URL, nullable temporary URL, provider, media type, and duration.

TTS attempts only the first resolved provider by default. Set `auto_fallback: true` to try the remaining configured providers. With fallback off, use `provider_options` for the selected provider; a failure returns `502 provider_failed` with the provider, safe reason, optional upstream status, and guidance to correct the request or choose another provider. TTS does not retry a provider generation internally. The output is normalized to `audio/mpeg`.

Waveform creates the file in the represented actor's Briefcase application
folder using a name based on the canonical operation start time established by
the first successful PostgreSQL idempotency claim. The permanent URL is the
durable reference. The published one-shot upload contract returns no signed
delivery URL, so `temporary_url` is currently null in the running service.

The language hint may be ignored by providers that do not accept it. It must not change the response schema.

Text must contain at least one non-whitespace character and is limited to 4,096
Unicode scalar values while the API is synchronous. `lang` must be a valid BCP
47 tag. The hint is sent only to Gemini, as required by the product
understanding; ElevenLabs and OpenAI infer language from the text.

## Speech to text

### `POST /stt`

Transcribes an audio or video file referenced by a permanent Briefcase URL.

- **Authentication:** Bearer or OBO Access.
- **Required input:** `file_url`.
- **Optional input:** `language` as a BCP 47 hint and `provider_order` as a
  provider-name array. The array is a preferred prefix; unlisted providers
  follow the caller's account order for bearer requests. OBO requests use the
  configured default order until IAM-backed account lookup is available.
  `provider_keys` supplies request-only BYOK credentials.
- **Required header:** `Idempotency-Key`.
- **Returns:** Request ID, transcript, detected language, provider, and source duration when available.

Waveform uses OBO Access to read the source file from Briefcase as the represented actor. Supplying a URL does not bypass Briefcase permissions.

Waveform attempts its configured STT provider chain until one succeeds or all fail. Callers receive normalized text independent of the selected provider.

`file_url` must be an HTTPS permanent URL at the configured Briefcase origin.
Waveform never fetches this URL directly. Briefcase must re-authorize the actor
and return the media through its authenticated content flow. Synchronous STT is
limited to 25 MiB and the media types advertised by `/capabilities`; larger or
longer work needs a future durable-job API.

## Capabilities

### `GET /capabilities`

Returns the normalized public capabilities of Waveform.

- **Authentication:** None.
- **Returns:** TTS output format, supported language hints, STT language hints, and accepted media types.

This endpoint describes the stable Waveform contract, not each provider's private capabilities. Clients can use it to validate media before starting a request.

Capabilities are deterministic but returned with `Cache-Control: no-store` so
intermediaries cannot replay another request's `X-Request-ID`. Language arrays
describe best-effort hints accepted by Waveform rather than a guarantee that
every fallback provider supports every language.

## Idempotency and errors

The actor-scoped `GET /jobs` history endpoint accepts optional `operation`,
`limit` (1–100), and `cursor` query parameters. Rows expose `running`,
`failed`, or `completed` status; failed rows include an `error_code`, and
`finished_at` is present for terminal rows. Results are ordered newest
first and return `next_cursor` when another page exists. Treat the cursor as an
opaque value and pass it back unchanged; it encodes the last row's creation
timestamp and UUID for stable keyset pagination.

An idempotency key is scoped to the selected Waveform plane, represented actor,
organization, and speech operation for at least 24 hours; the default retention is 24 hours. It is bound
to an HMAC-SHA-256 digest of the validated request. A completed retry returns
the original operation request ID and stable result fields. Before returning a
cached STT transcript, Waveform rechecks the current actor's access to the exact
source file. A completed TTS retry rechecks access to the generated file and
returns its permanent URL with `temporary_url: null`; expiring delivery URLs
are never stored in the idempotency record. Neither replay reruns a speech provider. Reusing the key for
a different request, or retrying while the first request still owns its lease,
returns `409`; in-progress responses include `Retry-After`.

Every response also carries `Cache-Control: no-store`; transcripts, signed
temporary URLs, and correlation IDs must not be retained by intermediaries.
Every response carries `X-Request-ID`, and every speech success or JSON error
includes a request UUID. A successful idempotent replay returns the original
canonical operation UUID in both the header and body, while replay-time errors
use the current attempt UUID. Waveform also uses that current attempt UUID for
replay authorization audit calls. Errors use the common `error.code`,
`error.message`, and `error.request_id` envelope. The primary status classes are:

- `400` malformed request, credential combination, URL, or language hint.
- `401` inactive or invalid IAM credential.
- `403` actor, organization, or resource authorization failure.
- `404` Briefcase reports the exact STT source, or a completed TTS replay file,
  as missing.
- `409` idempotency mismatch or in-progress request.
- `413` synchronous text/media/audio limit exceeded.
- `415` unsupported source media.
- `429` local admission limit reached.
- `502 provider_failed` the selected TTS provider failed with fallback off; `502 providers_exhausted` all providers failed with fallback enabled or for STT.
- `503` IAM, Briefcase, PostgreSQL, or a required dependency-contract capability is unavailable.
- `504` the bounded synchronous workflow exceeded its deadline.

Provider-private response bodies, credentials, input text, transcripts, and file
URLs are never included in public errors.

## Operational endpoints

`GET /health/live` reports process liveness. `GET /health/ready` checks required
configuration, PostgreSQL, FFmpeg, provider availability, and required
dependency-contract capabilities without making billable provider calls. These
routes are intentionally outside `/api/v1`.

The required bearer speech storage contracts are implemented. Readiness reflects
local database, codec and provider configuration; it does not prove remote IAM
or Briefcase credentials are valid and does not make billable provider calls.

## Complete flows

### Text to speech

```text
Caller submits text and optional language
  -> Waveform verifies actor context
  -> Waveform attempts provider chain
  -> successful audio is normalized to MP3
  -> Waveform mints an exact-byte proof and stores MP3 in Briefcase through OBO Access
  -> caller receives a permanent URL and null temporary URL
```

### Voice-message transcription

```text
DM uploads voice file to Briefcase
  -> caller submits the permanent URL using its Waveform-issued bearer token
  -> Waveform verifies actor context and obtains Briefcase delegation
  -> Waveform resolves and reads the authorized Briefcase file with fresh proofs
  -> Waveform measures decoded source duration
  -> Waveform attempts provider chain
  -> DM sends voice message with transcript or a recorded transcription failure
```

## Remaining integration work and optional extensions

- Deployed paired IAM/Briefcase testing with real test keys and a test login remains required.
- Inbound OBO speech cannot mint a downstream Briefcase proof from the consumed incoming proof. Use a bearer token issued to Waveform for implemented speech flows.
- Voice/style controls, diarization, transcript segments, quotas and asynchronous execution are outside the current product contract.
- Jobs expose running/failed/completed state, polling, cancellation recovery and expired-lease recovery; processing itself remains synchronous and bounded.

### Voice profiles

`GET /api/v1/voice-profiles` returns the selected environment's catalog.
`PATCH /api/v1/preferences` accepts `voice_profile` to change the account default.
TTS responses and job history include a nullable `voice_profile: {id, revision}`
for the mapping used. See [voice profiles](docs/voice-profiles.md) for fallback semantics.

## Provider controls and bring your own key

TTS `auto_fallback` defaults to `false`, including requests from existing clients
that omit the field. To retain the previous behavior, explicitly send `true`.
`provider_order` still selects the first provider and the optional fallback order.
Existing account preferences, encrypted provider keys and completed jobs remain
valid. Existing idempotent TTS jobs can replay with `auto_fallback: true` and no new
controls/keys. Use a new idempotency key when changing the provider, fallback mode,
controls or request credentials. No saved-key migration is needed.

```json
{
  "text": "Welcome back. Your table is ready.",
  "provider_order": ["gemini"],
  "auto_fallback": false,
  "provider_options": {
    "gemini": {
      "scene": "A quiet restaurant in the evening",
      "audio_profile": "A friendly host",
      "director_notes": "Warm and unhurried, with a pause after the greeting"
    }
  }
}
```

Only controls belonging to the selected provider are accepted, and nonempty
controls require fallback off. Omitted settings inherit the chosen voice profile.

| Provider | Controls |
| --- | --- |
| Gemini | `voice`, `scene`, `audio_profile`, `director_notes`, `sample_context` |
| ElevenLabs | `voice_id`, `model_id`, `stability`, `similarity_boost`, `style`, `speed`, `use_speaker_boost`, `seed`, `previous_text`, `next_text`, `apply_text_normalization` |
| OpenAI | `voice`, `model`, `instructions`, `speed` |

Gemini scene/profile/direction become labeled prompt sections before the transcript;
they are guidance, not guarantees about delivery. ElevenLabs stability, similarity
and style accept 0–1; speed accepts 0.7–1.2; normalization accepts `auto`, `on`, `off`.
OpenAI speed accepts 0.25–4. Instructions require explicit `gpt-4o-mini-tts`;
`tts-1` and `tts-1-hd` reject instructions. Unknown or mismatched controls fail
validation before generation. See OpenAPI for complete bounds.

For saved BYOK, use the existing write-only `PUT /provider-keys/{provider}` with
`{"api_key":"<private-key>"}`. Keys are encrypted and scoped to environment,
organization and actor; listing returns metadata only. Delete a saved key to
return to the deployment key. For a single TTS or STT request, supply
`provider_keys: {"gemini":"<private-key>"}` or another supported provider.
Selection is request key → saved actor key → deployment key, independently for
each provider. A rejected personal key never silently retries using Waveform's
key. Request keys are not saved, returned, or logged; only a keyed request digest
binds them to idempotency. Providers charge the account owning the chosen key.

Test environments validate the same request fields but always use prerecorded
profile audio or the fixed transcript, including every selected TTS provider.
Saved or request keys never invoke a paid provider in a test environment.

`GET /capabilities` exposes supported control names, the default fallback mode
and BYOK precedence. Failures only expose allowlisted reasons and status codes;
provider response bodies, speech text and credentials are never reflected.

Provider documentation verified for these mappings:
- [Gemini speech generation](https://ai.google.dev/gemini-api/docs/speech-generation)
- [ElevenLabs speech endpoint](https://elevenlabs.io/docs/api-reference/text-to-speech/convert)
- [OpenAI speech endpoint](https://developers.openai.com/api/reference/resources/audio/subresources/speech/methods/create)
