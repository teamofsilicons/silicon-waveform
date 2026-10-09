# Waveform: Silicon Accounts + Silicon Apps migration progress

Branch `migrate/accounts-apps-20261010` (from `origin/main` at `114a35c`). Every stage appends a dated section:
what it did, commits, test commands and results, what is left, gotchas. Decisions live in
[`decisions.md`](decisions.md).

## 2026-10-10 — Stage 1 (service): baseline before any change

Environment: Rust 1.98.0 (`rust-toolchain.toml`), `CARGO_TARGET_DIR=$PWD/target/mig CARGO_PROFILE_DEV_DEBUG=0
CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3`, Postgres 16 on `127.0.0.1:5460` (databases `waveform_test` and
`waveform_idem_test`, created by this stage), FFmpeg at `/opt/homebrew/bin/ffmpeg`. Logs: `.mig/logs/baseline/`.

| Command | Result at baseline |
| --- | --- |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --all-targets --all-features -- -D warnings` | pass |
| `cargo test --all-targets --all-features` | pass: lib 172 passed / 16 ignored (DB), `briefcase_contract` 2 passed, `postgres_idempotency` 8 ignored |
| `cargo build --manifest-path cli/Cargo.toml --locked` | pass |
| `WAVEFORM_TEST_DATABASE_URL=…/waveform_test cargo test --lib -- --ignored --test-threads=1` | pass: 16 passed |
| `WAVEFORM_TEST_DATABASE_URL=…/waveform_idem_test cargo test --test postgres_idempotency -- --ignored --test-threads=1` | pass: 8 passed |
| `cargo test --manifest-path client/Cargo.toml` | pass: 18 passed |
| `cargo test --manifest-path cli/Cargo.toml` | pass: 30 passed (8 unit, 19 discovery, 3 speech controls) |

Nothing failed or was skipped at baseline (apart from the `#[ignore]` DB tests, which pass when pointed at Postgres).
The exact passing test names are kept in `.mig/logs/baseline/*.passed` (not committed) so later regressions can be
told apart from tests that were deliberately replaced.

The 16 DB-backed lib tests at baseline all exercise IAM, Honeycomb lifecycle or test-plane behaviour that this
migration removes by decision (D1, D2, D5): `control::tests::{session_retries_preserve_the_iam_receipt_in_both_planes,
login_settings_secrets_webhooks_and_online_revocation, environment_management_is_owned_by_honeycomb,
voice_profiles_isolate_accounts_organizations_and_test_catalogs, unscoped_identity_uses_iam_workspace_and_rejects_wrong_audience,
app_secret_discovery_revalidates_isolates_and_cleans, discovered_identity_reports_permissions_and_webhooks_are_isolated,
honeycomb_lifecycle_import_establishes_and_preserves_the_bound_plane, honeycomb_lifecycle_replays_fences_cleans_and_keeps_tombstones}`,
`control::storage::tests::feature_consent_…`, `control::tests::source_target_tests::…`, `control::contracts::tests::…`
and the four `control::tests::speech_tests::…`. Each is replaced by an Accounts-era test that covers the same product
behaviour (listed in the stage section below).

## 2026-10-10 — Stage 1 (service): Silicon Accounts, no Teams

The Rust service now authenticates and authorises with Silicon Accounts only, owns data per account, keeps every
existing row, and passes its tests. Decisions are in [`decisions.md`](decisions.md); production steps the service
needs are in [`cutover.md`](cutover.md).

### What changed

- **Dependencies:** `silicon-accounts-client = "0.4.0"`; `vendor/silicon-iam-client` and `vendor/briefcase-client`
  deleted. The client and CLI crates take `silicon-iam-client = "=5.0.0"` from crates.io until the client/CLI stage
  replaces it. `cargo deny` passes (licenses, sources, bans).
