# Voice profiles

Waveform profiles give each account one voice choice across its fallback chain.
The catalog contains 30 profiles based on Gemini's named voices. The initial
default is `kore`. Profile IDs are lowercase slugs; display names retain the
provider's capitalization.

## Usage

Authenticate and list `GET /api/v1/voice-profiles` with `X-Org-Id`. The response
is `{ "items": [...] }`, including each profile's Gemini voice, OpenAI voice,
ElevenLabs configuration, revision, and listening-review status.

Set an account default with `PATCH /api/v1/preferences`:

```json
{ "voice_profile": "puck" }
```

Override that choice for one generation with `POST /api/v1/tts`:

```json
{
  "text": "Hello from Waveform.",
  "voice_profile": "sulafat",
  "provider_order": ["elevenlabs", "gemini", "openai"]
}
```

Omitting or sending null for `voice_profile` uses the saved account default.
An override never changes that default. An unknown ID is rejected before any
speech provider runs. Raw `voice_id` and `voice_settings` fields are not accepted
on this endpoint; they belong to the catalog mapping.

Provider order remains an independent preference. Waveform resolves the profile
once and supplies the appropriate mapping to each attempted provider. ElevenLabs
receives its mapped ID in the URL and `model_id: eleven_multilingual_v2`, `text`,
and all five `voice_settings` fields in the body. Gemini receives its named voice
and optional language hint. OpenAI retains `tts-1` and receives its mapped voice.

TTS results and job history include `voice_profile: { "id": "sulafat",
"revision": 1 }`. Legacy records and STT have no profile. Completed idempotent
replays retain the original profile and audio even after defaults or mappings
change. Changing the explicit profile while reusing a key is a conflicting
request. Failed attempts may resolve the current mapping when retried.

## Account and test-environment lifecycle

IAM owns signup. Waveform persists the initial default on the first successful
Waveform login after signup, and on first authenticated use for existing accounts
or headless/OBO callers. Login never overwrites an existing preference. Accounts
are scoped by environment, organization, and IAM principal ID.

New test environments copy the production catalog and workspace defaults into
their own rows. Their preferences and catalog edits cannot affect production.
Cleaning a test environment clears account choices; the next authenticated use
assigns that environment's default again. Catalog configuration survives cleaning.
Deletion and retention cleanup remove the catalog with the environment.

Test speech still uses the prescribed prerecorded fixture and paired Briefcase
storage; profile selection and metadata are real, but the fixture cannot validate
how a selected voice sounds. Provider HTTP contract tests verify the mapped
parameters independently.

## Initial mappings and listening review

Mappings are provisional (`auditioned: false`). They group voices by broad
delivery character and allow many profiles to share a provider voice. These are
starting configurations, not measurements of acoustic similarity.

ElevenLabs mappings use the replacement voices linked from its official default
voice migration page:

| Character | ElevenLabs voice | Voice ID | Stability | Style | Speed |
| --- | --- | --- | --- | --- | --- |
| Warm | Darian | `gOupLcAkjEnguROwi4oS` | 0.65 | 0.1 | 0.95 |
| Soft | Talia | `OZ0L6eISlOejga3XjDFt` | 0.65 | 0 | 0.95 |
| Narration | Elara | `WQP7cQUF5aAS6Axh5yaa` | 0.7 | 0 | 1 |
| Upbeat | Elowen | `dvbL7qkNGZY1IqPGZAjM` | 0.4 | 0.2 | 1.05 |

Initial similarity boost is 0.75 and speaker boost is true. Similarity boost
preserves the chosen ElevenLabs voice; it does not match an arbitrary Gemini or
OpenAI voice. The active API key must have access to the mapped library voice.
Provider failures still advance to the next configured fallback.

Operators can tune `waveform_voice_profiles.profile` in the selected plane after
listening to samples. Every edit automatically increments the profile revision.
Runtime validation rejects unsupported OpenAI voices, incorrect ElevenLabs models,
invalid identifiers, and settings outside the provider's numeric limits. Set
`auditioned` to true only after reviewing the mapping. There is no user-facing
catalog editing API; account users select among the available profiles.

Sources checked on 2026-09-08:

- [Gemini voice catalog](https://ai.google.dev/gemini-api/docs/speech-generation)
- [OpenAI tts-1 voice support](https://developers.openai.com/api/docs/guides/text-to-speech)
- [ElevenLabs replacement voices](https://elevenlabs.io/docs/help-center/product/voices/my-voices/what-are-default-voices)
- [ElevenLabs speech request](https://elevenlabs.io/docs/api-reference/text-to-speech/convert)
- [ElevenLabs delivery settings](https://elevenlabs.io/docs/api-reference/voices/settings/update)

## Rollout

Deploy the backend with migration `0009_voice_profiles.sql` before the frontend
and client changes. Startup applies migrations automatically. Existing account
rows are assigned their plane's workspace default; existing speech requests
remain valid. The frontend can then list profiles and save/select defaults.
This feature's local implementation does not itself deploy AWS or host the UI.
