# Waveform CLI

Build with `cargo build --manifest-path cli/Cargo.toml`; run
`waveform --help` or `waveform <command> --help` for the complete grammar.
The CLI stores IAM session tokens under `<home>/.waveform/dir/session-<scope-digest>.json`
with private directory/file permissions. By default `<home>` is `~`; use
`waveform config home <existing-directory>` to select another home before
logging in. The setting is persisted in the default `~/.waveform/config.json`
and the selected directory is canonicalized. `waveform login <slt>` accepts an
SLT (the hidden `--slt` spelling remains supported) and `waveform refresh`
rotates the saved access/refresh pair; it never prompts for
an IAM password. In an interactive terminal the SLT prompt disables echo so
the token is not shown while typing; when stdin is piped, login reads one line
so noninteractive jobs can provide an SLT without a terminal. `waveform logout`
revokes the access token and removes the local session.

`waveform config home <location>` is local-only and does not contact Waveform.
It rejects missing paths and files with `not a directory`; the selected
directory is created only when a later command needs to write session or test
environment state.

Use `--test <32-character-root-key>` on any command to select a sandbox. A
Waveform test-environment UUID is also accepted; the CLI resolves it through
the authenticated production management API, using `--org <id>` on the
subcommand (or `--organization`/`WAVEFORM_ORG`), then sends only the root key
to sandbox requests. Commands that need actor context use `--org <id>` and
`--actor <id>`.

Speech commands:

- `tts <text> --org <id> --actor <id> [--lang <bcp47>] [--voice-profile <profile-id>]`
- `stt <file-url> --org <id> --actor <id> [--language <bcp47>]`
- `jobs --org <id> --actor <id> [--operation tts|stt] [--limit 1..100] [--cursor <opaque>]`
- `jobs --job-id <uuid> --org <id> --actor <id>`

Control commands include `me`, `capabilities`, `preferences`,
`provider-keys`, `provider-key-set`, and `provider-key-delete`. Preferences
orders are comma-separated provider names; key values are accepted only by
`provider-key-set` and are never printed or returned by the server.

The `test-env` group manages lifecycle from production (`create`, `list`,
`show`, `key`, `rotate`, `delete`, `restore`), reads a selected sandbox with
`test-env current`, and clears it with `test-env clean --test <root-key-or-id> --org <id>`. Create requires
`--org`, `--iam-environment-id`, `--iam-environment-key`, `--app-secret`, and
`--briefcase-environment-key`; upstream test keys are validated and stored
only by Waveform's encrypted backend.

## Automatic CLI maintenance

`waveform config auto-update off` disables the default hourly update check.
Use `on` to enable it again, or `WAVEFORM_AUTO_UPDATE=false` for one invocation.
After a normal command prints its result, the CLI checks the persisted last
attempt time. If due, it queries crates.io for `waveform-cli` and installs a
newer stable release with `cargo install --version =<version> --locked --force`.
The next invocation uses the updated Cargo-installed binary. Installations made
by other package managers are not replaced in their own locations.

A process lock prevents concurrent maintenance, and failed attempts are also
throttled for an hour. Failure is printed on stderr and preserves the requested
command's exit status. Help and config commands do not run maintenance. The
CLI disables the library updater so it cannot change an unrelated project's
lockfile. Publishing and a real newer-release installation have not been tested.

## Session isolation

Sessions are separate for each backend URL and test root key. Logging in with
`--test` does not replace the production login; refresh and logout affect only
the selected session. A UUID selector resolves to the same root-key session.
Root-key rotation requires a new test login. UUID-to-key caches are scoped to
the backend URL as well. `me` derives the organization from the current IAM
authorization and does not require an extra organization argument.

CLI state follows `UNDERSTANDING.md`: `<configured-home>/.waveform/dir`.
The small home-location pointer stays at `~/.waveform/config.json`. Previous
unscoped `session.json` files are deliberately not imported because they do not
identify which server and environment issued the credentials. Log in once in
each selected plane after updating this development build.

## Monitoring speech while it runs

TTS and STT accept `--request-id <uuid>`. If omitted, the CLI generates one and
prints it to stderr before sending speech. The final response stays on stdout.
In another terminal use `jobs --org <org> --actor <actor> --job-id <uuid> --wait`
with the same server and test selection. Waiting tolerates the brief period
before the job row exists and is bounded by `--timeout-ms`. Reuse the original
request UUID and idempotency key when retrying an interrupted operation.

A history failure prevents provider work. Cancellation marks the current attempt
failed when the server can release it; process death is recovered after its
lease expires. Retrying inside idempotency retention keeps one job ID and row.

## Structured status output

`--json` applies to login, refresh, logout, provider-key mutations, test cleanup,
and configuration commands as well as data commands. Status commands return a
`{"message":"…"}` object; secrets are never included in login status output.
Progress and failures remain on stderr.

Voice profiles: `voice-profiles --org <id> --actor <id>` lists the catalog and
mappings. `preferences --org <id> --actor <id> --voice-profile puck` saves an
account default. `tts "Hello" --org <id> --actor <id> --voice-profile sulafat`
overrides it for one generation. These commands support the same `--test` selection.
