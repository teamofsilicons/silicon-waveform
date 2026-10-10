# Silicon Waveform API documentation

This document explains every operation in the Silicon Waveform OpenAPI contract (contract 2.0.0 of API v1). The
machine-readable contract is [`openapi.yaml`](./openapi.yaml); the shorter usage guide is [docs/api.md](docs/api.md).

## API conventions

### Base URL

```text
https://backend.waveform.teamofsilicons.com/api/v1
```

Waveform provides a provider-independent interface for text-to-speech and speech-to-text. TTS uses the selected
provider with automatic fallback off by default; STT keeps automatic fallback. Callers receive normalized response
shapes and safe provider failure details. All generation requests are synchronous and hold the connection until
success or terminal failure.

### Authentication

Send exactly one credential:

- **Bearer:** `Authorization: Bearer <access token>`, a Silicon Accounts access token issued to app `waveform` (an
  EdDSA JWT with `aud` = `waveform` and `iss` = the Silicon Accounts Waveform trusts). Waveform verifies it locally
  against the published signing keys, refuses tokens issued before the account signed out, removed Waveform's access
  or was deleted, and checks sensitive changes with Silicon Accounts at that moment. Carbons get a token with
  `waveform login` or the website; Silicons with `silicon-accounts login --app waveform -q | waveform login --slt-stdin`.
- **Proof:** `Authorization: Proof sap_…`, a Silicon Accounts User verification proof another app holds for an
  account, issued for `receiving_app` `waveform`. Accepted only on the speech routes below, for the scope each route
  needs, from apps allowed for that scope in `WAVEFORM_PROOF_ISSUERS`. Waveform verifies every proof with Silicon
  Accounts, acts as the proof's account, and records the calling app on the job (`issuing_app`).

| Scope | Routes |
| --- | --- |
| `waveform.tts` | `POST /tts`, `GET /audio/{request_id}` |
| `waveform.stt` | `POST /stt`, `POST /stt/source-target` |

Sending two credentials, a scheme other than these, or a header of earlier versions is a malformed request (`401` with
an exact `reason`). An `X-Org-ID` header sent by an old client is ignored. Speech routes also need `Idempotency-Key`.

### Whose data

Everything belongs to the account that made the request: speech history, preferences, saved provider keys, held audio
and Waveform's Briefcase access. On `GET /jobs`, `GET /jobs/{job_id}`, `GET`/`PATCH /preferences`, the
`/provider-keys` routes and `GET`/`DELETE /storage-authorizations`, a Carbon may add `?account=si:…` (or the Silicon's
uuid) to act on a Silicon it is the custodian of; anyone else gets `403 not_custodian`, and an unknown id is
`404 account_not_found`. Speech, held audio and giving Briefcase access always act as the credential's own account.

## Text to speech

### `POST /tts`

Converts text into speech and stores the final MP3 in the account's Briefcase files.

- **Credential:** Bearer, or Proof with `waveform.tts`.
- **Required input:** `text`.
- **Optional input:** `voice_profile` (a catalog id), `lang` (a BCP 47 language hint), `provider_order` (a provider
  name array: a preferred prefix, followed by the account's saved order), `auto_fallback` (false by default),
  `provider_options` (controls for the selected provider) and `provider_keys` (request-only keys).
- **Required header:** `Idempotency-Key`.
- **Returns:** `request_id`, `file_url`, `audio_url`, `storage`, `media_type`, `provider`, `duration_ms`,
  `voice_profile`, and `temporary_url` (always `null`, kept for older clients).

TTS attempts only the first resolved provider by default. Set `auto_fallback: true` to try the remaining configured
providers. With fallback off, use `provider_options` for the selected provider; a failure returns `502 provider_failed`
with the provider, a safe reason, an optional upstream status, and guidance to correct the request or choose another
provider. TTS does not retry a provider generation internally. The output is normalized to `audio/mpeg`.

Waveform creates the file in the account's Briefcase folder (`apps/waveform`, named from the canonical operation start
time established by the first successful idempotency claim) and returns its permanent URL as `file_url`, with
`storage.status = "stored"`. When it cannot store it there the request still completes: `file_url` is `null`,
`storage` is `{"status": "held", "reason", "message", "expires_at"}`, and `audio_url` serves the MP3 until
`expires_at`. The reasons are `delegated_call` (a proof caller: Waveform holds no Waveform sign-in of that account to
reach Briefcase with), `storage_not_authorized`, `briefcase_refused`, `briefcase_unavailable`, `briefcase_timeout` and
`briefcase_contract_unavailable`.

