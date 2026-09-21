# Silicon Waveform Rust client

Add the package with `cargo add silicon-waveform-client@0.3`. Production clients
use `https://backend.waveform.teamofsilicons.com` as their API origin.

`silicon-waveform-client` is stateless. Construct it with `Auth::Anonymous`,
then use `with_bearer` with an access token obtained by exchanging an IAM SLT
through `login`. The client never asks for IAM credentials or persists a
session. `refresh` exchanges a refresh token; `logout` revokes a token and
correctly accepts Waveform's `204 No Content` response.

`client.iam().await?` returns typed `IamInfo` containing `app_id`,
`iam_base_url`, and the upstream IAM `testing_environment_id` (null in production).
It requires no login and honors the selected test environment.

`client.with_bearer(access_token).login_status().await?` returns typed
`LoginStatus`: `authenticated`, optional `actor` (`actor_type`, `public_id`), `org_id`, and `testing_environment_id`. It verifies the token via
`auth/me`; anonymous clients return false without network access, and HTTP
401/403 also return false. Other failures remain errors. No tokens are returned
in status, and it does not refresh or persist them.

Version 0.3 uses the immutable canonical `public_id` for Carbon and Silicon identities.
There is no separate principal UUID. Upgrade older Waveform clients before the IAM 3
cutover; version 0.3 also accepts responses from the preceding Waveform backend.

The stateless client does not resolve home directories. The CLI uses
`SILICON_HOME` as its default home when present, otherwise `~`, with an explicit
`waveform config home` setting taking precedence. See [CLI guide](cli.md).

Use `with_test_environment(TestEnvironmentKey::new(app_secret)?)` to select a sandbox.
`current_test_environment()` validates the secret with IAM and returns safe metadata.
Every request carries the selector; invalid selectors fail, without production fallback.
No separate Briefcase key is needed. `login` accepts a test SLT or existing public ID in this mode.

Speech methods are `tts` and `stt`; each accepts an optional `provider_order`
prefix to override the account's first choices. `capabilities` is unauthenticated. Authenticated
control methods include `me`, actor-scoped `jobs`/`job`, provider `preferences`
and `update_preferences`, and write-only personal provider-key methods.
`jobs_page` accepts optional operation, limit (1-100), and opaque cursor values. A `Job` is terminal when its status is `completed` or `failed`; `wait_for_job` polls until a terminal state or caller timeout, and failed rows include `error_code`.

Legacy test-plane lifecycle methods require the organization ID because IAM
authorization is organization-scoped: `create_test_environment`,
`test_environments`, `test_environment_detail`, `test_environment_key`,
`rotate_test_environment_key`, `delete_test_environment`, and
`restore_test_environment`. `test_environment` and
`clean_test_environment` retain their historical signatures for source compatibility. Lifecycle mutations now return `manage_environment_in_honeycomb`; manage shared worlds in Honeycomb.

## Dependency maintenance

The Rust client is a normal, stateless project dependency. Requests never query
crates.io, invoke Cargo or modify a consuming project's lockfile. Update the
dependency explicitly through your project's normal review and build process.
`with_auto_update` remains a no-op for source compatibility; `update_status()`
returns `Disabled`. Honeycomb owns CLI installation and updates.

TTS `temporary_url` is optional. Published Briefcase one-shot uploads return a
permanent authenticated URL. Test-plane TTS uses the same upload adapter and
also returns `temporary_url: null`.

Personal provider keys saved through `put_provider_key` are selected by the
backend for that authenticated account and plane. A matching personal key
replaces the deployment key for that provider; other providers retain their
own configured keys. Key storage errors stop the operation, and deleting a
personal key restores deployment-key selection on subsequent requests.

## Polling a running speech operation

Create a UUID and use `client.with_speech_request_id(id)?` for that logical
speech operation. It sends `X-Request-ID` on TTS/STT requests; authentication,
preferences and history calls remain independent. Use a fresh UUID for a new
operation and retain the original UUID and idempotency key for a retry.

