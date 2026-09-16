# Build with Waveform

## Local development

```sh
cargo test --locked
cargo test --locked --manifest-path client/Cargo.toml
cargo test --locked --manifest-path cli/Cargo.toml
cd frontend && npm ci && npm run build && npm test
```

Run `python3 scripts/local_database.py`, then source `.local-test/environment` before `cargo test --locked -- --ignored` to run PostgreSQL integration coverage. Use isolated test IAM identities for complete speech/storage tests.

The backend is a Rust library and an API process. The client is a stateless Rust library, and the CLI owns local sessions; Honeycomb owns CLI updates. Database migrations are applied by startup. Never edit `UNDERSTANDING.md`: it is the human-owned requirements source.

## Integration requirements

Use the official IAM and Briefcase clients. Validate the selected sandbox online. Preserve subject, organization, audience, scope and plane checks. Bind OBO proofs to the exact serialized request. Never retain downstream testing secrets in jobs or audit records. Do not send paid provider requests or real report emails from test environments.

The public references are [API](api.md), [Rust client](client.md), [IAM](iam.md), [testing](testing.md), and [OpenAPI](../openapi.yaml). Read usage guides first, then follow these contracts when building automation.

## Documentation

Canonical Markdown lives in `docs/`; `docs-site` renders it to a static site with machine-readable `llms.txt` and `llms-full.txt`. CLI documentation is bundled from matching files under `cli/docs` for offline use. Update both when changing a public operation.

Build and check the published docs and installer snapshot:

```sh
python3 scripts/package_docs_source.py
cd docs-site
npm ci
npm run build
npm run check
```

## Publishing docs

The production docs project is `silicon-waveform-docs` on Vercel, with domain `docs.waveform.teamofsilicons.com`. Build locally with the commands above. Publish only `docs-site/dist` (which includes static hosting headers), not the repository or local state:

```sh
cd docs-site/dist
vercel link --yes --project silicon-waveform-docs
vercel deploy --prod --yes
```

The custom domain uses the existing Namecheap DNS zone. Updating documentation does not require changing other application records. See [verification](verification-2026-09-13.md) for current evidence and deployment limits.
