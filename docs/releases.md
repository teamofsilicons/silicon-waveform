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
