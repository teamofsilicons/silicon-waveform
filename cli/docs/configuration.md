# Configuration

## CLI settings

| Setting | Default | Change |
| --- | --- | --- |
| State home | `SILICON_HOME`, otherwise `HOME` | `waveform config home /existing/directory` |
| API URL | `https://backend.waveform.teamofsilicons.com` | `--url` or `WAVEFORM_URL` |
| Sandbox | Production | `--app-secret-file FILE` or `--test APP_SECRET_OR_UUID` |
| Automatic updates | On | `waveform config auto-update off` or `WAVEFORM_AUTO_UPDATE=0` |
| Local telemetry | On | `waveform config telemetry off` or `WAVEFORM_TELEMETRY=0` |
| Account telemetry | On | `waveform telemetry off` or website preferences |

CLI secrets are kept in private files under the selected home's `.waveform/dir`. Production and test sessions are separate. `ISI` is optional context; no command requires it.

## Unattended updates

```sh
waveform daemon install
waveform daemon status --json
waveform config auto-update off
```

Installation registers a macOS launch agent or Linux user service. The daemon checks crates.io at most once per hour, including after a failed check, and installs newer stable CLI versions with Cargo. It reads opt-out settings independently of command usage. The new binary is used on the next invocation. `daemon start` runs without registering a service; `daemon run` runs in the foreground for another service manager. Linux user services run while the user manager is active; an administrator can enable lingering for operation after logout.

After changing the state home, reinstall/restart the daemon so it reads the new directory. For a registered service, disable updates with `config auto-update off`; `daemon stop` only stops the current process and the service manager may restart it.

The stateless Rust client retains its existing automatic lockfile updater. Disable that behavior with `.with_auto_update(false)` for pinned builds. A library cannot replace code already loaded into its process.

## Backend configuration

The full server settings are in [`.env.example`](../.env.example). Use an encrypted secret manager for application secrets and provider keys.

- `WAVEFORM_POSTMARK_TOKEN`: Postmark server token. Reports fail explicitly if production mail is not configured.
- `WAVEFORM_POSTMARK_URL`: defaults to `https://api.postmarkapp.com/email`; HTTPS only.
- `WAVEFORM_TELEMETRY_KEY`: ordinary Space Station table recording key for Waveform diagnostics.
- `WAVEFORM_TELEMETRY=0`: deployment-wide telemetry opt-out.
- `SPACE_STATION_URL`: optional Space Station backend override.

Telemetry is enabled by default when its table key is configured. Missing telemetry configuration never prevents a request. Account opt-outs suppress that actor's backend diagnostics; CLI opt-outs are stored separately on the local machine. Events contain source, step, progress, outcome and version. Tokens, app secrets, report bodies and speech text are excluded. Test requests never emit production events.

## Bug reports

```sh
waveform report 'Reproduction steps and observed behavior' --pr https://github.com/teamofsilicons/silicon-waveform/pull/123
```

PR links are optional. Reports require a signed-in identity, are limited to ten per actor per hour and are stored before acknowledgment. Keep the printed retry key and reuse `--idempotency KEY` if the result is uncertain. Production delivery is queued through Postmark from `waveform@teamofsilicons.com` to `saketdev12@gmail.com`, `shubhastro2@gmails.com`, and `bugs@teamofsilicons.com`. A queued acknowledgment is not a delivery receipt. Temporary delivery failures retry with backoff. Test reports simulate delivery.

Remove all credentials before submitting reports. Attach a minimal reproduction and a PR when you can; the repository is open for contributions.