Text must contain at least one non-whitespace character and is limited to 4,096 Unicode scalar values while the API
is synchronous. `lang` must be a valid BCP 47 tag; it is sent only to Gemini (ElevenLabs and OpenAI infer the language
from the text) and never changes the response schema.

### `GET /audio/{request_id}`

Downloads an MP3 Waveform holds for a TTS request (see `storage` above).

- **Credential:** Bearer of the account that made the request, or Proof with `waveform.tts` from the app that made it.
- **Returns:** the MP3, with `X-Waveform-Audio-Expires-At`. Expired or unknown ids answer `404`. A custodian cannot
  download its Silicons' audio.

## Speech to text

### `POST /stt`

Transcribes an audio or video file referenced by a permanent URL in the account's Briefcase files.

- **Credential:** Bearer, or Proof with `waveform.stt`.
- **Required input:** `file_url`.
- **Optional input:** `language` (a BCP 47 hint), `provider_order` and `provider_keys`.
- **Required header:** `Idempotency-Key`.
- **Returns:** `request_id`, `transcript`, `detected_language` (nullable), `provider` and `duration_ms` (the source
  duration, nullable).

Waveform reads the source from Briefcase as the account, with the Briefcase access the account gave Waveform (a
direct request creates it when needed; a proof caller can only use access the account gave before, otherwise
`403 storage_authorization_required`). Supplying a URL never bypasses Briefcase permissions. Waveform attempts its
STT provider chain until one succeeds or all fail, and returns normalized text whichever provider answered.

`file_url` must be an HTTPS permanent URL at the configured Briefcase origin. Waveform never fetches the URL directly:
Briefcase re-authorizes the account and returns the media through its authenticated content flow. Synchronous STT is
limited to 25 MiB and the media types advertised by `/capabilities`; Waveform measures the decoded duration before any
provider call, so malformed media never reaches a paid provider.

### `POST /stt/source-target`

Makes sure the account's Waveform folder exists in Briefcase and returns `{app_id, account, folder_id, folder_path}`.
Upload sources there with Briefcase, then transcribe their permanent URLs.

- **Credential:** Bearer, or Proof with `waveform.stt`. **Body:** `{}`.

## Capabilities

### `GET /capabilities`

Returns the normalized public capabilities of Waveform, without authentication: the TTS output format, the language
hints TTS and STT accept, the accepted media types, the provider control names, the default fallback mode and the key
precedence. It describes the stable Waveform contract, not each provider's private capabilities, so clients can
validate media before starting a request. Answers carry `Cache-Control: no-store` so intermediaries cannot replay
another request's `X-Request-ID`. Language arrays describe best-effort hints rather than a guarantee that every
fallback provider supports every language.

## Accounts

### `GET /accounts`

Public discovery, without authentication: `{app_id, accounts_url, api_url, docs_url, proof_scopes, version}`. It is
what `waveform accounts --json` prints offline.

### `GET /me` (also `GET /auth/me`)

The signed-in account: `{authenticated, app_id, account{uuid, id, kind, display_name, pfp_url, custodian}, via,
expires_at}`. `via` is `bearer`. A Silicon's answer names its custodian.

### `GET /silicons`

The Silicons the signed-in Carbon looks after that Waveform knows (custodian data comes from Silicon Accounts, cached
and kept current by its events). A Silicon gets an empty list.

## History and settings

### `GET /jobs` and `GET /jobs/{job_id}`

Content-free history, newest first: `id`, `operation` (`tts`/`stt`), `status` (`running`, `failed`, `completed`),
`first_line`, `duration_ms`, `provider`, `voice_profile`, `issuing_app` (the app that called with a proof),
`error_code` on failed rows, `created_at` and `finished_at` on terminal rows. Query parameters: `operation`, `limit`
(1–100), `cursor` (opaque; pass `next_cursor` back unchanged) and `account`. Send a non-nil UUID in `X-Request-ID` on a
speech request to know its job id before the synchronous answer: `GET /jobs/{id}` shows the running row once the
request is authorized.

### `GET /preferences` and `PATCH /preferences`

