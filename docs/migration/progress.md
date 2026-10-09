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
