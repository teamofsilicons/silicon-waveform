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

Waveform creates the file in the represented actor's Briefcase application folder using a name based on the request time. The permanent URL is the durable reference; the temporary URL is for immediate playback.

The language hint may be ignored by providers that do not accept it. It must not change the response schema.

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

## Capabilities

### `GET /capabilities`

Returns the normalized public capabilities of Waveform.

- **Authentication:** None.
- **Returns:** TTS output format, supported language hints, STT language hints, and accepted media types.

This endpoint describes the stable Waveform contract, not each provider's private capabilities. Clients can use it to validate media before starting a request.

## Complete flows

### Text to speech

```text
Caller submits text and optional language
  -> Waveform attempts provider chain
  -> successful audio is normalized to MP3
  -> Waveform stores MP3 in Briefcase through OBO Access
  -> caller receives permanent and temporary URLs
```

### Voice-message transcription

```text
DM uploads voice file to Briefcase
  -> DM calls Waveform STT through OBO Access
  -> Waveform reads authorized Briefcase file
  -> Waveform attempts provider chain
  -> DM sends voice message with transcript or a recorded transcription failure
```

## Contract gaps

- Long-running asynchronous jobs, status polling, and cancellation are not defined.
- TTS voice, style, speed, pitch, and output-quality controls are missing.
- Text and media duration limits are not specified.
- STT timestamps, segments, confidence, diarization, punctuation, and speaker labels are not represented.
- Provider timeout, fallback, retry, and error-normalization rules are not documented.
- The capabilities response does not distinguish guaranteed from best-effort languages.
- Briefcase destination selection and filename collision behavior are not configurable.
- Usage quotas, cost attribution, rate limits, and organization policy are undefined.
- Retention and privacy behavior for provider-submitted text and audio need explicit rules.
- Synchronous processing is unsuitable for very long media and needs a durable job API.
