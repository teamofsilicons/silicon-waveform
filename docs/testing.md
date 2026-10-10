# Testing with Waveform

Waveform has no hosted sandboxes. Test against a development Waveform: one you run on your machine (or in CI) with
deterministic speech fixtures, signed in with a development Silicon Accounts. Nothing you do there reaches a paid
speech provider or production data.

## Run a development Waveform

Start the service with fixtures on, pointed at a development Silicon Accounts and a PostgreSQL database:

```sh
export WAVEFORM_DATABASE_URL=postgres://postgres@127.0.0.1:5432/waveform_dev
export WAVEFORM_BIND_ADDR=127.0.0.1:4161 WAVEFORM_PUBLIC_URL=http://127.0.0.1:4161
export ACCOUNTS_URL=http://localhost:9590 ACCOUNTS_API_URL=http://127.0.0.1:9589
export WAVEFORM_APP_SECRET=sa_app_…            # Waveform's secret at that Silicon Accounts
export WAVEFORM_SPEECH_FIXTURES=true WAVEFORM_ENVIRONMENT=development
export WAVEFORM_IDEMPOTENCY_DIGEST_KEY="$(openssl rand -hex 32)"
cargo run --bin waveform-api
```

`WAVEFORM_SPEECH_FIXTURES=true` (refused in production) replaces every speech provider for the whole process:

- TTS returns the prerecorded clip of the requested (or your default) voice profile, whichever provider you select.
  Provider controls are validated but do not change the clip.
- STT returns a fixed transcript for any valid source audio.
- Saved and request-only provider keys are accepted and never used, so key flows can be exercised end to end.

Everything else is real: sign-in, history, preferences, idempotency, Briefcase access and storage. See
[configuration](configuration.md) for every variable.

## Point the CLI at it

```sh
export WAVEFORM_URL=http://127.0.0.1:4161 ACCOUNTS_URL=http://localhost:9590
waveform login                                                    # a Carbon: approve the code
silicon-accounts login --app waveform -q | waveform login --slt-stdin   # a Silicon
waveform tts 'Test speech'
```

Plain `http://` is accepted only for this machine. Sign-ins are kept per Silicon Accounts origin, so a development
sign-in never replaces your production one. Use `--profile NAME` to keep several test accounts side by side.

## Automated tests

- `cargo test --manifest-path cli/Cargo.toml` runs the real `waveform` binary in empty homes against a stub Silicon
  Accounts and a stub Waveform: the device flow, short-lived token refusals, refresh rotation across processes,
  sign-out, discovery shapes and the speech commands.
- `cargo test --manifest-path client/Cargo.toml` covers the Rust client against stubs.
- The service's own tests run against PostgreSQL with `WAVEFORM_TEST_DATABASE_URL` set
  (`cargo test --lib -- --ignored --test-threads=1`).

In your own tests, stub Waveform with any HTTP mock: the client sends `Authorization: Bearer …` (or `Proof …`),
`Idempotency-Key` on speech, and the version headers `X-Waveform-API-Version: v1` and
`X-Waveform-Protocol-Version: 1`.

## Proofs in tests

To test an app that calls Waveform for its users, register the app at your development Silicon Accounts, sign a test
account in to it, issue a User verification proof for `receiving_app` `waveform` with scope `waveform.tts` or
`waveform.stt`, and allow the app in the development Waveform's `WAVEFORM_PROOF_ISSUERS`. A proof from an app that is
not allowed for the scope is refused with `403 proof_issuer_not_allowed`.

## Bug reports

`waveform report` needs a configured mail service (`WAVEFORM_POSTMARK_TOKEN`); without one a development Waveform
answers `503 postmark_not_configured` and keeps nothing.