- **Configuration:** `ACCOUNTS_URL`, `ACCOUNTS_API_URL`, `WAVEFORM_APP_ID`, `WAVEFORM_APP_SECRET`,
  `WAVEFORM_ACCOUNTS_WEBHOOK_SECRET` (+ `_PREVIOUS`), `WAVEFORM_ACCOUNTS_TIMEOUT_SECONDS`, `WAVEFORM_PROOF_ISSUERS`,
  `WAVEFORM_PUBLIC_URL`, `WAVEFORM_BRIEFCASE_APP_ID` (now Briefcase's id), `WAVEFORM_BRIEFCASE_APP_FOLDER`,
  `WAVEFORM_SPEECH_FIXTURES`. http only for loopback hosts outside production. IAM and Honeycomb variables are
  ignored with a warning; `WAVEFORM_BRIEFCASE_AUDIENCE` is refused. `.env.example`, `compose.yaml` and
  `docs/configuration.md` updated.
- **Authentication:** bearer JWTs verified locally (cached key set, unknown `kid` refetch at most every 30 s),
  revocation markers from the webhook, introspection on sensitive changes; `Authorization: Proof sap_…` on the four
  speech routes with scope and issuer checks. IAM sign-in, test-environment and Honeycomb routes are gone (the sign-in
  ones answer `410 route_retired`).
- **No Teams:** everything is the requesting account's; `?account=si:…` lets a Carbon manage the Silicons it is
  custodian of (history, preferences, provider keys, Briefcase access), never speak as them. `GET /api/v1/silicons`.
- **Data:** migrations `0016_accounts_identity` (additive) and `0017_api_contract_2`; `waveform-api link-identities
  --file mapping.csv [--dry-run] [--merge] [--relink]` and `waveform-api contracts deprecate`.
- **Webhook:** `POST /webhooks/accounts`, all six events plus `ping`, deduped on `event_id`.
- **Briefcase:** User verification proofs for `briefcase`, stored encrypted, refreshed single-flight; TTS falls back
  to audio Waveform holds (`GET /api/v1/audio/{request_id}`) with an exact `storage.reason`.
- **OpenAPI:** `openapi.yaml` rewritten (bearerAuth, proofAuth, `?account=`, new routes and schemas, contract
  2.0.0); Redocly lint is clean.

### Commits

`b03c6ab` migration 0016 · `4ec705e` the Accounts service · `5afb845` vendored IAM and Briefcase clients and the IAM
catalog check removed · `f5bbb1b` fixtures take provider keys, voice and STT-folder route test · the docs commit that
adds this section (OpenAPI, configuration docs, decisions, cutover notes).

### Tests (Rust env from the baseline section; Postgres 16 on 5460)

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | pass |
| `cargo clippy --all-targets --all-features -- -D warnings` | pass |
| `cargo test --all-targets --all-features` | pass: lib 174 passed / 9 ignored (DB), `postgres_idempotency` 8 ignored |
| `WAVEFORM_TEST_DATABASE_URL=…/waveform_test cargo test --lib -- --ignored --test-threads=1` | pass: 9 passed (run 3× in a row after the race fix) |
| `WAVEFORM_TEST_DATABASE_URL=…/waveform_idem_test cargo test --test postgres_idempotency -- --ignored --test-threads=1` | pass: 8 passed. This database was created at baseline (migrations 0001–0015) and upgraded to 0017 by this run. |
| `cargo test --manifest-path client/Cargo.toml` / `cli/Cargo.toml` | pass: 18 / 30 passed |
| `cargo fmt` + `cargo clippy -D warnings` for `client/` and `cli/` | pass |
| `cargo build --manifest-path cli/Cargo.toml --locked`, `cargo build --locked --bin waveform-api` | pass |
| `cargo deny --offline check licenses sources bans` | ok (duplicate-version warnings only, as configured) |
| `npx --yes @redocly/cli@1.34.5 lint openapi.yaml` | valid, no warnings |
| baseline code (`95a9cea`) plus migration 0016, `cargo test --lib` and its 16 DB tests | pass: 172 and 16 passed — the running version keeps working while 0016 is applied (rolling deploy) |

Baseline → now: of the 172 baseline lib tests, 133 are unchanged and 39 were retired with the code they tested (IAM
adapter and OBO headers, test planes, Honeycomb lifecycle, the vendored IAM-era Briefcase client); 41 were added. The
2 `briefcase_contract` integration tests went with the vendored client; the proof adapter has 4 wiremock tests
instead. The 16 IAM-era DB tests are
replaced by 9 Accounts-era ones:

| Baseline DB test(s) | Now covered by |
| --- | --- |
| `unscoped_identity_uses_iam_workspace_and_rejects_wrong_audience`, `discovered_identity_reports_permissions_…` | `accounts_are_keyed_by_their_exact_uuid_and_silicons_learn_their_custodian`, `credentials_are_refused_with_exact_reasons_before_any_data_access` (no DB) |
| `login_settings_secrets_webhooks_and_online_revocation`, `voice_profiles_isolate_accounts_organizations_and_test_catalogs` | `a_custodian_manages_its_silicons_settings_keys_and_history_and_nobody_else_can`, `webhooks_are_verified_deduplicated_and_applied`, `voice_catalog_and_the_stt_folder_follow_the_credential` |
| `storage::feature_consent_…` | `briefcase_access_is_approved_by_the_account_and_ended_by_events` |
| `speech_tests::{discovered_tts_…, paired_stt_and_replays_…}`, `source_target_tests::…` | `speech_runs_for_accounts_and_for_apps_with_proofs`, `voice_catalog_and_the_stt_folder_follow_the_credential` |
| `speech_tests::tts_rejects_fallback_with_provider_controls_before_creating_a_job` | unit `tts_rejects_controls_that_would_be_lost_by_fallback` |
| `honeycomb_lifecycle_…` (tombstones) | `account_deletion_purges_the_accounts_data_and_keeps_a_tombstone` |
| `contracts::consumer_negotiation_and_quiet_period_are_durable` | `contract_negotiation_and_operator_deprecation_are_durable` |
| `session_retries_preserve_the_iam_receipt_…`, `app_secret_discovery_…`, `environment_management_…`, `real_cli_keeps_production_and_test_logins_separate` | removed with IAM sessions and test environments (D1, D5); retired routes covered by `discovery_is_public_and_retired_routes_say_what_replaced_them` |
| (new) | `iam_era_data_upgrades_and_links_to_accounts_without_loss` (IAM-era rows → `link-identities` → same data under the account) |

### Live check against the shared Accounts stack

`waveform-api` on `127.0.0.1:4161` with fixtures, database `waveform_smoke`, Briefcase pointed at an unused port
(4169), Accounts `http://localhost:9590` / `http://127.0.0.1:9589`. Fresh Carbon and Silicon per run; Waveform tokens
through short-lived tokens; `interface` proofs from `POST /v1/proofs/user-verification`. Waveform's webhook was pointed
at the service for the run (`PUT /v1/apps/waveform/webhook`, secret kept) and restored to
`http://127.0.0.1:9593/waveform/webhooks` afterwards. 26/26 checks passed: discovery; `/me` for both kinds (the
Silicon names its custodian); first-party token refused `wrong_audience`; proof refused on account routes; the Carbon
lists and manages its Silicon, the Silicon cannot select its custodian, an unknown id is `404 account_not_found`;
TTS held with `briefcase_unavailable` and its MP3 served only to its owner; a live Briefcase proof issued by real
Accounts (4 scopes); an `interface` proof gets `delegated_call` audio it can download, a proof without
`waveform.tts` is refused `proof_scope_missing`, history shows `issuing_app`; a signed `ping`, `account.updated`
(name cached) and `membership.access_removed` (earlier token refused `sign_in_ended`, Briefcase proof ended, the
Silicon unaffected) all arrived signed and were applied. No token or secret appeared in the service log.

Found by it and fixed: a Silicon's first `/me` lacked its custodian (the first lookup is now awaited, at most 2 s,
with a 60 s backoff); a lookup answered before a webhook change could bring back the old id (lookups no longer
overwrite ids or custodians changed since they started; a mutation check confirmed the test catches it).

