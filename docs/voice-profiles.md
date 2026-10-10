# Voice profiles

Waveform profiles give each account one voice choice across its fallback chain.
The catalog contains 30 profiles based on Gemini's named voices. The initial
default is `kore`. Profile IDs are lowercase slugs; display names retain the
provider's capitalization.

## Usage

List the catalog with `GET /api/v1/voice-profiles` (any signed-in account; `waveform voice-profiles` in the CLI).
The response is `{ "items": [...] }`, including each profile's Gemini voice, OpenAI voice, ElevenLabs configuration,
revision, and listening-review status.

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
at the top level. With automatic fallback off, use the selected provider’s
`provider_options` to override voice/model/delivery settings for this generation.

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

## Account defaults

Every account starts with Waveform's default profile, which lives in the database (`waveform_provider_defaults`), not
in code. Waveform saves it as the account's preference the first time the account uses Waveform; signing in again
never overwrites a saved choice, and an account that had a choice before Silicon Accounts keeps it once its data is
linked. A Carbon can read and change the default of a Silicon it looks after with
`PATCH /api/v1/preferences?account=si:…` (`waveform preferences --account si:… --voice-profile …`).

With speech fixtures on (`WAVEFORM_SPEECH_FIXTURES=true`, development and CI only), TTS returns the prerecorded clip
of the resolved profile instead of calling a provider. Profile selection and metadata are real, but a fixture cannot
show how a selected voice sounds; provider HTTP contract tests verify the mapped parameters independently.

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
Provider failures advance only when `auto_fallback: true` is explicitly requested.
The default is off. See [per-provider controls and BYOK](api.md).

Operators can tune `waveform_voice_profiles.profile` (the rows with the nil `plane_id`)
after listening to samples. Every edit automatically increments the profile revision.
Runtime validation rejects unsupported OpenAI voices, incorrect ElevenLabs models,
invalid identifiers, and settings outside the provider's numeric limits. Set
`auditioned` to true only after reviewing the mapping. There is no catalog editing
API; Carbons and Silicons choose among the available profiles.

Sources checked on 2026-09-08:

- [Gemini voice catalog](https://ai.google.dev/gemini-api/docs/speech-generation)
- [OpenAI tts-1 voice support](https://developers.openai.com/api/docs/guides/text-to-speech)
- [ElevenLabs replacement voices](https://elevenlabs.io/docs/help-center/product/voices/my-voices/what-are-default-voices)
- [ElevenLabs speech request](https://elevenlabs.io/docs/api-reference/text-to-speech/convert)
- [ElevenLabs delivery settings](https://elevenlabs.io/docs/api-reference/voices/settings/update)

## Rollout

The catalog arrived with migration `0009_voice_profiles.sql`, which startup applies; existing accounts were given the
default profile and existing speech requests stayed valid. A new profile or mapping is a database change: no client or
website release is needed for it to appear.
