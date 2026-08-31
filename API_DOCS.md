# Silicon Waveform API documentation

This document explains every operation in the Silicon Waveform OpenAPI contract. The machine-readable contract is in [`openapi.yaml`](./openapi.yaml).

## API conventions

### Base URL

```text
https://waveform.teamofsilicons.com/api/v1
```

Waveform provides a provider-independent interface for text-to-speech and speech-to-text. Provider selection and fallback are internal; callers receive one normalized response shape.

### Authentication

- **Bearer authentication:** IAM access token for a Carbon or Silicon.
- **OBO Access:** `X-IAM-OBO-Access-Proof` and `X-App-ID` for applications acting for an actor.
- **Organization context:** Speech operations require `X-Org-ID`.
- **Idempotency:** Speech operations require `Idempotency-Key`.

Exactly one authentication mode is accepted. Sending both Bearer and OBO
credentials, or sending only one header from the OBO pair, is a malformed
request. Waveform verifies the selected organization and requested action online
with IAM for every speech operation. Bearer tokens use introspection and OBO
proofs use IAM's published verification operation.

All current generation requests are synchronous and hold the connection until success or terminal failure.

## Text to speech

### `POST /tts`

Converts text into speech and stores the final MP3 in Briefcase.

- **Authentication:** Bearer or OBO Access.
- **Required input:** `text`.
- **Optional input:** `lang` as a BCP 47 language hint.
- **Required header:** `Idempotency-Key`.
- **Returns:** Request ID, permanent Briefcase URL, temporary URL, provider, media type, and duration.

Waveform attempts its configured provider chain in order. Provider failures are internal implementation details unless every provider fails. The output is normalized to `audio/mpeg` regardless of the provider that succeeds.

Waveform creates the file in the represented actor's Briefcase application
folder using a name based on the canonical operation start time established by
the first successful PostgreSQL idempotency claim. The permanent URL is the
durable reference; the temporary URL is for immediate playback.

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
- **Optional input:** `language` as a BCP 47 hint.
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

An idempotency key is scoped to the represented actor, organization, and speech
operation for at least 24 hours; the default retention is 24 hours. It is bound
to an HMAC-SHA-256 digest of the validated request. A completed retry returns
the original operation request ID and stable result fields. Before returning a
cached STT transcript, Waveform rechecks the current actor's access to the exact
source file. A completed TTS retry rechecks access to the generated file and
issues a fresh temporary URL; expiring delivery URLs are never stored in the
idempotency record. Neither replay reruns a speech provider. Reusing the key for
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
- `502` every provider failed or returned an invalid result.
- `503` IAM, Briefcase, PostgreSQL, or a required dependency-contract capability is unavailable.
- `504` the bounded synchronous workflow exceeded its deadline.

Provider-private response bodies, credentials, input text, transcripts, and file
URLs are never included in public errors.

## Operational endpoints

`GET /health/live` reports process liveness. `GET /health/ready` checks required
configuration, PostgreSQL, FFmpeg, provider availability, and required
dependency-contract capabilities without making billable provider calls. These
routes are intentionally outside `/api/v1`.

The shipped dependency composition remains deliberately unavailable for speech:
`/health/ready` returns `503 not_ready`, and otherwise-valid speech work that
reaches downstream delegation returns `503` at the first missing IAM or
Briefcase contract boundary. This happens before a speech provider is called or
billed.

## Complete flows

### Text to speech

```text
Caller submits text and optional language
  -> Waveform verifies actor context and obtains Briefcase delegation
  -> Waveform attempts provider chain
  -> successful audio is normalized to MP3
  -> Waveform stores MP3 in Briefcase through OBO Access
  -> caller receives permanent and temporary URLs
```

### Voice-message transcription

```text
DM uploads voice file to Briefcase
  -> DM calls Waveform STT through OBO Access
  -> Waveform verifies actor context and obtains Briefcase delegation
  -> Waveform reads authorized Briefcase file
  -> Waveform attempts provider chain
  -> DM sends voice message with transcript or a recorded transcription failure
```

## Remaining contract gaps and upstream blockers

- Long-running asynchronous jobs, status polling, and cancellation are not defined.
- TTS voice, style, speed, pitch, and output-quality controls are not public inputs.
- STT timestamps, segments, confidence, diarization, punctuation, and speaker labels are not represented.
- IAM does not yet define an operation to mint a new Briefcase-audience proof for an actor after Waveform verifies the inbound credential.
- Briefcase does not yet define actor application-folder resolution or a safe
  authenticated mapping from permanent file URLs to the entry-ID-based read,
  access-check, and delivery-URL operations Waveform needs. Waveform fails
  closed at these boundaries.
- Usage quotas, cost attribution, per-organization rate limits, and organization policy are undefined.
- Provider data-processing regions and account-level retention policies still require deployment policy.
- Synchronous processing remains unsuitable for very long media and needs a durable job API.
