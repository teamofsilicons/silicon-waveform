# Waveform implementation checklist

`UNDERSTANDING.md` is the product source of truth. This file records what is
implemented in the worktree and what still depends on upstream contracts or
additional product work.

## Implemented backend behavior

- Official Silicon IAM SDK control-plane login, refresh, logout, live
  authorization, signed webhook verification, and event deduplication.
- Account preferences and encrypted provider-key storage. Personal keys are
  loaded for the exact authorized account and plane, selected in request-local
  adapters, and never overwrite shared deployment credentials.
- Isolated test-environment create/list/detail/key/rotate/delete/restore/clean
  routes, creator/admin/owner checks, root-key hashing, encrypted upstream
  credentials, inactivity soft-delete, recovery, and permanent purge.
- Process-level hourly retention cleanup plus request-time cleanup fallback;
  permanent purge cascades scoped idempotency state.
- Deterministic test-plane speech providers with the text from
  `UNDERSTANDING.md`. The packaged spoken fixture is seeded into PostgreSQL
  and loaded from that database for each test TTS request. TTS uploads it through the paired IAM
  and Briefcase SDK clients and returns a permanent URL with null temporary URL.
  Runtime storage does not use fabricated fixture URLs.
- Actor-scoped running, failed and completed jobs use the canonical idempotent
  operation ID. History creation precedes provider work; completion and failure
  share transactions with idempotency state. Lease fencing protects retries,
  cancellation releases the owned attempt, and expired attempts are recovered
  during history reads and scheduled cleanup.
- Production TTS/STT provider adapters, bounded request admission, idempotency
  leases, replay access checks, media normalization, and stable error mapping.
- Local PostgreSQL runner, fixture integration coverage, and documentation for
  the test-plane workflow.

## Implemented Rust client and CLI behavior

- Typed authentication, account, provider-key, job, preference, speech, and
  test-environment lifecycle methods.
- Test-plane selection on every request, root-key/UUID resolution with a
  private local UUID-to-key map, credential redaction, bounded transport, and
  structured errors.
- Stateful CLI login/refresh/logout, positional or hidden-flag SLT input,
  configurable private state home (`config home`), server/test-scoped sessions
  in `.waveform/dir`, `--test` guardrails, JSON
  output, job pagination, provider-key and preference commands, and complete
  test-env lifecycle commands.

## Scope and remaining optional work

The required backend, Rust client, and CLI scope above the "Later to do"
section of `UNDERSTANDING.md` is implemented. Outbound Waveform-to-Briefcase
OBO storage is exercised against real paired IAM and Briefcase environments.
Inbound OBO speech is a separate, unsupported entry mode: IAM does not provide
an onward subject-token handoff for a consumed incoming proof. Bearer login
supports both Carbon and Silicon actors.

The UI and `waveform report` are explicitly deferred in `UNDERSTANDING.md`.
Hourly updater scheduling, opt-out, and state handling are implemented and
covered locally; installing a genuinely newer published Waveform release was
not exercised. The final speech tests use the required deterministic testing
providers, so they do not evaluate live paid-provider output or availability.

## Verification evidence

Validation is rerun on the current worktree. The local suite covers provider
requests, encrypted personal-key scope/deletion, account configuration,
environment lifecycle, webhooks, and PostgreSQL idempotency concurrency.
Additional regression tests cover:

- Personal keys reaching real TTS/STT HTTP adapters while the shared provider
  remains unchanged and an adapter without any key makes no network call.
- Two HTTP TTS requests and an authorized replay using the exact spoken fixture, distinct filenames,
  paired IAM and Briefcase test headers, signed digest-bound exchanges, raw SDK
  uploads, and completed job rows.
- Carbon and Silicon STT, pagination, single-use exact-byte read proofs and
  replay denial after storage access is revoked.
- Decoded source duration, invalid-audio rejection and bounded codec execution.
- Compiled CLI login, `me`, refresh, UUID/root-key selection, test logout, and
  preservation of the production session over a local HTTP server.

Those regression tests use mock upstream transports. They are supplemented by
an independent real-service run on 2026-09-08: compiled CLI → Waveform → real
IAM → real Briefcase → PostgreSQL/MinIO, with paired testing keys and real
Carbon/Silicon sessions. It passed **67 distinct real-service assertions**.
See [the full test report](test-report-2026-09-08.md) for the requirement matrix,
fixes, evidence, and verification limits.

Final regression validation: **166 tests passed** (140 backend tests, 13
explicitly enabled PostgreSQL/HTTP tests, 8 Rust client tests, 5 CLI tests).
The compiled CLI regression also verifies that `--json` session commands emit
parseable JSON. Formatting, strict Clippy for all three crates, API/CLI builds,
and `git diff --check` passed.
