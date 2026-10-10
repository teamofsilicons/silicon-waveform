# Silicon Waveform

Silicon Waveform is the speech service for Carbons and Silicons. It provides one synchronous, provider-neutral API for
text-to-speech and speech-to-text, applies explicit TTS and automatic STT fallback chains, and stores generated MP3s in
the requesting account's Briefcase files. Waveform signs everyone in with
[Silicon Accounts](https://accounts.teamofsilicons.com), and the `waveform` CLI is installed and updated by
[Silicon Apps](https://apps.teamofsilicons.com).

## Use it

```sh
silicon-apps install waveform
waveform login                                                   # a Carbon: approve a code in the browser
silicon-accounts login --app waveform -q | waveform login --slt-stdin   # a Silicon: hand over a short-lived token
waveform tts 'Hello, Carbons and Silicons.'
waveform stt https://briefcase.teamofsilicons.com/…/voice-note.m4a
```

Carbons can also use the website at https://waveform.teamofsilicons.com. Everything belongs to the account that made
it (speech history, settings, provider keys and audio); a Carbon can manage the Waveform settings and history of the
Silicons it looks after, and never speaks as them. Read the
[usage and development documentation](https://docs.waveform.teamofsilicons.com/): start with the
[CLI guide](docs/cli.md), then [Sign in with Silicon Accounts](docs/accounts.md), the [API guide](docs/api.md) and
the [Rust client](docs/client.md).

The product contract lives in [`UNDERSTANDING.md`](./UNDERSTANDING.md) (Carbon-owned), [`API_DOCS.md`](./API_DOCS.md)
and [`openapi.yaml`](./openapi.yaml). Implementation choices are recorded in the append-only
[`decisions.md`](./decisions.md); the move to Silicon Accounts and Silicon Apps is in [`docs/migration/`](docs/migration/),
and records of earlier releases are in [`docs/history/`](docs/history/).

## Public API

The base URL is `https://backend.waveform.teamofsilicons.com/api/v1`.

- `POST /tts` synthesizes text (with an optional BCP 47 language hint and voice profile), normalizes it to MP3, stores
  it in the account's Briefcase files and returns its permanent URL. When Briefcase cannot take it, the request still
  completes and Waveform holds the MP3 for the idempotency retention (`GET /audio/{request_id}`), saying why.
- `POST /stt` transcribes a permanent Briefcase file URL (optional language hint), reading the file as the account.
- `GET /capabilities` returns the languages, media types and provider controls, without authentication.
- History, preferences, saved provider keys, voice profiles, Briefcase access, bug reports and the signed-in account
  are described in the [API guide](docs/api.md).

Every route except discovery takes exactly one credential: `Authorization: Bearer <access token>`, a Silicon Accounts
access token issued to `waveform`, or, on the speech routes, `Authorization: Proof sap_…`, a User verification proof
another app holds for an account. Speech also needs an `Idempotency-Key`. Operational probes are outside the product
API: `GET /health/live` and `GET /health/ready`. Silicon Accounts posts signed account events to `POST /webhooks/accounts`.

## Provider order

TTS (only the first selected provider runs unless `auto_fallback: true`):

1. Gemini `gemini-3.1-flash-tts-preview`
2. ElevenLabs `eleven_multilingual_v2`
3. OpenAI `tts-1`

STT:

1. Gemini `gemini-3.5-transcribe`
2. OpenAI `gpt-transcribe`
3. Deepgram `nova-3` multilingual

Provider adapters are isolated and bounded. TTS supports per-provider performance controls with fallback off, and
reports a safe reason when the selected provider fails. Saved encrypted keys and request-only keys override
Waveform's own credentials. See [the API guide](docs/api.md) and [the full reference](API_DOCS.md).

## Architecture

```text
HTTP API
  -> Silicon Accounts authentication (local JWT check, or proof verification) and the custodian rule
  -> durable idempotency lease/replay
  -> TTS: ordered provider ports -> MP3 normalization -> Briefcase storage with a User verification proof
          (or Waveform-held audio when Briefcase cannot take it)
  -> STT: Briefcase read with a User verification proof -> ordered provider ports
  -> normalized response
```

The crate is a modular monolith:

- `src/domain` contains validated values and provider-independent models.
- `src/application` contains use cases and object-safe ports.
- `src/api` owns Axum extraction, routing, deadlines and public errors.
- `src/control` owns accounts, history, settings, provider keys, Briefcase access, the Accounts webhook, reports and
  API contracts.
- `src/infrastructure` owns PostgreSQL, Silicon Accounts, Briefcase, provider and FFmpeg adapters.
- `migrations` contains forward-only PostgreSQL migrations; `waveform-api link-identities` links data from before
  Silicon Accounts to the accounts it belongs to, at cutover.

PostgreSQL is required for production idempotency. During the configured retention window (24 hours by default and
never less) it keeps scoped lifecycle state, completed STT results, and only the durable fields of a completed TTS
result. Speech providers, audio normalization and content mutations are not repeated on replay. Content-free
provider-attempt outcomes and latency are emitted as structured telemetry rather than transactional data. Waveform
never persists request text, source media, credentials, raw provider responses or provider error bodies; generated
audio is kept only while it is held for an account that Briefcase could not serve. Database access, encryption,
backups and retention must use the speech-data classification.

Canonical request digests are HMAC-SHA-256 values under the dedicated `WAVEFORM_IDEMPOTENCY_DIGEST_KEY`, so a database
reader cannot test likely text or URLs offline. Generate this key independently from every other credential. Rotate
it only after records made with the prior value have aged out during a planned drain; changing it sooner fails safely
as idempotency-key conflicts for still-retained records.

## Local development

Prerequisites: Rust 1.98 (installed by `rust-toolchain.toml`), PostgreSQL 16 or newer, FFmpeg with `libmp3lame`, and
a development Silicon Accounts (the [testing guide](docs/testing.md) shows how to run Waveform against one with
deterministic speech fixtures).

```bash
cp .env.example .env
docker compose up -d postgres
cargo run --bin waveform-api
```

Development allows individual provider keys to be empty. Production validates the complete fallback chain and
requires secure dependency URLs, secrets and JSON logs. Never commit `.env`.

The complete local quality gate:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
WAVEFORM_TEST_DATABASE_URL=postgres://silicon_waveform:silicon_waveform@127.0.0.1:5432/silicon_waveform \
  cargo test --lib -- --ignored --test-threads=1
WAVEFORM_TEST_DATABASE_URL=postgres://silicon_waveform:silicon_waveform@127.0.0.1:5432/silicon_waveform \
  cargo test --test postgres_idempotency -- --ignored --test-threads=1
cargo test --manifest-path client/Cargo.toml
cargo test --manifest-path cli/Cargo.toml
python3 scripts/sync-cli-docs.py --check
python3 scripts/test_package_apps.py
python3 deploy/native/test_upgrade.py
python3 deploy/native/test_accounts_cutover.py
cargo deny check
```

## Releases and deployment

- The CLI ships through Silicon Apps: `scripts/package-apps.sh` builds one `apps.yaml` archive per target, and the
  release workflow builds all six. See [releases](docs/releases.md).
- The service runs natively on the Waveform host: [`deploy/native`](deploy/native/README.md). The one-time move to
  Silicon Accounts is the [cutover runbook](docs/migration/cutover.md).
- Configuration: [`docs/configuration.md`](docs/configuration.md) and [`.env.example`](.env.example).
