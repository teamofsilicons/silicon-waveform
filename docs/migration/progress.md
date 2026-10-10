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

## 2026-10-10 — Stage 2 (client and CLI): Silicon Accounts sign-in, no Teams

The Rust client (`silicon-waveform-client` 0.6.0) and the `waveform` CLI (`waveform-cli` 0.6.0) speak Silicon
Accounts only, follow the Silicon Apps CLI contract, and work against the migrated service. Decisions:
[`decisions.md`](decisions.md#client-and-cli-stage-2); production steps: [`cutover.md`](cutover.md) section 5.

An earlier run of this stage was interrupted after writing most of the client and the CLI modules (uncommitted, last
edit 05:44). This run reviewed that work file by file against the service and `silicon-accounts-client` 0.4.0
sources, kept it, and finished it (`main.rs`, `retired.rs`, `accounts`/`docs`/`config` commands, the tests, the docs).

### What changed

- **Client:** no IAM types, organizations, actors, test environments or updater. `signin::SignIn` (device flow with
  `interval`/`slow_down`, secretless short-lived token exchange with exact refusal reasons, rotating refresh,
  revoke); `Auth::{Anonymous, Bearer, Proof}`; `for_account` (custodian); typed `ApiError` keeping
  `{error:{code,message,hint,reason,request_id,provider…}}`; `accounts()`, `silicons()`, `held_audio`,
  `approve_storage`/`storage`/`revoke_storage`; the API-version handshake headers unchanged.
- **CLI:** `login` (device flow, `--open`, `--label`, `--json` line stream), `login --slt`/`--slt-stdin`/positional,
  `login status [--json] [--offline]`, `logout` (revokes), `refresh`, `accounts --json`, hidden `iam`, `me`,
  `silicons`, `tts` (`--text-file`, `--proof-file`), `stt`, `stt-folder`, `audio`, `jobs`, `preferences`,
  `voice-profiles`, `provider-keys [list|set|delete]`, `storage approve|status|revoke`, `telemetry`, `report`,
  `capabilities`, `contracts`, `docs`, `config show|home|telemetry`. Sessions: per profile and Silicon Accounts
  origin, `0600`, atomic, file lock, single-flight refresh. Retired flags/commands fail with exit 2 and the
  replacement. `daemon.rs`/`updater.rs` deleted. Space Station telemetry unchanged (opt-out, step names only).
- **Docs:** `cli/README.md` (= `docs/cli.md`, the long help), `docs/{README,accounts,api,client,testing}.md` and the
  CLI half of `configuration.md` rewritten; `docs/iam.md` → `docs/history/iam.md`; the sync script covers the two
  READMEs too; docs-site nav lists `accounts.md`.
- **Service:** `?account=` ids Silicon Accounts calls `invalid_id` now answer `404 account_not_found` (was 503).

### Commits

`433a7c3` feat!: client and CLI 0.6.0 sign in with Silicon Accounts · `5195e4b` fix: answer 404 for an account id
Silicon Accounts calls invalid · `56e8810` feat(cli): refuse --account where only the account itself may act · the
docs commit that adds this section.

### Tests (Rust env from stage 1)

| Command | Result |
| --- | --- |
| `cargo test --manifest-path client/Cargo.toml` | pass: 5 unit, 13 `api`, 9 `signin`, 4 doctests (3 from the README) |
| `cargo test --manifest-path cli/Cargo.toml` | pass: 15 unit (session perms/lock/corruption/legacy, home, retired spellings, argument grammar incl. clap `debug_assert`), 9 `discovery`, 12 `signin`, 6 `speech` |
| `cargo fmt --check` + `cargo clippy --all-targets --all-features -D warnings` in `client/`, `cli/` and the root | pass |
| `cargo build --locked` for `client/` and `cli/`; `cargo package --allow-dirty` in `client/` (verifies) | pass; no `silicon-iam-client`/`briefcase-client` in either lockfile |
| `python3 scripts/sync-cli-docs.py --check` | pass (it failed at the start of this stage: `configuration.md` was stale) |
| `cargo test --all-targets --all-features` (root) | pass: lib 174 passed / 9 ignored |
| `WAVEFORM_TEST_DATABASE_URL=…/waveform_test cargo test --lib -- --ignored --test-threads=1` | pass: 9 passed |
| `cargo test --lib lookups_and_revocations` with the 404 fix reverted | fails (the new assertion catches the bug); passes with it |
| `~/.apps/bin/silicon-apps validate <scratch dir>` (apps.yaml + `bin/waveform`, macos-aarch64, local only) | `"valid": true` |

The stub-based CLI tests run the real binary in empty homes: device flow pending → slow_down (interval 1 → 6) →
success; denied and expired (exit 3); SLT from stdin/argument/flag with `client_id=waveform` and no secret; each
`invalid_grant` description → its code and hint; four concurrent processes with an expiring token → exactly one
refresh, all succeed; a stale-token 401 → one refresh and one retry; an ended sign-in deleted; logout revokes (and
still deletes the file when Silicon Accounts answers 503); `login status` offline / refused / unreachable; golden
`accounts --json`; help tree free of org/IAM/Honeycomb/test-env wording; legacy and corrupted session files.

### Live run against the shared stack and the migrated service

Service: `waveform-api` from this branch on `127.0.0.1:4161`, database `waveform_cli` (dropped afterwards),
`WAVEFORM_SPEECH_FIXTURES=true`, Briefcase pointed at unused `127.0.0.1:4169`, `ACCOUNTS_URL=http://localhost:9590`,
`ACCOUNTS_API_URL=http://127.0.0.1:9589`, `WAVEFORM_PROOF_ISSUERS='waveform.tts=interface;waveform.stt=interface'`.
CLI env: fresh `SILICON_HOME=HOME=<scratch>/home-*`, `WAVEFORM_URL=http://127.0.0.1:4161`,
`ACCOUNTS_URL=http://localhost:9590`. Identities: `mint.mts silicon --custodian-email waveform-cli-599174@example.test
--handle waveform-cli-599174` → Silicon `si:waveform-cli-599174` (uuid `AYF`), custodian `c:waveform-cli-599174`
(uuid `YEW`).

Silicon:
- `mint.mts slt --silicon si:waveform-cli-599174 --stk … --app waveform` → `printf %s "$SLT" | waveform login --slt-stdin`
  → "Signed in to Waveform as si:waveform-cli-599174 … custodian c:waveform-cli-599174 … checked yes", exit 0.
- `waveform login status --json` → `{"authenticated":true,"uuid":"AYF","id":"si:waveform-cli-599174","kind":"silicon",
  "custodian":{"id":"c:waveform-cli-599174","uuid":"YEW"},"expires_at":…,"refresh_expires_at":…,"method":"slt","verified":true,…}`.
- The same SLT again → `{"error":{"code":"slt_already_used",…,"hint":"Short-lived tokens work once. Mint a fresh one …"}}`, exit 3.
  The SLT appeared in no output and no file.
- `waveform me --json` (custodian shown); `waveform tts "Hello from a Silicon." --voice-profile puck --idempotency
  cli-stage-silicon-0001 --json` → `storage {"status":"held","reason":"briefcase_unavailable",…}`, `audio_url`;
  replay with the same key → same `request_id`; `waveform audio <id> --output speech.mp3` → 184364 bytes, "MPEG ADTS,
  layer III"; `waveform jobs --limit 5 --json`; `waveform jobs --job-id <id> --wait --json` → completed;
  `waveform preferences --voice-profile sulafat --tts-order elevenlabs,gemini --json`; `waveform voice-profiles` →
  30; `waveform storage status --json` → `active`, the four `briefcase.*` scopes (created by the first TTS, from a
  real Silicon Accounts proof); `waveform refresh --json` → `refreshed: true`; `waveform silicons --json` → `[]`.
- Single-flight, live: the saved access token was marked as expiring in 20 s and four `waveform me --json` ran at
  once → all exit 0, and the rotated family still refreshed afterwards (a reused refresh token would have revoked it).
- `waveform logout --json` → `{"signed_out":true,"revoked":true,…}`; `login status --json` → `{"authenticated":false}`
  (exit 0), text form exit 1; presenting the old refresh token to `/v1/oauth/token` → `invalid_grant … revoked at …
  (app_revoked)`.

Carbon:
- `waveform login --json --label "waveform CLI stage test"` in the background → first line `{"event":"device_code",
  "user_code":"MR3P-RVGJ","verification_uri":"http://localhost:9590/device",…,"interval":5}`; `mint.mts approve
  --email waveform-cli-599174@example.test --code MR3P-RVGJ` → 204; the CLI finished with
  `{"event":"signed_in","id":"c:waveform-cli-599174","kind":"carbon","method":"device","verified":true,…}`.
- As custodian: `waveform silicons` → `si:waveform-cli-599174  uuid AYF`; `waveform jobs --account si:waveform-cli-599174`
  → the Silicon's TTS job; `waveform preferences --account … --stt-order deepgram`; `printf %s <key> | waveform
  provider-keys set openai --key-file - --account …` → saved (the key appeared in no output), listed, deleted;
  `waveform storage status --account …` → `active`; the Carbon's own `waveform tts` → held audio.
- Refusals (after the fixes): `waveform tts 'Hi' --account si:…` → exit 2 `speech_runs_as_yourself`;
  `waveform storage approve --account si:… --json` → exit 2 `approval_by_the_account_only`;
  `waveform jobs --account si:nobody-looks-after-this-one-599174` → exit 4 "No Silicon Accounts account has the id or
  uuid …" (before the service fix: exit 1 "Silicon Accounts is temporarily unavailable"). `waveform logout` → revoked.

Proofs: `mint.mts app-signin --app interface --email waveform-cli-599174@example.test --redirect
http://127.0.0.1:9593/interface/callback --exchange`, then `POST /v1/proofs/user-verification` (interface's
credentials, `receiving_app` `waveform`, scopes `["waveform.tts"]`) → `sap_…` in a `0600` file. In an empty home:
`waveform tts "Spoken for an app." --proof-file proof.txt --json` → `storage.reason: "delegated_call"`;
`waveform audio <id> --output proof.mp3 --proof-file proof.txt` → 182797 bytes MP3; `printf %s "$PROOF" | waveform stt
… --proof-file -` → exit 3, `reason: proof_scope_missing`; `login status --json` → `{"authenticated":false}`; nothing
written to the home; the proof appeared in no output.

