# Silicon Waveform Rust client

`silicon-waveform-client` is stateless. Construct it with `Auth::Anonymous`,
then use `with_bearer` with an access token obtained by exchanging an IAM SLT
through `login`. The client never asks for IAM credentials or persists a
session. `refresh` exchanges a refresh token; `logout` revokes a token and
correctly accepts Waveform's `204 No Content` response.

Use `with_test_environment(TestEnvironmentKey::new(root_key)?)` to select an
isolated plane explicitly. Every selected-plane request carries the root key;
omitting the selector always targets production. The CLI can resolve a test
plane UUID to its root key, but the Rust client intentionally requires the
root key so callers cannot accidentally use an unbounded environment lookup.

Speech methods are `tts` and `stt`; each accepts an optional `provider_order`
prefix to override the account's first choices. `capabilities` is unauthenticated. Authenticated
control methods include `me`, actor-scoped `jobs`/`job`, provider `preferences`
and `update_preferences`, and write-only personal provider-key methods.
`jobs_page` accepts optional operation, limit (1-100), and opaque cursor values. A `Job` is terminal when its status is `completed` or `failed`; `wait_for_job` polls until a terminal state or caller timeout, and failed rows include `error_code`.

Test-plane lifecycle methods require the organization ID because IAM
authorization is organization-scoped: `create_test_environment`,
`test_environments`, `test_environment_detail`, `test_environment_key`,
`rotate_test_environment_key`, `delete_test_environment`, and
`restore_test_environment`. `test_environment` and
`clean_test_environment` operate on the selected root-key plane.

## Automatic package maintenance

The client checks crates.io after a request finishes, at most once per hour
per client and its clones. Checks are enabled by default. Use
`client.with_auto_update(false)` or `WAVEFORM_CLIENT_AUTO_UPDATE=false` to opt out.
`client.update_status()` exposes the last outcome. A failure leaves the API
result unchanged, and cancelled attempts retain their hourly throttle.

When a newer `silicon-waveform-client` release exists, maintenance runs
`cargo update -p silicon-waveform-client --precise <version>` against the
nearest consuming Cargo manifest. This updates its lockfile for the next build;
it cannot replace library code already loaded in the current process.
Unpublished packages or incompatible dependency constraints produce a failed
maintenance status. No publish or installation was performed during local tests.

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