Run the synchronous speech future concurrently with
`client.wait_for_job(&id.to_string(), org, actor, timeout, interval)`. The polling
method retries an initial 404 while authorization and job creation are pending.
Its timeout covers HTTP requests as well as polling delays. It returns the job
record for either terminal state; inspect `status` and `error_code`. A successful
speech response retains the canonical ID if an idempotency key already existed.

`TtsRequest.voice_profile: Option<String>` selects a profile for one generation.
`voice_profiles(org, actor)` returns a typed catalog.
`update_preferences_with_voice(org, actor, tts_order, stt_order, voice_profile)`
updates defaults atomically; the existing `update_preferences` method preserves
the saved voice. TTS responses and jobs expose optional `VoiceProfileRef` metadata.

## Reporting and settings

`report(message, pr, idempotency_key)` submits an authenticated report, with simulated delivery in sandboxes. `set_telemetry(org, actor, enabled)` controls the account opt-out. On Unix, `telemetry::from_environment()` provides an optional Space Station sender for embedding programs; only record safe action labels and outcomes.

`client.contracts().await?` reads version negotiation and compatibility information.

`refresh_with_key(refresh_token, idempotency_key)` retains a caller-supplied retry key across uncertain refresh responses. Persist the returned token pair before using it again; concurrent rotations must be serialized by the caller. The CLI does this automatically.

## Request-specific synthesis controls and BYOK

`TtsRequest` implements `Default`; its `auto_fallback` defaults to false. Only the
first provider resolved from `provider_order` and account preferences is tried.
Set `auto_fallback: true` to restore the provider fallback sequence. STT fallback
is unchanged.

`TtsProviderOptions` has optional `gemini: GeminiTtsOptions`,
`elevenlabs: ElevenLabsTtsOptions`, and `openai: OpenAiTtsOptions` fields. Provider
controls require fallback to be disabled and must match the selected provider.
Gemini accepts scene, audio profile, director notes, sample context and voice;
ElevenLabs accepts model, voice and delivery controls; OpenAI accepts model,
voice, instructions and speed. OpenAI instructions require explicit
`model: Some("gpt-4o-mini-tts".into())`. Unknown JSON fields are rejected.

```rust,ignore
let mut keys = silicon_waveform_client::ProviderKeys::default();
keys.insert("gemini", std::env::var("MY_GEMINI_KEY")?)?;
let request = silicon_waveform_client::TtsRequest {
    text: "Welcome home.".into(),
    provider_order: Some(vec!["gemini".into()]),
    provider_options: silicon_waveform_client::TtsProviderOptions {
        gemini: Some(silicon_waveform_client::GeminiTtsOptions {
            scene: Some("A quiet evening at home".into()),
            ..Default::default()
        }),
        ..Default::default()
    },
    provider_keys: keys,
    ..Default::default()
};
```

Both `TtsRequest.provider_keys` and `SttRequest.provider_keys` accept `ProviderKeys`.
This wrapper redacts `Debug` output and serializes a map of provider names to keys
only when sending a request. Request keys override saved personal keys, which
override deployment keys. A supplied invalid key fails for that provider; it is
not retried using another credential for the same provider. Request keys are not
saved. Existing `put_provider_key` remains available to save a personal key.

`Error::Api` includes the safe backend `message`, `request_id`, `provider`,
`reason`, and `provider_status` when available. Its display includes the backend
message so callers can distinguish provider rejection, rate limiting and other
failures and select another provider. Legacy error bodies without these fields
remain supported.

`capabilities().tts` exposes optional `auto_fallback_default` and a provider-to-field
`provider_options` map. Top-level `byok: Option<ByokCapabilities>` reports saved
and per-request support plus credential precedence. Older servers may omit this
metadata; optional fields remain `None` and the options map remains empty.