Discovery in an empty, scrubbed environment (`env -i HOME=$E SILICON_HOME=$E`): `waveform --help` exit 0 (14.7 KB);
`waveform accounts --json` exit 0 with `{"app_id":"waveform","accounts_url":"https://accounts.teamofsilicons.com",
"api_url":"https://backend.waveform.teamofsilicons.com","docs_url":…,"version":"0.6.0","proof_scopes":[…],"sign_in":{…}}`;
`waveform login status --json` exit 0 `{"authenticated":false}`; `waveform iam --json` → same object; no file created.

### Blocked on

- Nothing new. Briefcase's Accounts-era contract (stage 1) still decides where TTS lands; the CLI only reports what
  the service answers.

### Left for later stages

- Ship: `apps.yaml` packaging, `release.yml`/`ci.yml` (the CI `sync-cli-docs --check` step keeps working), remove
  `honeycomb.yaml`/Honeycomb scripts, publish order (client before CLI), move the remaining IAM/Honeycomb/dated docs
  to `docs/history/` and exclude it from the docs site, the root README and `API_DOCS.md`.
- Web: Next.js + Arc UI BFF (the CLI's `docs/accounts.md` describes the website's sign-in as the hosted pages with
  PKCE and a server-held session).
- Stemcell: drop `--org/--actor` from its voice-note instructions; its `default_tts_provider: google` is not a
  Waveform provider id (`gemini`).

