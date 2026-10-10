# Build with Waveform

## Local development

```sh
cargo test --locked
cargo test --locked --manifest-path client/Cargo.toml
cargo test --locked --manifest-path cli/Cargo.toml
```

The service's PostgreSQL tests run with `WAVEFORM_TEST_DATABASE_URL` pointing at an empty database:
`python3 scripts/local_database.py` starts an isolated PostgreSQL and writes `.local-test/environment`; source it, then
run `cargo test --locked --lib -- --ignored --test-threads=1` and
`cargo test --locked --test postgres_idempotency -- --ignored --test-threads=1`. To run Waveform itself against a
development Silicon Accounts with deterministic speech fixtures, follow [testing](testing.md).

The backend is a Rust library and an API process. The client is a stateless Rust library; the CLI owns local sign-ins
and is built only on the client, so it has no feature the client lacks. Silicon Apps installs and updates the CLI.
Database migrations are forward-only and applied at startup (or with `waveform-api migrate`). Never edit
`UNDERSTANDING.md`: it is the Carbon-owned requirements source.

## Integration requirements

- **Identity is Silicon Accounts.** Verify access tokens with `silicon-accounts-client` (EdDSA, `aud` = `waveform`,
  `iss` = `ACCOUNTS_URL`); never accept a token another app received. Key everything on the account `uuid` (short,
  case-sensitive text) and show the current `c:`/`si:` id.
- **Whose data.** Everything belongs to the account that made it. A Carbon may read and manage the settings and history
  of the Silicons it looks after, but nobody runs speech as another account.
- **Proofs, not shared tokens.** Apps acting for an account send User verification proofs for `receiving_app`
  `waveform`; Waveform reaches Briefcase with proofs it asks Silicon Accounts for, from the caller's own access token.
- **No paid providers or real mail in tests.** Use `WAVEFORM_SPEECH_FIXTURES=true` and leave
  `WAVEFORM_POSTMARK_TOKEN` unset. Never put downstream secrets in jobs, logs or audit records.

The public references are the [API](api.md), the [Rust client](client.md), [Sign in with Silicon Accounts](accounts.md),
[testing](testing.md) and the [OpenAPI document](../openapi.yaml). Read the usage guides first, then follow these
contracts when building automation.

## Documentation

Canonical Markdown lives in `docs/`; `docs-site` renders it to a static site with machine-readable `llms.txt` and
`llms-full.txt`. `docs/history/` (records of earlier releases) and `docs/migration/` (the move to Silicon Accounts and
Silicon Apps) are kept in the repository and not published. The CLI bundles copies of its guides under `cli/docs`,
and `cli/README.md` and `client/README.md` are copies of `docs/cli.md` and `docs/client.md`: edit `docs/`, then run
`python3 scripts/sync-cli-docs.py` (CI runs it with `--check`).

Build and check the documentation:

```sh
cd docs-site
npm ci
npm run build
npm run check
```

## Publishing docs

The production docs project is `silicon-waveform-docs` on Vercel, with the domain `docs.waveform.teamofsilicons.com`.
Build locally with the commands above. Publish only `docs-site/dist` (which includes the static hosting headers), not
the repository or local state:

```sh
cd docs-site/dist
vercel link --yes --project silicon-waveform-docs
vercel deploy --prod --yes
```

The custom domain uses the existing Namecheap DNS zone. Updating documentation does not require changing other
application records.
