# Silicon Waveform

Silicon Waveform is the organization-scoped speech service for Carbons,
Silicons, and IAM applications. It provides one synchronous, provider-neutral
API for text-to-speech and speech-to-text, applies the documented fallback
chains, and stores generated MP3 files in Silicon Briefcase.

The product contract lives in [`UNDERSTANDING.md`](./UNDERSTANDING.md),
[`API_DOCS.md`](./API_DOCS.md), and [`openapi.yaml`](./openapi.yaml). Material
implementation choices and contract interpretations are recorded in the
append-only [`decisions.md`](./decisions.md).

## Current implementation status

Bearer TTS and test-plane TTS use official IAM proof exchange and Briefcase
uploads. Personal provider keys are selected per account, and CLI sessions
are isolated by server and test plane. STT source reads and TTS/STT replay
access checks use the Briefcase 0.2 delegated API. Inbound OBO speech remains
unavailable because IAM does not issue a downstream subject token from a
consumed proof. See [the implementation checklist](./docs/IMPLEMENTATION.md) for
remaining work and the distinction between local mocks and deployed tests.

## Public API

The production base URL is
`https://waveform.teamofsilicons.com/api/v1`.

- `POST /tts` accepts text and an optional BCP 47 language hint, synthesizes
  speech, normalizes it to MP3, stores it in Briefcase, and returns a permanent
  URL. `temporary_url` is nullable.
- `POST /stt` accepts a permanent Briefcase file URL and optional BCP 47
  language hint, re-authorizes the file read, and returns a normalized
  transcript.
- `GET /capabilities` returns stable Waveform-level language and media
  capabilities without authentication.

Both speech routes require `X-Org-ID` and `Idempotency-Key`, plus exactly one of
an IAM Bearer token or the `X-IAM-OBO-Access-Proof`/`X-App-ID` pair.

Operational probes are outside the product API:

- `GET /health/live`
- `GET /health/ready`

## Provider order

TTS:

1. Gemini `gemini-3.1-flash-tts-preview`
2. ElevenLabs `eleven_multilingual_v2`
3. OpenAI `tts-1`

STT:

1. Gemini `gemini-3.5-transcribe`
2. OpenAI `gpt-transcribe`
3. Deepgram `nova-3` multilingual

Provider adapters are isolated and bounded. They do not leak provider response
schemas or private errors into the public API.

## Architecture

```text
HTTP API
  -> authentication and organization authorization
  -> durable idempotency lease/replay
  -> purpose-bound downstream delegation
  -> TTS: ordered provider ports -> MP3 normalization -> Briefcase storage
  -> STT: authorized Briefcase read -> ordered provider ports
  -> normalized response
```

The crate is a modular monolith:

- `src/domain` contains validated values and provider-independent models.
- `src/application` contains use cases and object-safe ports.
- `src/api` owns Axum extraction, routing, deadlines, and public errors.
- `src/infrastructure` owns PostgreSQL, IAM, Briefcase, provider, and FFmpeg
  adapters.
- `migrations` contains forward-only PostgreSQL migrations.

PostgreSQL is required for production idempotency. During the configured
retention window (24 hours by default and never less), it retains scoped
lifecycle state, completed STT results, and only the durable fields of a
completed TTS result. It never persists an expiring TTS delivery URL. Every
completed replay still performs online IAM authorization: STT rechecks current
read access to the exact source before releasing the cached transcript, while
TTS rechecks access to the generated file and returns its permanent URL with
`temporary_url: null`.
Speech providers, audio normalization, and content mutations are not repeated.
Content-free provider-attempt outcomes and latency are emitted as structured
telemetry rather than transactional data. Waveform never persists request text,
source media, generated audio, credentials, raw provider responses, or provider
error bodies. Database access, encryption, backups, and retention must use the
speech-data classification.
Canonical request digests are HMAC-SHA-256 values under the dedicated
`WAVEFORM_IDEMPOTENCY_DIGEST_KEY`; this prevents a database reader from testing
likely text or URLs offline. Generate this key independently from every other
credential. Rotate it only after records made with the prior value have aged
out during a planned drain; changing it sooner fails safely as idempotency-key
conflicts for still-retained records.

## Local development

Prerequisites:

- Rust 1.98 (installed automatically by `rust-toolchain.toml`)
- PostgreSQL 16 or newer
- FFmpeg with the `libmp3lame` encoder

Create local configuration and start PostgreSQL:

```bash
cp .env.example .env
docker compose up -d postgres
```

Development allows individual provider keys to be empty. Production validates
the complete fallback chain and requires secure dependency URLs, secrets, and
JSON telemetry. Never commit `.env`.

Run the service:

```bash
cargo run --bin waveform-api
```

Run the complete local quality gate:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
WAVEFORM_TEST_DATABASE_URL=postgres://silicon_waveform:silicon_waveform@127.0.0.1:5432/silicon_waveform \
  cargo test --test postgres_idempotency -- --ignored --test-threads=1
cargo deny check
```

## Current dependency-contract blockers

Waveform intentionally fails closed where neighboring contracts are incomplete:

- IAM's current OBO verifier binds proofs to the exact request method, path,
  and body digest; the released Waveform adapter does not yet receive those
  raw request bytes and therefore fails closed. IAM also has no operation to
  exchange the verified actor context for a new Briefcase-audience proof.
- Briefcase requires an upload `parent_id` but has no operation to resolve or
  create the represented actor's `apps/{app_id}` folder.
- Briefcase metadata and delivery-URL operations require an entry ID, while
  Waveform receives a permanent file URL. Briefcase has no published
  authenticated permanent-URL-to-entry-ID resolution or direct content-read
  operation for that reference.

The shipped composition therefore reports `503 not_ready` from
`/health/ready`. Otherwise-valid speech work that reaches downstream delegation
fails with `503` at the first missing contract boundary before any provider is
called or billed.

Those capabilities are ports with complete workflow tests; no audience-bound
proof is forwarded and no caller URL is fetched directly. Once IAM and
Briefcase publish the missing operations, their HTTP adapters can be completed
without changing the public API or application services. See D-008, D-027, and
D-051 in [`decisions.md`](./decisions.md) for the governing boundaries.