### Blocked on

- **Briefcase's Accounts-era contract** is not published. The adapter's assumptions (routes, `apps/waveform`
  folder, permanent URL shape) are isolated in `src/infrastructure/briefcase.rs` and listed in `decisions.md`; the
  integration stage must reconcile them with Briefcase's release.

### Left for later stages

- Client crate and CLI: move to `silicon-accounts-client` (drop the `=5.0.0` IAM pin), `waveform login` /
  `--slt-stdin`, `--account`, `waveform storage approve` (`POST /api/v1/storage-authorizations`), held audio
  (`audio_url`), remove test-environment commands, version 0.6.0.
- Web: Next.js + Arc UI BFF.
- Silicon Interface's switch to proofs and the production Accounts setup (`cutover.md`).
- Packaging and release: `deploy/aws`, `deploy/native`, `scripts/{import-iam-identities,test-iam-*,verify-identifier-migration,e2e-local-stack,e2e-provider-controls-http,sync-honeycomb,package-*}.py`,
  `honeycomb.yaml`, Honeycomb steps in `ci.yml` / `release.yml` / `backend-release.yml`.
- Docs: README, `docs/*.md` (the CLI part of `configuration.md` still describes Honeycomb), `API_DOCS.md`,
  `docs-site/`, `understanding-proposal.md`.

### Gotchas

- `psql` is not on PATH: `/opt/homebrew/opt/postgresql@16/bin/psql`.
- On the shared stack Waveform's sign-in setup offers only Apple and Google, so the hosted email sign-in
  (`mint.mts app-signin`) fails with `method_not_enabled`; mint a Carbon's Waveform token with
  `POST /v1/me/short-lived-tokens {"app_id":"waveform"}` (first-party token) and `mint.mts app-token`.
- App-authenticated `GET /v1/accounts/{uuid}` returns no display name or photo (only id, kind, status, custodian);
  names arrive in token claims and `account.updated`.
- `infrastructure::providers::common::tests::skips_retry_when_the_total_budget_cannot_fit_it` (untouched) uses a
  20 ms timeout and failed once under a parallel build; it passes alone and on reruns.
- A DB test whose stub keeps answering stale data will expose any lookup still in flight; await first lookups rather
  than sleeping.
