# Build a Honeycomb release

Install with `honeycomb install 'tos>waveform'`, then `waveform login SLT`.
Honeycomb owns CLI installation and updates. The Rust library is a normal Cargo
dependency and never rewrites a project's lockfile at runtime.

Keep the CLI and root `honeycomb.yaml` release versions equal. Backend and Rust client versions can advance independently. The release
workflow builds the CLI on Linux, Windows and macOS for x86_64 and aarch64, tests
each native target, and uploads one archive plus checksums as reviewable artifacts.
It does not publish or deploy. Run it with a matching `vVERSION` tag or manually.

For local packaging, supply all six prebuilt binaries under directories named
`linux-x86_64`, `linux-aarch64`, `windows-x86_64`, `windows-aarch64`, `macos-x86_64`
and `macos-aarch64`. Files are `waveform` or `waveform.exe` on Windows.
Include the canonical `honeycomb.yaml` beside each binary; packaging rejects
artifacts with missing or different manifests.

```sh
python3 scripts/sync-cli-docs.py --check
python3 scripts/sync-honeycomb.py --check
python3 scripts/package-release.py --binaries-dir release-binaries --honeycomb honeycomb
```

The packager verifies native file formats and CPU architectures, stages only the
manifest and executables, runs `honeycomb validate` followed by `honeycomb pack`,
validates the resulting archive, and checks its members and SHA-256 digest.
`honeycomb.yaml` is at the archive root; platform payloads are below `targets/`.
All six genuine binaries are required; absent targets fail packaging.

## Manifest in every build

The root `honeycomb.yaml` identifies `tos>waveform` and is the source of truth.
The release workflow includes it beside each native binary, at the root of the
combined Honeycomb archive, and beside the final archive and checksums.

Both API and frontend container images carry it at
`/usr/share/waveform/honeycomb.yaml`. Frontend and documentation builds include
`dist/honeycomb.yaml`; CI uploads both static build artifacts and checks the
manifest inside both container images. These copies identify the application;
the six-platform CLI archive is the installable Honeycomb package.

After changing the root manifest, run `python3 scripts/sync-honeycomb.py` to
refresh the copy in `frontend/public/` used by its isolated Docker build context.
CI rejects stale copies. The docs build copies the root manifest directly.
Builds and artifact uploads do not roll out services or publish an application.

## 0.3.3 — Recover early access rejection

CLI and Honeycomb package 0.3.3 validate saved access before authenticated commands
and renew once if the server rejects it before its advertised expiry. Recovery
uses the durable session lock and persists the replacement credentials. Temporary
service failures retain the login, and user commands run once. Backend 0.3.1,
frontend 0.3.2 and Rust client 0.3.0 are unchanged.

All six native targets and package validation passed.
[Release artifacts](https://github.com/teamofsilicons/silicon-waveform/releases/tag/v0.3.3)
are published through GitHub and Honeycomb; `waveform-cli 0.3.3` is on crates.io.

## 0.3.2 — Durable sessions and refresh recovery

CLI and Honeycomb package 0.3.2 preserve the original request time when recovering
a refresh, so a cached response cannot silently extend an old access token's
lifetime. Backend 0.3.1 keeps sessions during temporary IAM configuration failures.
The browser gateway stores encrypted sessions on persistent disk and resumes
rotation safely after a restart. The first upgrade from the previous memory-only
gateway requires one fresh sign-in. Rust client 0.3.0 is unchanged.

All six native CLI targets passed tests, and the source, backend and frontend
builds passed. [Release artifacts](https://github.com/teamofsilicons/silicon-waveform/releases/tag/v0.3.2)
include the validated Honeycomb package, checksums and exact native backend bundle.

## 0.3.1 — Portable Linux CLI

CLI and Honeycomb package 0.3.1 use static musl binaries for Linux x86_64 and
aarch64. The 0.3.0 GNU Linux artifacts require GLIBC 2.39; upgrade those installs
to 0.3.1. CI rejects a Linux dynamic loader or GLIBC symbol requirement and runs
each binary on its native architecture. Backend and Rust client remain 0.3.0.

## 0.3.0 — Canonical IAM identities and session refresh

Backend, Rust client, CLI and Honeycomb package versions are 0.3.0. The public
HTTP path remains `/api/v1`. Identity responses and CLI login status expose only
`public_id` with the actor type; membership IDs are canonical `actor[org]` strings.
Rust users should replace `LoginActor.principal_id` with `LoginActor.public_id`.
The CLI refreshes near-expiry sessions before authenticated commands and retains
saved sessions across temporary refresh failures.

Publish `silicon-waveform-client` before `waveform-cli`, then build and publish all
six native CLI targets from `v0.3.0`. Upgrade installed 0.2 clients and CLIs before
switching the backend: their old IAM SDK requires the removed UUID identity fields.
Version 0.3 accepts old backend responses too, so clients can upgrade first.
Backend rollout additionally requires the private actor-key import documented in
`deploy/iam-3-cutover.md`; it preserves preferences, jobs and encrypted provider keys.

## 0.2.0 — Provider controls and BYOK

Backend, Rust client, CLI and Honeycomb package versions are 0.2.0. The public
HTTP path remains `/api/v1`; contract discovery includes the 0.2.x clients.
TTS automatic fallback now defaults off, with explicit opt-in for the provider
chain. STT fallback is unchanged. Provider-specific synthesis controls and
request-only keys are available in the API, Rust client, CLI and website; saved
personal keys remain encrypted and write-only. Provider failures include safe
provider and reason metadata.

Rust consumers upgrading to 0.2.0 must account for the new TTS/STT request fields
and the additional `Error::Api` metadata. Prefer `..Default::default()` for TTS
requests and explicitly set `auto_fallback: true` where the old fallback behavior
is required. Existing completed legacy TTS jobs can still be replayed by sending
that flag without new controls or request keys.

Publish `silicon-waveform-client` before `waveform-cli`, because Cargo resolves
the CLI's versioned client dependency from crates.io when packaging. Tag the
release `v0.2.0` to build the six native Honeycomb targets. Confirm the combined
archive, registry versions and public application release after publication;
local package checks and CI artifacts alone do not establish publication.
