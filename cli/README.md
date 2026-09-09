# Waveform CLI

Install with `cargo install waveform-cli --locked`. Commands use
`https://backend.waveform.teamofsilicons.com` by default. Set `WAVEFORM_URL` or
pass `--url <backend>` before the command to select another server; for local
development, use `--url http://127.0.0.1:8080`.

Build with `cargo build --manifest-path cli/Cargo.toml`; run
`waveform --help` or `waveform <command> --help` for the complete grammar.
The CLI stores IAM session tokens under `<home>/.waveform/dir/session-<scope-digest>.json`
with private directory/file permissions. By default `<home>` is `SILICON_HOME`
when that environment variable is present, otherwise `~`; use
`waveform config home <existing-directory>` to select another home before
logging in. The explicit setting takes precedence over that default and is
persisted in `<default-home>/.waveform/config.json` (under `SILICON_HOME` when set).
The selected directory is canonicalized. `waveform login <slt>` accepts an
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
The small home-location pointer stays at `<default-home>/.waveform/config.json`. Previous
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
`{"message":"…"}` object for mutation acknowledgements; secrets are never included in login status output.
Progress and failures remain on stderr.

Voice profiles: `voice-profiles --org <id> --actor <id>` lists the catalog and
mappings. `preferences --org <id> --actor <id> --voice-profile puck` saves an
account default. `tts "Hello" --org <id> --actor <id> --voice-profile sulafat`
overrides it for one generation. These commands support the same `--test` selection.

## IAM discovery and login status

`waveform --help` includes this guide and the command list; `-h` prints the short
command list. Use `waveform <command> --help` for a command's options and required
arguments, including `waveform login status --help`.

`waveform iam --json` reads public metadata from the selected Waveform backend
without a saved login. Use the returned `app_id` when obtaining an IAM SLT:

```json
{"app_id":"tos>waveform","iam_base_url":"https://backend.iam.teamofsilicons.com/","testing_environment_id":null}
```

The IAM URL comes from server configuration. `testing_environment_id` is the
upstream IAM environment UUID for a selected sandbox, or null for production.
No app secret or test key is returned. Use `waveform --url <backend> iam --json`
to discover another server.

`waveform login status --json` verifies the saved access token through
`GET /api/v1/auth/me`, then returns:

```json
{"authenticated":true,"actor":{"principal_id":"00000000-0000-0000-0000-000000000001","actor_type":"carbon","public_id":"12345678"},"org_id":"tos","testing_environment_id":null}
```

`actor_type` identifies a `carbon` or `silicon`. With no saved session, or a
server rejection (HTTP 401/403), it returns `authenticated: false` and null
identity fields. These status results exit successfully; scripts should inspect
`authenticated`. Network failures, server errors and malformed responses exit
nonzero, with diagnostics on stderr. Status never prints, refreshes or changes
saved tokens. Without `--json`, it prints a readable identity or login guidance.

Both commands support `--test <root-key-or-id>` and the usual server/session
isolation. Resolving an uncached test UUID still requires a production session
and organization; a root key can be used directly for unauthenticated discovery.

## Selecting the default home with SILICON_HOME

Sessions, cached test keys, and updater settings/timestamps all use the resolved
state directory. Changing `SILICON_HOME` selects a separate configuration and
does not migrate existing state. An explicit `config home` selection overrides
the default within that configuration. For example:

```sh
export SILICON_HOME=/existing/agent-home
waveform config auto-update off
waveform login <slt>
waveform login status --json
```