`tts_order`, `stt_order`, `voice_profile` and `telemetry_enabled`, with Waveform's `defaults` (stored in the database,
not in code). `PATCH` accepts any of the four. A custodian changing a Silicon's preferences is checked with Silicon
Accounts at that moment.

### `GET /voice-profiles`

The voice catalog (30 profiles) and each profile's provider mappings. See [voice profiles](docs/voice-profiles.md).

### `GET /provider-keys`, `PUT /provider-keys/{provider}`, `DELETE /provider-keys/{provider}`

Saved provider keys are write-only: `PUT` takes `{"api_key": "…"}`, the list shows metadata only, and nothing returns a
value. Keys are encrypted per account. Saving and deleting are checked with Silicon Accounts at that moment.

## Briefcase access

### `POST`, `GET` and `DELETE /storage-authorizations`

`POST` gives Waveform Briefcase access for the calling account: Waveform asks Silicon Accounts for a User verification
proof for `receiving_app` `briefcase` with the scopes `briefcase.uploads.reserve`, `briefcase.uploads.commit`,
`briefcase.entries.list` and `briefcase.files.read`, made from the caller's own access token, and keeps it encrypted.
Only the account itself can give it (a direct speech request does it automatically when needed). `GET` shows it
(`active`, `revoked` or `missing`, with the scopes and expiries, never a token) and `DELETE` revokes it at Silicon
Accounts; a custodian may do both for a Silicon it looks after. Waveform also revokes it when the account signs out
everywhere, removes Waveform's access or is deleted.

## Contracts, diagnostics and reports

- `GET /contracts` (also unversioned `GET /api/contracts`): versions, the compatibility matrix and the deprecation
  policy, without authentication. See [API compatibility](docs/api-contracts.md).
- `POST /telemetry`: records a diagnostics event from the website (step names only); dropped when the account opted
  out. `204`.
- `POST /reports`: `{message, pr?, client_version?}` with an `Idempotency-Key`; `202` with the report id. Ten reports
  per account per hour; reports that contain tokens or secrets are refused; retry identical content with the same key
  (changed content answers `409`). A Waveform without configured mail answers `503 postmark_not_configured`.

## Account events

### `POST /webhooks/accounts`

Silicon Accounts posts signed events about Waveform's members here. Waveform verifies
`X-Accounts-Signature: v1=<hex HMAC-SHA256(whsec, "{X-Accounts-Timestamp}.{raw body}")>` over the raw body with a
five-minute timestamp tolerance (`401` otherwise), ignores repeats of an `event_id`, and applies:
`account.id_changed` (the new id everywhere), `account.updated` (cached name and photo), `silicon.custodian_changed`
(custodian powers move), `membership.signed_out` (any reason except `app_revoked` refuses tokens issued earlier and
ends Waveform's Briefcase access), `membership.access_removed` (the same) and `account.deleted` (the account's Waveform
data is deleted). Unknown types and `ping` answer `200`. Without a configured secret it answers `503`.

## Idempotency and errors

An idempotency key (8–255 visible characters) is scoped to the account and the speech operation for at least 24 hours
(the configured retention). It is bound to an HMAC-SHA-256 digest of the validated request. A completed retry returns
the original request id and stable result fields without running a provider again; before returning a cached STT
transcript Waveform rechecks the account's access to the exact source file. Reusing the key for a different request,
or retrying while the first request still holds its lease, answers `409`; in-progress answers include `Retry-After`.
Job state and idempotency completion are committed together; an interrupted attempt becomes `failed` with
`request_interrupted` once its lease expires.

Every response carries `Cache-Control: no-store` and `X-Request-ID`. A successful replay returns the original request
id in both the header and body; replay-time errors use the current attempt's id. Errors use one envelope:
`{"error": {"code", "message", "hint", "reason", "request_id", …}}`, where `message` says exactly what failed and why,
`hint` what to do next, and `reason` the exact authentication refusal (`missing_credentials`, `token_expired`,
`wrong_audience`, `wrong_issuer`, `sign_in_ended`, `invalid_proof`, `proof_scope_missing`, `proof_issuer_not_allowed`,
…) or the dependency or provider that failed. The status classes:

- `400` malformed request, URL or language hint.
- `401` missing, malformed, expired, revoked or foreign credential (see `reason`).
- `403` a valid credential that is not enough: a proof without the route's scope or from an app not allowed for it,
  `not_custodian`, or `storage_authorization_required`.