### Gotchas

- `silicon-accounts-client` 0.4.0 lacks a secretless SLT exchange and a non-first-party public revoke; `SignIn`
  posts both forms itself (see decisions).
- The shared stack's issuer is `http://localhost:9590`; the CLI's `ACCOUNTS_URL` must be that origin (the session file
  is keyed by it), while the service also needs `ACCOUNTS_API_URL=http://127.0.0.1:9589`.
- Restarting the service with a new `WAVEFORM_ENCRYPTION_KEY` orphans the Briefcase proofs it stored; keep the key
  fixed across restarts in local runs.
- Tool-call JSON decodes `\uXXXX` escapes: writing a test that needs a literal `\ud83d` through a heredoc produced the
  emoji instead. Build such strings with `chr(92)` (Python) or check the result with `cat -v`.

## 2026-10-10 — Stage 3 (ship): packaging, CI, deployment and documentation

Everything around the code now says and does Silicon Accounts and Silicon Apps. Nothing was pushed, released or
deployed. Decisions: [`decisions.md`](decisions.md#ship-stage-packaging-ci-deployment-docs); production runbook:
[`cutover.md`](cutover.md); proposed contract text: [`understanding-proposal.md`](understanding-proposal.md).

### What changed

- **Packaging:** `packaging/apps.yaml.in`, `scripts/package-apps.sh` (entry point) + `scripts/package_apps.py`
  (unit-tested). One archive per target with `apps.yaml` for that target and `bin/waveform[.exe]`; refuses
  non-native or dynamically linked Linux binaries, a version other than `cli/Cargo.toml`, and wrong discovery answers
  when the binary can run here; hermetic `silicon-apps validate`/`pack`; re-validates the extracted archive.
  `honeycomb.yaml` (and its frontend copy), `sync-honeycomb.py`, `package-release.py` and `install.sh` deleted; the
  docs site no longer copies them, and publishes neither `docs/history/` nor `docs/migration/`.
- **CI:** `release.yml` (tag check, same six targets and toolchains, per-target packing on each runner with
  `PACKAGE_DISCOVERY=require`, aggregate `SHA256SUMS`, artifact `waveform-silicon-apps-release`); `ci.yml` (new
  `silicon-apps-package` job, Python tooling tests, manifest checks removed, no CLI build before DB tests);
  `backend-release.yml` (manifest checks removed); `linux-cli-smoke.yml` (runs the three commands from the archive).
- **Deployment:** new `deploy/native/accounts_cutover.py` (+ tests) for the one-time move with a database-restoring
  rollback; `upgrade.py` requires an Accounts-era `native.env` and checks the app credentials first; `install.py`,
  `deploy/aws/install.py` and `install-frontend.py` moved to `WAVEFORM_APP_SECRET`, the webhook secret, the proof
  issuers and the Next.js website's settings; both deploy READMEs rewritten. IAM 3 cutover tooling, the IAM identity
  scripts and the mock-IAM e2e scripts retired (in git history).
- **Docs:** README, `API_DOCS.md`, `docs/{releases,development,api-contracts,voice-profiles}.md` rewritten;
  `docs/client.md` and `docs/configuration.md` touched (bundled copies synced); 19 dated reports, the IAM/Honeycomb
  guides, the 0.5 implementation record, the earlier release notes and 3 dated deploy records moved to
  `docs/history/` (index in `docs/history/README.md`, links repointed); root `decisions.md` entry appended;
  `cutover.md` completed; `understanding-proposal.md` written.

### Commits

`56069b7` build: package the CLI for Silicon Apps instead of Honeycomb · `f62c191` ci: build Silicon Apps archives
per target and drop the Honeycomb steps · `8c7d3ec` chore: retire the one-time IAM-era cutover and test tooling ·
`6495602` deploy: run the API and website on Silicon Accounts · `c833309` docs: describe Waveform on Silicon Accounts
and Silicon Apps · `812046e` docs(migration): write the cutover runbook and the UNDERSTANDING proposal · the commit
that adds this section.

### Tests (Rust env from stage 1; Postgres 16 on 5460; logs in `.mig/logs/ship/`)

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check`; `cargo clippy --all-targets --all-features -- -D warnings` | pass; pass |
| `cargo test --all-targets --all-features` | pass: lib 174 passed / 9 ignored, `postgres_idempotency` 8 ignored |
| `WAVEFORM_TEST_DATABASE_URL=…/waveform_test cargo test --lib -- --ignored --test-threads=1` | pass: 9 passed |
| `WAVEFORM_TEST_DATABASE_URL=…/waveform_idem_test cargo test --test postgres_idempotency -- --ignored --test-threads=1` | pass: 8 passed |
| `cargo fmt --check` / `clippy -D warnings` / `test` for `client/` | pass / pass / 5 unit + 13 `api` + 9 `signin` + 4 doctests |
| `cargo fmt --check` / `clippy -D warnings` / `test` for `cli/` | pass / pass / 15 unit + 9 `discovery` + 12 `signin` + 6 `speech` |
| `cargo deny --offline check licenses sources bans` | bans ok, licenses ok, sources ok |
| `python3 scripts/test_package_apps.py` | 11 passed |
| `python3 deploy/native/test_upgrade.py` | 13 passed (8 before, 5 new) |
| `python3 deploy/native/test_accounts_cutover.py` | 12 passed; two mutations (no database restore; retired keys kept) each failed it, then reverted |
| `python3 scripts/sync-cli-docs.py --check` | pass |
| `cd docs-site && npm ci && npm run build && npm run check` | 11 pages built; links, anchors and canonical URLs verified; no IAM/Honeycomb/org words in `dist` except the deliberate `X-Org-ID` note |
| `cd frontend && npm ci && npm test && npm run build` (CI's old-website job, after the manifest removal) | 74/74 passed; built without `dist/honeycomb.yaml`; outputs removed afterwards |
| PyYAML `safe_load` of every workflow; `actionlint 1.7.7` (labels `windows-11-arm`, `macos-15-intel` declared, as they post-date its list) | all parse; actionlint clean |

### Packaging, proved on this Mac

- `cargo build --release --locked --manifest-path cli/Cargo.toml` → Mach-O arm64 `waveform` 0.6.0.
- `PACKAGE_DISCOVERY=require scripts/package-apps.sh 0.6.0 macos-aarch64 target/mig/release/waveform` → discovery
  passed, `dist/apps/waveform-0.6.0-macos-aarch64.tar.gz` (4,502,869 bytes, sha256 `caecb6de…5113b30`) and `.sha256`.
- The archive holds exactly `apps.yaml` (0644) and `bin/waveform` (0755). Extracted into a scratch directory:
  `~/.apps/bin/silicon-apps validate <dir> --json --home <empty>` → `"valid": true`, `"errors": []`;
  `env -i HOME=$E SILICON_HOME=$E PATH=/usr/bin:/bin bin/waveform --help` → exit 0, 14,769 bytes;
  `accounts --json` → exit 0, `"app_id": "waveform"`, `"version": "0.6.0"`, production URLs;
  `login status --json` → exit 0, `{"authenticated": false}`; nothing written to the empty home.
- Packing twice gives the same sha256 (deterministic). Refusals: the Mac binary as `linux-x86_64` ("needs a
  little-endian ELF executable"), version 0.5.1 ("differs from cli/Cargo.toml"), a non-PE file as `windows-x86_64`, and
  a native Mach-O compiled to print `{"app_id":"remind"}` (`waveform accounts --json must contain "app_id":"waveform"`);
  no archive was written in any of them.

### Deployment tooling, proved where it can be off the host

- **Database restore** (the cutover's rollback): on 5460, role `waveform_ship_owner` and database `waveform_ship_rt`,
  `waveform-api migrate` (17 migrations) as that role, one row, `pg_dump -Fc`, then a second row and an extra table,
  then `restore_database(dump, "20261010T091500Z", database="waveform_ship_rt", owner="waveform_ship_owner", …)`.
  Result: the restored database has only the first row, 17 migrations, no extra table, tables and database owned by
  `waveform_ship_owner`; `waveform_ship_rt_cutover_20261010t091500z` holds both rows and the table; `waveform-api
  migrate` on the restored database exits 0. Both databases and the role were dropped afterwards.
- **The rewritten `native.env` boots the new service in production mode:** a production-like IAM-era env (origin/main's
  `.env.example` plus the overrides of origin/main's `deploy/aws/install.py` and `deploy/native/install.py`, 82 keys)
  run through `cutover_env` loses the 17 IAM/Honeycomb-era keys and gains `ACCOUNTS_URL`, `WAVEFORM_APP_SECRET`,
  `WAVEFORM_ACCOUNTS_WEBHOOK_SECRET` and `WAVEFORM_PROOF_ISSUERS` (69 keys). `waveform-api migrate` with the earlier env
  stops at settings ("invalid environment variable WAVEFORM_BRIEFCASE_AUDIENCE: was renamed…"); with the rewritten env it
  passes every production setting check and stops only at "failed to connect to PostgreSQL" (the host `postgres` does
  not resolve off the server).
- **Credential preflight** against the shared stack (`http://127.0.0.1:9589`, Waveform's dev credentials):
  right secret → accepted (`{"active":false}`); wrong secret → "Silicon Accounts refused Waveform's app credentials
  (401)"; unreachable port → "could not be reached".
- **Webhook test helper** against the shared stack: Waveform's webhook (`http://127.0.0.1:9593/waveform/webhooks`,
  `active`) → `POST /v1/apps/waveform/webhook/test` + polling the delivery → `delivered` after 3 s. (Only Waveform's
  own webhook was pinged; nothing was changed.)
