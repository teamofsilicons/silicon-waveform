# Silicon Waveform

Turn text into stored speech, or transcribe audio already in Briefcase. Start with the CLI; use the Rust client or HTTP API when building an integration. Waveform authenticates both Carbons and Silicons through IAM.

## Install

```sh
honeycomb install 'tos>waveform'
```

Honeycomb installs the native CLI and manages updates. Then sign in with an IAM short-lived token.

## Sign in and generate speech

```sh
waveform iam --json
waveform login IAM_SHORT_LIVED_TOKEN
waveform login status --json
waveform tts 'Hello, Carbons and Silicons.' --org YOUR_ORG --actor YOUR_PRINCIPAL_UUID
```

Use the returned `app_id` to obtain an SLT from IAM. `login status --json` gives your identity and organization. Waveform never needs your IAM password or Silicon credential. The TTS result contains the stored audio URL; STT accepts a Briefcase file URL.

## Try an isolated sandbox

```sh
waveform --app-secret-file /path/to/waveform-test-secret login TEST_PUBLIC_ID
waveform --test ENVIRONMENT_UUID login status --json
waveform --test ENVIRONMENT_UUID tts 'Test speech' --org TEST_ORG --actor TEST_PRINCIPAL_UUID
```

Supply **only Waveform's IAM test `app_secret`**. The sandbox is discovered automatically. No IAM root key or separately paired Briefcase key is required. IAM supplies the downstream Briefcase credential with each OBO exchange. Test speech uses prerecorded audio and fixed transcripts, while the upload, identity and permissions are real sandbox operations.

The public ID shortcut works only for an existing active test identity. Production accepts issued SLTs only. See [testing](testing.md) for isolation and lifecycle details.

## Choose your path

- [CLI guide](cli.md): usage, configuration, jobs, reports and updates.
- [API guide](api.md): authentication, HTTP headers and speech requests.
- [Rust client](client.md): build Waveform into your app.
- [IAM integration](iam.md): scopes and delegated Briefcase storage.
- [Configuration](configuration.md): local state, opt-outs and deployment settings.
- [API compatibility](api-contracts.md): negotiation, consumer contracts and sunset policy.
- [Releases](releases.md): build and validate the Honeycomb archive.
- [Development](development.md): build, validate and extend Waveform.
- [Voice profiles](voice-profiles.md): the available voices and provider mappings.

Need help from an agent? Ask: “Install Waveform using its docs, sign in using the SLT I provide, and generate speech for this text.”

Repository: https://github.com/teamofsilicons/silicon-waveform

Rust package: https://crates.io/crates/silicon-waveform-client
