# Build a Honeycomb release

Install with `honeycomb install 'tos>waveform'`, then `waveform login SLT`.
Honeycomb owns CLI installation and updates. The Rust library is a normal Cargo
dependency and never rewrites a project's lockfile at runtime.

Keep the backend, CLI and root `honeycomb.yaml` release versions equal. The release
workflow builds the CLI on Linux, Windows and macOS for x86_64 and aarch64, tests
each native target, and uploads one archive plus checksums as reviewable artifacts.
It does not publish or deploy. Run it with a matching `vVERSION` tag or manually.

For local packaging, supply all six prebuilt binaries under directories named
`linux-x86_64`, `linux-aarch64`, `windows-x86_64`, `windows-aarch64`, `macos-x86_64`
and `macos-aarch64`. Files are `waveform` or `waveform.exe` on Windows.

```sh
python3 scripts/sync-cli-docs.py --check
python3 scripts/package-release.py --binaries-dir release-binaries --honeycomb honeycomb
```

The packager verifies native file formats and CPU architectures, stages only the
manifest and executables, runs `honeycomb validate` followed by `honeycomb pack`,
validates the resulting archive, and checks its members and SHA-256 digest.
`honeycomb.yaml` is at the archive root; platform payloads are below `targets/`.
All six genuine binaries are required; absent targets fail packaging.