- `404` an unknown job, held audio or account id, or a Briefcase source or replayed file Briefcase reports missing.
- `409` idempotency mismatch or a request in progress.
- `410` a retired route of earlier versions (`route_retired`, with the replacement in `hint`) or a sunset API version.
- `413` synchronous text, media or audio limit exceeded; `415` unsupported source media.
- `429` local admission limit or report quota reached.
- `502 provider_failed` (the selected TTS provider failed with fallback off) or `502 providers_exhausted`.
- `503` Silicon Accounts, Briefcase, PostgreSQL or a required dependency contract is unavailable.
- `504` the bounded synchronous workflow exceeded its deadline.

Provider-private response bodies, credentials, input text, transcripts and file URLs never appear in public errors.

## Operational endpoints

`GET /health/live` reports process liveness. `GET /health/ready` checks configuration, PostgreSQL, FFmpeg, provider
availability and the required dependency contracts without billable provider calls. Both are outside `/api/v1`.
Readiness does not prove remote credentials are valid.

## Complete flows

### Text to speech

```text
Caller (a Carbon or Silicon with its token, or an app with a proof) submits text
  -> Waveform verifies the credential and acts as its account
  -> Waveform attempts the provider chain; successful audio is normalized to MP3
  -> Waveform stores it in the account's Briefcase files with its User verification proof for Briefcase
     (or holds it itself and says why)
  -> caller receives the permanent URL (or the audio_url of the held MP3)
```

### Voice-message transcription

```text
An app uploads the voice file to the account's Briefcase files (the folder from POST /stt/source-target)
  -> the app submits the permanent URL with its waveform.stt proof (or the account with its own token)
  -> Waveform reads the file from Briefcase as the account and measures its decoded duration
  -> Waveform attempts the STT provider chain
  -> the app receives the transcript
```

## Provider controls and bring your own key

TTS `auto_fallback` defaults to `false`, including requests from clients that omit the field; send `true` for the
provider chain. `provider_order` still selects the first provider and the optional fallback order. Use a new
idempotency key when changing the provider, fallback mode, controls or request keys.

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

Only controls belonging to the selected provider are accepted, and nonempty controls require fallback off. Omitted
settings inherit the chosen voice profile.

| Provider | Controls |
| --- | --- |
| Gemini | `voice`, `scene`, `audio_profile`, `director_notes`, `sample_context` |
| ElevenLabs | `voice_id`, `model_id`, `stability`, `similarity_boost`, `style`, `speed`, `use_speaker_boost`, `seed`, `previous_text`, `next_text`, `apply_text_normalization` |
| OpenAI | `voice`, `model`, `instructions`, `speed` |

Gemini scene, profile and direction become labeled prompt sections before the transcript; they are guidance, not
guarantees about delivery. ElevenLabs stability, similarity and style accept 0–1; speed accepts 0.7–1.2;
normalization accepts `auto`, `on`, `off`. OpenAI speed accepts 0.25–4. Instructions require `gpt-4o-mini-tts`;
`tts-1` and `tts-1-hd` reject them. Unknown or mismatched controls fail validation before generation. See the OpenAPI
document for every bound.

Save a key with `PUT /provider-keys/{provider}` and `{"api_key": "<private-key>"}`: it is encrypted for the account,
listing returns metadata only, and deleting it returns the account to Waveform's key. For one request, send
`provider_keys: {"gemini": "<private-key>"}` (or another supported provider). Selection is request key → the account's
saved key → Waveform's key, independently for each provider. A rejected personal key never silently retries with
Waveform's key. Request keys are not saved, returned or logged; only a keyed request digest binds them to idempotency.
Providers charge the account that owns the chosen key.

A development Waveform with speech fixtures (`WAVEFORM_SPEECH_FIXTURES=true`, never in production) validates the same
fields but returns prerecorded profile audio or a fixed transcript, so saved or request keys never reach a paid
provider there.

Provider documentation verified for these mappings:
- [Gemini speech generation](https://ai.google.dev/gemini-api/docs/speech-generation)
- [ElevenLabs speech endpoint](https://elevenlabs.io/docs/api-reference/text-to-speech/convert)
- [OpenAI speech endpoint](https://developers.openai.com/api/reference/resources/audio/subresources/speech/methods/create)
