# Testing environments

## Select your sandbox

Create or import Waveform in a Honeycomb testing environment. Take the application's `app_secret`, not the IAM environment root key. Use Honeycomb for lifecycle management and IAM for test identities.

```sh
waveform --app-secret-file /private/path/test-secret login TEST_PUBLIC_ID
waveform --test ENVIRONMENT_UUID tts 'Hello' --org TEST_ORG --actor TEST_PRINCIPAL_UUID
```

You may also use `waveform --test 'ask_…' COMMAND`. A private file or stdin avoids putting the secret in shell history. Once discovery succeeds, the CLI stores a private UUID-to-secret mapping so `--test ENVIRONMENT_UUID` works. State lives under `$SILICON_HOME/.waveform/dir`, or `~/.waveform/dir` by default. The selected sandbox is printed to stderr at the end, including failures; stdout remains usable as JSON.

In the website, choose **Use a test app_secret** on sign-in or in settings. Enter the secret, then an IAM test SLT or an existing active Carbon/Silicon public ID. The banner identifies the environment and identity. **Exit testing mode** returns to the separate production session.

## API and Rust

Send `X-Testing-Environment-Key: <Waveform IAM test app_secret>` on every request. This historical header name now accepts application secrets. `GET /api/v1/testing-environment` discovers the sandbox without a user login. `POST /api/v1/auth/login` accepts `{"slt":"TEST_PUBLIC_ID"}` or an IAM-issued test SLT. Other operations require the resulting bearer and the appropriate organization and permissions.

```rust,no_run
use silicon_waveform_client::{Auth, Client, TestEnvironmentKey};
# async fn example(secret: String) -> Result<(), Box<dyn std::error::Error>> {
let sandbox = Client::new("https://backend.waveform.teamofsilicons.com", Auth::Anonymous)?
    .with_test_environment(TestEnvironmentKey::new(secret)?);
let environment = sandbox.current_test_environment().await?;
let tokens = sandbox.login("existing-test-user").await?;
let authenticated = sandbox.with_bearer(tokens.access_token);
# Ok(()) }
```

## Isolation and permissions

The secret selects a world; it does not select a user or confer administrative privileges. Ordinary IAM authorization, organization and speech scopes apply. Unknown/inactive identities, production tokens, wrong-world tokens, revoked secrets and unavailable IAM are rejected. A failed test selection never falls back to production.

New sandboxes contain no user data. Voice profiles and provider defaults are configuration shared as initial defaults. Jobs, provider keys, preferences, idempotency, reports and webhook metadata are scoped to the environment. Honeycomb owns environment creation and lifecycle. Waveform receives authenticated preparation, rotation, cleaning, disabling, restoration and removal instructions; IAM still authenticates the test identities. Cleanup fences running operations and clears test records before acknowledging completion. The environment link survives cleanup. Old public lifecycle URLs return `manage_environment_in_honeycomb`. See the [participant protocol](honeycomb-lifecycle.md).

## Speech and Briefcase

Test TTS selects the prerecorded Gemini clip matching the requested/account-default voice profile. Each request uploads a new uniquely named MP3 through the Briefcase client. Test STT validates source access and returns the fixed test transcript. No paid TTS or STT provider call runs.

Waveform exchanges a subject-bound, exact-byte OBO request with IAM. In a test world IAM includes the destination application's test credential with the proof. Waveform forwards that credential only to Briefcase for the corresponding request; you do not provide a separate Briefcase key. If the downstream testing context is missing or invalid, the operation fails closed.

## Webhooks and side effects

Waveform verifies signatures over the complete raw body before interpreting the test wrapper. The authenticated testing key digest selects the matching environment. It records content-free event metadata, deduplicates event IDs and always reads current authorization from IAM, so reordered events cannot grant stale access. Root keys and raw webhook payloads are not stored.

Bug reports in testing are persisted as `simulated`; they never send real mail. Test operations do not emit production telemetry. Sandbox requests cannot invoke paid speech providers.