- Not runnable here: the host steps (systemd, Docker, SSM, S3) of `accounts_cutover.py`, `upgrade.py`,
  `install.py`, `install-frontend.py` and `deploy/aws/install.py`; they are covered by the mocked transaction tests
  and Python parsing only. `release.yml`'s cross-target builds need GitHub's runners.

### Sweep

`git grep -n -i -E 'iam|honeycomb|org_id|organi[sz]ation|\borg\b|tenant'` outside `docs/history/` and
`docs/migration/` (both deliberate records). Every remaining hit is intentional:

| Where | Why it stays |
| --- | --- |
| `migrations/0001`–`0015` | Applied migrations are never edited. |
| `migrations/0016`, `0017` comments; `src/operator.rs`; `src/bin/waveform_api.rs`; `deploy/native/README.md` line 61 | `link-identities` and its mapping file format (`iam_principal_id,accounts_uuid`), which re-keys IAM-era data at cutover. |
| `src/control/accounts.rs`, `reporting.rs`, `voices.rs`, `src/infrastructure/idempotency.rs`, `src/domain/idempotency.rs`, `tests/postgres_idempotency.rs`, `src/control/tests.rs` (SQL) | The retained legacy `org_id` columns (never dropped; new rows write the sentinel `@`). |
| `src/config.rs`, `src/infrastructure/runtime.rs` | The list of obsolete IAM/Honeycomb variables the service warns about at boot. |
| `src/api/headers.rs`, `src/api/mod.rs`, `src/domain/auth.rs`, `src/infrastructure/briefcase*.rs` | Refusing and redacting IAM-era credential headers. |
| `src/control/mod.rs`, `src/control/tests.rs` | Retired routes answering `410 route_retired`, and their tests; the IAM-era data upgrade test. |
| `cli/src/{args,main,retired,session}.rs`, `cli/tests/discovery.rs`, `client/tests/api.rs` | The hidden `waveform iam` transition alias, retired-flag refusals (`organizations_removed`), legacy-session detection, and tests asserting the help has none of these words. |
| `deploy/native/{upgrade,accounts_cutover,install-frontend}.py` and their tests, `deploy/aws/install.py` | Lists of retired settings the tools remove or refuse, and the IAM-era env the cutover expects. |
| `deploy/aws/standalone.json` | `AWS::IAM::Role` and friends: AWS's own IAM service. |
| `API_DOCS.md` line 39, `docs/api.md` line 24 | Tell old integrations that an `X-Org-ID` header is ignored. |
| `frontend/` (≈130 hits) | The IAM-era SolidJS website. The web stages replace it with `web/` and delete `frontend/`; until then CI keeps building it. |
| `UNDERSTANDING.md` | Carbon-owned; the replacement text is in `understanding-proposal.md`. |
| `decisions.md` (root) | Append-only record; the new entries supersede the IAM/Honeycomb ones. |

