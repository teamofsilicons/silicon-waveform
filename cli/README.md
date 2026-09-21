# Waveform CLI

Install with `honeycomb install 'tos>waveform'`. Commands use
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

Use `--app-secret-file FILE` (or `-` for stdin) to supply Waveform's IAM testing app_secret.
`--test APP_SECRET_OR_UUID` also works; after initial discovery the UUID resolves from
private local state. No production login, IAM environment root key or Briefcase key is required.
Test login accepts an existing active sandbox public ID as well as an IAM test SLT.
The selected environment prints to stderr after every command, including failures.
Commands that need actor context use `--org` and `--actor`.

Speech commands:

- `tts <text> --org <id> --actor <id> [--lang <bcp47>] [--voice-profile <profile-id>]`
- `stt <file-url> --org <id> --actor <id> [--language <bcp47>]`
- `jobs --org <id> --actor <id> [--operation tts|stt] [--limit 1..100] [--cursor <opaque>]`
- `jobs --job-id <uuid> --org <id> --actor <id>`

Control commands include `me`, `capabilities`, `preferences`,
`provider-keys`, `provider-key-set`, and `provider-key-delete`. Preferences
orders are comma-separated provider names. Provider keys can be saved for the
account or supplied for just one speech request; secret values are never printed
or returned by the server.

The historical `test-env` lifecycle commands return `manage_environment_in_honeycomb`. Create, clean, disable, restore and remove environments through Honeycomb. `test-env current` and saved app-secret selectors remain available. Read [testing](testing.md).

## Automatic CLI maintenance

Honeycomb manages native CLI installation and updates. Run `honeycomb update`.
Waveform never independently replaces the CLI. `waveform daemon stop` disables
a legacy updater; remove its OS service registration when migrating. Legacy
`daemon start`, `daemon run`, `daemon install`, and `config auto-update on`
return migration guidance.

`waveform docs TOPIC` opens bundled guides (`start`, `cli`, `api`, `client`, `iam`,
`testing`, `configuration`). `waveform report 'details' --pr https://github.com/teamofsilicons/silicon-waveform/pull/123`
submits an authenticated bug report. The PR is optional; test reports simulate delivery.
`waveform telemetry off` controls account diagnostics and `waveform config telemetry off`
controls local diagnostics. See [configuration](configuration.md).

## Session isolation

Sessions are separate for each backend URL and test app secret. Logging in with
`--test` does not replace the production login; refresh and logout affect only
the selected session. A UUID selector resolves to the same app-secret session.
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
{"authenticated":true,"actor":{"actor_type":"carbon","public_id":"12345678"},"org_id":"tos","testing_environment_id":null}
```

`actor_type` identifies a `carbon` or `silicon`. With no saved session, or a
server rejection (HTTP 401/403), it returns `authenticated: false` and null
identity fields. These status results exit successfully; scripts should inspect
`authenticated`. Network failures, server errors and malformed responses exit
nonzero, with diagnostics on stderr. Authenticated commands, including status,
automatically refresh within 60 seconds of access-token expiry. Saved sessions
from older CLI versions refresh once to acquire an expiry timestamp. Refreshes
serialize across processes, preserve the selected server and testing environment,
and reuse the same retry key after an uncertain response. Tokens are never
printed. Without `--json`, status prints a readable identity or login guidance.

Both commands support `--test <app-secret-or-id>` and the usual server/session
isolation. Resolving an uncached test UUID still requires a production session
and organization; a app secret can be used directly for unauthenticated discovery.

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

## API compatibility

`waveform contracts --json` shows API versions, protocol compatibility and deprecation policy.

## Provider controls and automatic fallback

TTS calls only the first selected provider by default. Use `--provider gemini`,
`--provider elevenlabs`, or `--provider openai` to select it. Omission uses your
saved provider order. `--provider-order` still accepts a comma-separated prefix;
without `--auto-fallback`, only its first provider is attempted. Add
`--auto-fallback` to enable the existing sequence of backup providers. STT keeps
automatic fallback enabled.

Supply `--provider-options '<JSON>'` or `--provider-options-file FILE` (`-` reads
stdin) to control the selected TTS provider. Controls and automatic fallback
cannot be combined. Supported fields are:

- `gemini`: `voice`, `scene`, `audio_profile`, `director_notes`, `sample_context`.
- `elevenlabs`: `voice_id`, `model_id`, `stability`, `similarity_boost`, `style`,
  `speed`, `use_speaker_boost`, `seed`, `previous_text`, `next_text`,
  `apply_text_normalization` (`auto`, `on`, or `off`).
- `openai`: `voice`, `model`, `instructions`, `speed`. `instructions` requires
  `model: "gpt-4o-mini-tts"`; the other supported models are `tts-1` and `tts-1-hd`.

```sh
waveform tts 'Welcome home.' --org bricks --actor ACTOR --provider gemini \
  --provider-options '{"gemini":{"scene":"A quiet evening at home","director_notes":"Warm, relaxed delivery"}}'
waveform tts 'Welcome home.' --org bricks --actor ACTOR --provider elevenlabs \
  --provider-options '{"elevenlabs":{"stability":0.35,"speed":0.9}}'
waveform tts 'Welcome home.' --org bricks --actor ACTOR --auto-fallback
```

Omitted controls use the selected voice profile's defaults. Controls for another
provider, unknown fields, and unsupported values fail validation. If synthesis
fails, the error identifies the provider and a safe reason so you can choose a
different provider explicitly.

## Bring your own provider keys

Use `--provider-key-file PROVIDER=FILE` on TTS or STT for a request-only key.
Repeat the flag for different providers; `PROVIDER=-` reads a key from stdin.
The key overrides a saved personal or deployment key for that provider only and
is never saved by this command. STT fallback and opt-in TTS fallback can still
use the saved or deployment keys belonging to other providers.

```sh
waveform tts 'Hello.' --org bricks --actor ACTOR --provider gemini \
  --provider-key-file gemini=/private/path/gemini.key
waveform stt FILE_URL --org bricks --actor ACTOR \
  --provider-key-file openai=/private/path/openai.key
waveform provider-key-set --org bricks --actor ACTOR gemini \
  --key-file /private/path/gemini.key
```

`provider-key-set --key-file -` reads stdin and saves the key for the authenticated
account and selected environment. The historical positional key argument still
works, but `--key-file` keeps the secret out of shell history and process arguments.
Only one input may read stdin in a command. Key files contain only the key, with
an optional final newline. Provider-key deletion restores normal selection on
later requests.
