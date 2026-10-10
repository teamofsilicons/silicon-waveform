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

## Everything on one machine, end to end

In a checkout of the repository, `scripts/dev-accounts.sh` runs Waveform against a development Silicon Accounts with
one command, and `scripts/e2e-accounts.sh` proves the whole product against it with real tokens:

```sh
export WAVEFORM_DEV_STACK=/path/to/stack.json   # the development Silicon Accounts (below)
scripts/dev-accounts.sh start                   # API on 127.0.0.1:4161, a Briefcase stand-in on :4162
eval "$(scripts/dev-accounts.sh env)"           # WAVEFORM_URL and ACCOUNTS_URL for the CLI
waveform login
scripts/dev-accounts.sh stop
```

The stack description is JSON: `accounts_public_url`, `accounts_api_url`, optionally `fake_apps_url`, and
`apps.waveform.app_secret` and `apps.briefcase.app_secret` (development secrets only; the script refuses a Silicon
Accounts that is not on this machine). `start` is idempotent: it creates and migrates the database (`waveform_e2e` on
`127.0.0.1:5460`), keeps encryption keys in `.mig/dev/`, makes Waveform's sign-in setup there offer email codes, the
device flow, the CLI as a public client and the local callbacks, points Waveform's webhook at the API with a fresh
secret (kept in `.mig/dev/`, never in the repository), starts a stand-in Briefcase that verifies every proof with
Briefcase's own development credentials (or uses the Briefcase at `WAVEFORM_BRIEFCASE_BASE_URL`), starts the API
with speech fixtures, and waits until a signed test event arrives. `restart-api`, `status` and `stop` do what they say.

`scripts/e2e-accounts.sh` starts a fresh stack and checks, with new Carbons and Silicons on every run: a Carbon on the
API, a Silicon on the CLI, the device flow, custodians and outsiders, account events (an id change, access removed, a
custodian transfer, forged and replayed deliveries), proofs from other apps and Waveform's own proofs to Briefcase
(refresh, recovery and revocation), discovery from a packaged archive, and a restart. It prints PASS or FAIL for every
check, keeps a transcript without secrets in `.mig/e2e/`, and exits 1 when anything failed (`--help` lists what it
needs).

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
