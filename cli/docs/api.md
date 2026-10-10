# Waveform API

The base is `https://backend.waveform.teamofsilicons.com/api/v1`. The full operation catalogue is
[`openapi.yaml`](https://docs.waveform.teamofsilicons.com/openapi.yaml); this guide explains how the pieces fit.

## Authentication

Send exactly one credential:

- `Authorization: Bearer <access token>`: a Silicon Accounts access token issued to app `waveform` (EdDSA JWT,
  `aud` = `waveform`, `iss` = the Silicon Accounts Waveform trusts). Carbons get one with `waveform login`, Silicons
  with `silicon-accounts login --app waveform -q | waveform login --slt-stdin`; the website's server holds its own.
- `Authorization: Proof sap_…`: a User verification proof another app holds for an account, issued for
  `receiving_app` `waveform`. Accepted only on the speech routes, for the scopes in the table below, from apps Waveform
  allows for that scope.

| Scope | Routes |
| --- | --- |
| `waveform.tts` | `POST /tts`, `GET /audio/{request_id}` |
| `waveform.stt` | `POST /stt`, `POST /stt/source-target` |

Refusals answer `401` (or `403` for a proof that is valid but not enough) with an exact `error.reason`; see
[Sign in with Silicon Accounts](accounts.md) for the reasons, the proof checks and the account events Waveform
applies. Headers of earlier versions (`X-Org-ID`, delegated-token headers) are not used; delegated-token headers are
refused.

## Whose data

Everything belongs to the account that made the request. Account routes (`/jobs`, `/preferences`, `/provider-keys`,
`GET`/`DELETE /storage-authorizations`) accept `?account=c:…|si:…|<uuid>`: a Carbon may select a Silicon it is the
custodian of. Anyone else gets `403 not_custodian`; an unknown id is `404 account_not_found`. Speech always runs as
the credential's own account.

## Speech

`POST /tts` takes `{text, voice_profile?, lang?, provider_order?, auto_fallback?, provider_options?, provider_keys?}`
and `POST /stt` takes `{file_url, language?, provider_order?, provider_keys?}`. Both are synchronous, need an
`Idempotency-Key` (8-255 visible characters), and return provider-independent results. `provider_order` is a
preferred prefix; omitted providers follow the account's saved order. TTS `auto_fallback` defaults to false, so only
the first provider runs; STT always falls back automatically.

TTS returns `{request_id, file_url, audio_url, storage, media_type, provider, duration_ms, voice_profile}`.
`file_url` is the MP3's permanent URL in the account's Briefcase files (`apps/waveform`). When Waveform cannot store
it there, the request still completes: `file_url` is `null`, `storage` is
`{"status":"held","reason":…,"message":…,"expires_at":…}` and `GET /audio/{request_id}` serves the MP3 to the same
account (or the same app with its proof) until `expires_at` (header `X-Waveform-Audio-Expires-At`). Reasons:
`delegated_call`, `storage_not_authorized`, `briefcase_refused`, `briefcase_unavailable`, `briefcase_timeout`,
`briefcase_contract_unavailable`.

STT reads `file_url` from the account's own Briefcase files. `POST /stt/source-target` with `{}` makes sure the
account's Waveform folder exists and returns `{app_id, account, folder_id, folder_path}`; upload sources there with
Briefcase and transcribe their permanent URLs. Briefcase shows Waveform its folder (`apps/waveform`) only through the
files in it, so while the folder is still empty the answer is `404 briefcase_folder_not_found` (store one speech
there first, or upload with your own Briefcase access). Without Briefcase access STT answers
`403 storage_authorization_required`.

The speech routes (`/tts`, `/stt`, `/stt/source-target`, `/audio/{request_id}`) always run as the caller: a query
naming an `account` is refused with `400 speech_runs_as_yourself` instead of being ignored.

### Request ids and retries

Send a non-nil UUID in `X-Request-ID` to know the job id before the synchronous answer; `GET /jobs/{id}` shows the
running row once the request is authorised (an initial 404 can mean it is still being authorised). Retrying with the
same `Idempotency-Key` returns the original result and job id without running a provider twice; changing the body
under the same key answers `409`. Job state and idempotency completion are committed together; an interrupted attempt
becomes `failed` with `request_interrupted` once its lease expires.

## Briefcase access

`POST /storage-authorizations` gives Waveform Briefcase access for the calling account (a User verification proof for
`receiving_app` `briefcase`, made from the caller's own access token and kept encrypted). `GET` shows it (`active`,
`revoked` or `missing`, never a token) and `DELETE` revokes it. A direct speech request creates the access when the
account has none yet, and replaces a proof that ended with the sign-in it was issued under (for example after another
machine's `waveform logout`) once Briefcase refuses it. After the account or its custodian revokes the access (here, or
the account revokes the proof at Silicon Accounts), speech is held with `storage_not_authorized` until the account
approves again with `POST`. A proof caller can only use access the account gave before.

## Accounts, history and settings

| Route | What it does |
| --- | --- |
| `GET /accounts` | public discovery: `{app_id, accounts_url, api_url, docs_url, proof_scopes, version}` |
| `GET /me` (alias `/auth/me`) | the signed-in account: `{authenticated, app_id, account{uuid,id,kind,display_name,pfp_url,custodian}, via, expires_at}`; the name and photo come from Silicon Accounts (read once with the account's own token, then kept current by `account.updated`) |
| `GET /silicons` | the Silicons the signed-in Carbon looks after that Waveform knows |
| `GET /jobs`, `GET /jobs/{id}` | content-free history, newest first (`operation`, `limit` 1-100, `cursor`); jobs called by another app carry `issuing_app` |
| `GET`/`PATCH /preferences` | `tts_order`, `stt_order`, `voice_profile`, `telemetry_enabled`, with Waveform's `defaults` |
| `GET /voice-profiles` | the voice catalogue and provider mappings ([voice profiles](voice-profiles.md)) |
| `GET /provider-keys`, `PUT`/`DELETE /provider-keys/{provider}` | write-only saved keys (`{"api_key":…}`) |
| `POST /reports` | `{message, pr?, client_version?}` with an `Idempotency-Key`; 202 with the report id |
| `GET /capabilities` | languages, formats and provider controls (public) |

Saving or deleting a key, a custodian changing a Silicon's settings, and giving or revoking Briefcase access are
checked with Silicon Accounts at that moment. Reports are limited to ten per account per hour; retry identical content
with the same key (changed content answers `409`).

## Account events

Silicon Accounts posts signed events to `POST https://backend.waveform.teamofsilicons.com/webhooks/accounts`
(headers `X-Accounts-Timestamp`, `X-Accounts-Signature`, verified over the raw body; repeats of an event id are
ignored). See [Sign in with Silicon Accounts](accounts.md#account-events).

## Contract negotiation

Every request may send `X-Waveform-API-Version: v1` and `X-Waveform-Protocol-Version: 1`; answers carry
`X-Waveform-Contract-Version`. `GET /api/contracts` lists versions, the compatibility matrix (client and CLI 0.6) and
the deprecation policy. See [API compatibility](api-contracts.md).

## Provider controls and request keys

TTS `provider_options` accepts one provider object matching the selected first provider, and only when
`auto_fallback` is false:

```json
{"text":"Welcome home.","provider_order":["gemini"],"auto_fallback":false,"provider_options":{"gemini":{"scene":"A quiet evening at home","director_notes":"Warm, relaxed delivery"}}}
```

Gemini controls are `voice`, `scene`, `audio_profile`, `director_notes` and `sample_context`. ElevenLabs controls are
`voice_id`, `model_id`, `stability`, `similarity_boost`, `style`, `speed`, `use_speaker_boost`, `seed`,
`previous_text`, `next_text` and `apply_text_normalization`. OpenAI controls are `voice`, `model`, `instructions` and
`speed`; `instructions` requires `gpt-4o-mini-tts`. Unknown fields, controls for another provider, and controls
combined with automatic fallback are refused. Omitted controls use the selected voice profile's defaults.

`provider_keys` (TTS and STT) is a map such as `{"gemini":"<key>"}`. A request key overrides the account's saved key
and Waveform's key for that provider, and is never stored in preferences, history or idempotency results.

Provider failures return a safe `error.message` with `provider`, `reason` and `provider_status`, alongside `code`
and `request_id`, so the caller can choose another provider.
