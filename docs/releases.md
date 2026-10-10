# Release Waveform

Waveform ships four things: the `waveform` CLI (through Silicon Apps, and as `waveform-cli` on crates.io), the Rust
client `silicon-waveform-client` (crates.io), the service `waveform-api` (on the Waveform host) and the website (a
container on the same host). Nothing in this repository publishes or deploys by itself: every upload and deployment
below is a reviewed step a Carbon runs.

## Versions

- The CLI's version (`cli/Cargo.toml`), its `apps.yaml` version and the release tag (`vX.Y.Z`) are always equal; the
  release workflow refuses a tag that differs.
- While Waveform is 0.x, a breaking change raises the minor version (0.5 → 0.6). The client and CLI stay on the same
  minor, because the CLI depends on the client by version.
- The HTTP path stays `/api/v1`; the contract version (`X-Waveform-Contract-Version`, `GET /api/contracts`) records
  breaking changes and the compatibility matrix ([API compatibility](api-contracts.md)).

## Build the CLI packages

1. Update the versions and the bundled guides (`python3 scripts/sync-cli-docs.py`), then check them:
   `python3 scripts/sync-cli-docs.py --check`.
2. Push the tag `vX.Y.Z` (or run the **Build Silicon Apps release** workflow by hand). For each target it builds the
   CLI with the pinned toolchain (static musl on Linux), runs the CLI tests, then packs the target on its own runner
   with `scripts/package-apps.sh`, which runs `waveform --help`, `waveform accounts --json` and
   `waveform login status --json` in an empty home before `silicon-apps validate` and `silicon-apps pack`.
3. Download the workflow artifact `waveform-silicon-apps-release`: one archive per target,
   `waveform-X.Y.Z-<target>.tar.gz`, each with its `.sha256`, and `SHA256SUMS`.
4. Optionally run **Verify Linux CLI on Amazon Linux 2023** with the release run id and source revision: it runs the
   three commands from the Linux archives on the oldest Linux the Silicons use.

Each archive holds `apps.yaml`, listing only its own target, and `bin/waveform` (`bin/waveform.exe` on Windows). The
targets are `linux-x86_64`, `linux-aarch64`, `windows-x86_64`, `windows-aarch64`, `macos-x86_64` and `macos-aarch64`.

To package one build on your own machine:

```sh
cargo build --release --locked --manifest-path cli/Cargo.toml
scripts/package-apps.sh 0.6.0 macos-aarch64 cli/target/release/waveform
```

It needs Python 3.9+ and `silicon-apps` 0.2 (`cargo install --locked silicon-apps-cli --version 0.2.0`), writes
`dist/apps/waveform-0.6.0-macos-aarch64.tar.gz` and its `.sha256`, and refuses a binary that is not native for the
target, a dynamically linked Linux binary, or one that answers the three commands wrongly. A binary this machine
cannot run is packed with a note: the Silicon Apps validation worker for its target runs the commands at upload.

## Publish

Silicon Apps validates uploads on its Linux workers today (`linux-x86_64`, `linux-i686`, `linux-aarch64`,
`linux-armv7hf`). Upload the two Linux archives; keep the macOS and Windows archives for when their workers go live
(a release's packages cannot change afterwards, so those targets join a later release).

```sh
silicon-apps upload waveform --target linux-x86_64 waveform-0.6.0-linux-x86_64.tar.gz
silicon-apps upload waveform --target linux-aarch64 waveform-0.6.0-linux-aarch64.tar.gz
silicon-apps packages waveform
silicon-apps release waveform --version 0.6.0 --package <linux-x86_64 package id> --package <linux-aarch64 package id>
silicon-apps install 'waveform>dev'          # check the development release on a Linux machine
silicon-apps promote waveform <development release id> --version 0.6.0
```

Then publish the Rust crates, the client first (Cargo resolves the CLI's client dependency from crates.io):

```sh
cargo publish --manifest-path client/Cargo.toml
cargo publish --manifest-path cli/Cargo.toml
```

Installed copies update within about a minute of a production release: Silicon Apps' updater checks every minute, and
`waveform` never updates itself. A bad release is withdrawn with
`silicon-apps withdraw waveform <release id> --reason "…"`; installs move to the previous good release.

## Deploy the service and the website

Build the native bundle with the **Build native backend bundle** workflow and deploy it with
[`deploy/native`](../deploy/native/README.md): `upgrade.py` for a release, `install-frontend.py` for the website's
image. The 0.6.0 release moves to Silicon Accounts once, with `accounts_cutover.py`, in the order the
[cutover runbook](migration/cutover.md) gives.

## 0.6.0: Silicon Accounts and Silicon Apps

Breaking for every consumer. The service, client and CLI are 0.6.0; the API path stays `/api/v1` with contract 2.0.0.

- **Sign-in.** Silicon Accounts is the only identity. Carbons sign in to the CLI with `waveform login` (device flow)
  and to the website with the hosted sign-in pages; Silicons hand over a short-lived token
  (`silicon-accounts login --app waveform -q | waveform login --slt-stdin`). The service has no sign-in routes: it
  checks Silicon Accounts access tokens itself. Every machine signs in again once.
- **Your account.** Everything belongs to the account that made it. A Carbon reads and manages the history, settings,
  provider keys and Briefcase access of the Silicons it looks after (`--account si:<id>`, `?account=`), and never runs
  speech as them.
- **Other apps** call Waveform for an account with Silicon Accounts User verification proofs (`waveform.tts`,
  `waveform.stt`), from the apps allowed in `WAVEFORM_PROOF_ISSUERS`.
- **Briefcase.** Waveform reaches each account's Briefcase files with User verification proofs. When Briefcase cannot
  take the audio, TTS still completes and Waveform holds the MP3 for the idempotency retention (`audio_url`,
  `storage.reason`).
- **Account events** arrive signed at `POST /webhooks/accounts`.
- **Distribution.** Silicon Apps installs and updates the CLI (`silicon-apps install waveform`); the CLI has no updater.
- **Rust client.** Sign-in moved to `signin::SignIn`; `Auth` is `Anonymous`, `Bearer` or `Proof`; methods take no
  workspace or actor parameters (`for_account` selects a Silicon you look after); `TtsResponse` adds `audio_url` and
  `storage`. See [the client guide](client.md#upgrading-from-05).
- **CLI.** Flags and commands of the earlier sign-in, test-environment and update model fail with exit 2 and name what
  replaced them; `provider-key-set|delete` became `provider-keys set|delete` and `storage start|complete` became
  `storage approve`.

Releases before 0.6.0: [docs/history/releases-0.2-0.5.md](https://github.com/teamofsilicons/silicon-waveform/blob/main/docs/history/releases-0.2-0.5.md).