The other D9 words (`human`, `AI agent`, `user account`, `team` as a grouping) appear only in `cli/tests/discovery.rs`
(its forbidden-words list), `frontend/`, history, `UNDERSTANDING.md` and the root decisions record.

### Blocked on

- **Briefcase's Accounts-era contract** (unchanged from stage 1): until Briefcase ships it, TTS is held by Waveform
  and STT cannot read sources; `cutover.md` orders Briefcase first.

### Left for later stages

- **Web (stages 7–8):** build `web/` from the kit and ship `web/Dockerfile` meeting the container contract in
  `deploy/native/README.md` (Next.js standalone on `PORT=4325`, `HOSTNAME=0.0.0.0`, unprivileged, writes only `/tmp` and
  `/app/.next/cache`, `GET /auth/session` 200, settings `APP_ID`, `APP_SECRET`, `ACCOUNTS_URL`, `APP_API_URL`,
  `SESSION_SECRET`, `PUBLIC_URL`); point `ci.yml`'s `container`/`web-builds` jobs and `backend-release.yml`'s website job
  at `web/`; delete `frontend/`.
- **E2E (stage 4):** `scripts/dev-accounts.sh` / `scripts/e2e-accounts.sh` replace the retired mock-IAM harness;
  scenario 7 can use `scripts/package-apps.sh`.
- **Cutover:** everything in `cutover.md`, run by a Carbon: sign-in setup, webhook (`"events": null`), secrets, the
  mapping, `accounts_cutover.py`, Interface's switch to proofs, uploads to Silicon Apps and crates.io.

### Gotchas

- Silicon Apps' webhook "recommended" picks leave out `custodian_change`; Waveform needs it. Register with Silicon
  Accounts' `PUT /v1/apps/waveform/webhook` and `"events": null`.
- The previous production binary cannot start on a database migrated past what it knows (`sqlx` `VersionMissing`),
  so any rollback across migrations needs the database restored, not just files.
- Accounts imports match by email/phone and may assign another id than the earlier handle; map principals from the
  import record or `external_id`, not from ids alone.
- `silicon-apps pack` on Windows writes every file 0644; pack Linux/macOS archives on unix runners (the release
  workflow does).
- PyYAML and actionlint are not on this Mac: a scratch venv (`pip install pyyaml==6.0.3`) and the checksum-verified
  actionlint 1.7.7 release binary were used from the scratchpad.
