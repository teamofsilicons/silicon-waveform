# Configuration

## CLI settings

| Setting | Default | Change |
| --- | --- | --- |
| State home | `SILICON_HOME`, otherwise `HOME` | `waveform config home /existing/directory` |
| API URL | `https://backend.waveform.teamofsilicons.com` | `--url` or `WAVEFORM_URL` |
| Sandbox | Production | `--app-secret-file FILE` or `--test APP_SECRET_OR_UUID` |
| CLI updates | Honeycomb | `honeycomb update` |
| Local telemetry | On | `waveform config telemetry off` or `WAVEFORM_TELEMETRY=0` |
| Account telemetry | On | `waveform telemetry off` or website preferences |

CLI secrets are kept in private files under the selected home's `.waveform/dir`. Production and test sessions are separate. `ISI` is optional context; no command requires it.

## Updates

Install with `honeycomb install 'waveform'`; update with `honeycomb update`.
The CLI does not run an updater. Stop a legacy updater with `waveform daemon stop`
and remove its launchd/systemd registration. Rust clients never update dependencies
at runtime; manage their versions in your application's Cargo manifest and lockfile.

## Service configuration

Every setting is listed in [`.env.example`](../.env.example). Keep secrets (the app secret, the webhook secret,
the encryption and digest keys, provider keys) in an encrypted secret manager. The service checks its settings at
start and refuses to run with a message naming the variable and what is wrong with it.

### Silicon Accounts

| Variable | Default | Meaning |
| --- | --- | --- |
| `ACCOUNTS_URL` | `https://accounts.teamofsilicons.com` | Public Silicon Accounts origin. Access tokens must name it as their issuer. |
| `ACCOUNTS_API_URL` | `ACCOUNTS_URL` | Origin for Waveform's own calls to Silicon Accounts (signing keys, token checks, proofs, account lookups). |
| `WAVEFORM_APP_ID` | `waveform` | Waveform's app id. Access tokens must be issued to it, and proofs must name it as the receiving app. |
| `WAVEFORM_APP_SECRET` | required | Waveform's `sa_app_…` app secret. Never logged or sent to a client. |
| `WAVEFORM_ACCOUNTS_WEBHOOK_SECRET` | required in production | The `whsec_…` secret that signs deliveries to `POST /webhooks/accounts`. |
| `WAVEFORM_ACCOUNTS_WEBHOOK_SECRET_PREVIOUS` | unset | Also accepted while the webhook secret is being rotated. |
| `WAVEFORM_ACCOUNTS_TIMEOUT_SECONDS` | `5` | Timeout of each call to Silicon Accounts (1–60). |
| `WAVEFORM_PROOF_ISSUERS` | empty: every proof is refused | Apps allowed to call Waveform for an account with a User verification proof, per scope, for example `waveform.tts=interface;waveform.stt=interface,dm`. |

Both Accounts URLs are origins without a path. Plain `http` is accepted only for a loopback host
(`localhost`, `*.localhost`, `127.0.0.0/8`, `::1`) outside production; the shared local stack is
`ACCOUNTS_URL=http://localhost:9590` with `ACCOUNTS_API_URL=http://127.0.0.1:9589`.

Waveform's app at Silicon Accounts needs:

- a webhook at `https://backend.waveform.teamofsilicons.com/webhooks/accounts` with every update event (the default);
- the redirect URIs of the Waveform website and of the CLI's sign-in;
- the apps allowed in `WAVEFORM_PROOF_ISSUERS` agreeing to send `waveform.tts` / `waveform.stt` proofs.

### Waveform

| Variable | Default | Meaning |
| --- | --- | --- |
| `WAVEFORM_PUBLIC_URL` | `https://backend.waveform.teamofsilicons.com` in production, `http://127.0.0.1:<port>` elsewhere | Waveform's own origin; held audio is served at `{origin}/api/v1/audio/{request_id}`. |
| `WAVEFORM_ENCRYPTION_KEY` | required in production | 64 lowercase hex characters (`openssl rand -hex 32`). Encrypts saved provider keys and Waveform's Briefcase access. Losing it loses every saved key. |
| `WAVEFORM_IDEMPOTENCY_DIGEST_KEY` | required | Independent secret of at least 32 bytes. Keep it through rotations for the idempotency retention. |
| `WAVEFORM_SPEECH_FIXTURES` | `false` | Development and CI only (refused in production): every speech request returns the prerecorded fixtures instead of calling a paid provider. |

### Silicon Briefcase

| Variable | Default | Meaning |
| --- | --- | --- |
| `WAVEFORM_BRIEFCASE_BASE_URL` | `http://127.0.0.1:8082` | Briefcase API origin. |
| `WAVEFORM_BRIEFCASE_PERMANENT_ORIGIN` | `https://127.0.0.1:8082` | Origin of permanent file URLs (scheme, host and optional port only; HTTPS except for a loopback host). |
| `WAVEFORM_BRIEFCASE_APP_ID` | `briefcase` | Briefcase's app id: the receiving app of the proofs Waveform asks Silicon Accounts for. Setting it to Waveform's own app id is refused. |
| `WAVEFORM_BRIEFCASE_APP_FOLDER` | `apps/waveform` | Folder of each account's Briefcase drive where speech is stored and STT sources are uploaded. |

Speech is stored in the account's own Briefcase files. When Briefcase is unavailable, refuses, or the request came
from another app, the speech still completes and Waveform holds the MP3 for the idempotency retention instead.

### Mail and diagnostics

- `WAVEFORM_POSTMARK_TOKEN`: Postmark server token. Reports fail explicitly if production mail is not configured.
- `WAVEFORM_POSTMARK_URL`: defaults to `https://api.postmarkapp.com/email`; HTTPS only.
- `WAVEFORM_TELEMETRY_KEY`: ordinary Space Station table recording key for Waveform diagnostics.
- `WAVEFORM_TELEMETRY=0`: deployment-wide telemetry opt-out.
- `SPACE_STATION_URL`: optional Space Station override.

Telemetry is enabled by default when its table key is configured. Missing telemetry configuration never prevents a
request. Account opt-outs suppress that account's diagnostics; CLI opt-outs are stored separately on the local
machine. Events contain source, step, progress, outcome and version. Tokens, app secrets, report bodies and speech
text are excluded.

### Operator commands

```sh
waveform-api migrate                                        # apply the database migrations and exit
waveform-api link-identities --file mapping.csv --dry-run   # preview linking earlier accounts' data
waveform-api contracts deprecate v1 --successor v2          # deprecate an API version
```

They read the same environment as the service.

## Bug reports

```sh
waveform report 'Reproduction steps and observed behavior' --pr https://github.com/teamofsilicons/silicon-waveform/pull/123
```

PR links are optional. Reports require a signed-in identity, are limited to ten per actor per hour and are stored before acknowledgment. Keep the printed retry key and reuse `--idempotency KEY` if the result is uncertain. Production delivery is queued through Postmark from `waveform@teamofsilicons.com` to `saketdev12@gmail.com`, `shubhastro2@gmails.com`, and `bugs@teamofsilicons.com`. A queued acknowledgment is not a delivery receipt. Temporary delivery failures retry with backoff. Test reports simulate delivery.

Remove all credentials before submitting reports. Attach a minimal reproduction and a PR when you can; the repository is open for contributions.
