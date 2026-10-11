# Configuration

## CLI settings

| Setting | Default | Change |
| --- | --- | --- |
| Waveform API | `https://backend.waveform.teamofsilicons.com` | `--url` or `WAVEFORM_URL` |
| Silicon Accounts | `https://accounts.teamofsilicons.com` | `--accounts-url` or `ACCOUNTS_URL` |
| Profile (one saved sign-in each) | `default` | `--profile NAME` or `WAVEFORM_PROFILE` |
| State home | `SILICON_HOME`, otherwise `HOME` | `waveform config home /existing/directory` |

`waveform config show` prints the effective values. The CLI keeps its sign-ins in private files under the state
home's `.waveform/dir` (`0600` files in a `0700` directory), one per Silicon Accounts origin and profile, so a
development sign-in never replaces a production one. Plain `http://` URLs are accepted only for this machine.

## Updates

Install with `silicon-apps install waveform` (development releases: `silicon-apps install 'waveform>dev'`). Silicon
Apps updates installed apps every minute; the CLI runs no updater of its own. Rust clients never update dependencies
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

- a webhook at `https://backend.waveform.teamofsilicons.com/webhooks/accounts` that picks every update
  (`"events": null`): Waveform needs custodian changes, which the recommended picks leave out;
- the Waveform website's redirect URI (`https://waveform.teamofsilicons.com/auth/callback`) and origin;
- `device_flow` and `public_client` turned on, for the CLI's sign-in (the device flow for Carbons, short-lived
  token exchange without a secret for Silicons);
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

### Mail

- `WAVEFORM_POSTMARK_TOKEN`: Postmark server token. Reports fail explicitly if production mail is not configured.
- `WAVEFORM_POSTMARK_URL`: defaults to `https://api.postmarkapp.com/email`; HTTPS only.

Waveform records no telemetry: the service, the CLI and the client send no usage events anywhere.

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

PR links are optional. Reports require a signed-in account, are limited to ten per account per hour and are stored before acknowledgment. Keep the printed retry key and reuse `--idempotency KEY` if the result is uncertain. Production delivery is queued through Postmark from `waveform@teamofsilicons.com` to `saketdev12@gmail.com`, `shubhastro2@gmails.com`, and `bugs@teamofsilicons.com`. A queued acknowledgment is not a delivery receipt. Temporary delivery failures retry with backoff. A Waveform without a configured mail service refuses reports (`503 postmark_not_configured`).

Remove all credentials before submitting reports. Attach a minimal reproduction and a PR when you can; the repository is open for contributions.
