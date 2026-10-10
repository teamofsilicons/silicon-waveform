# Waveform CLI

`waveform` turns text into speech and transcribes audio, for Carbons and Silicons. Everything you do belongs to the
account you signed in as: your speech history, settings, provider keys and audio. Speech is stored in your own
Briefcase files.

## Install and update

```sh
silicon-apps install waveform            # production
silicon-apps install 'waveform>dev'      # development releases
```

New to Silicon Apps? Bootstrap it from https://apps.teamofsilicons.com/install.sh. Silicon Apps keeps `waveform` up
to date (it checks every minute); the CLI never updates itself.

## Sign in

Waveform signs you in with [Silicon Accounts](https://accounts.teamofsilicons.com). The CLI never asks for a password
and never needs an app secret.

**Carbons** use the device flow:

```sh
waveform login            # prints a page and a code, then waits
waveform login --open     # also opens the page in your browser
```

Open the page in any browser where you are signed in to Silicon Accounts, check that the code matches, and approve.
The CLI finishes by itself. The code lasts 10 minutes. `--label TEXT` names this sign-in on your account's sign-ins
page (default: `waveform CLI on <host>`). With `--json`, progress is one JSON object per line on stdout
(`{"event":"device_code",…}`, `{"event":"slow_down",…}`, then `{"event":"signed_in",…}` or `{"error":…}`).

**Silicons** never see a page. Mint a short-lived token for Waveform and hand it over at once:

```sh
silicon-accounts login --app waveform -q | waveform login --slt-stdin
waveform login --slt slt_…        # or as a flag
waveform login slt_…              # or as an argument
```

`--slt-stdin` keeps the token out of the process list. A short-lived token works once and lasts 2 minutes. When
Silicon Accounts refuses one, the error says exactly why (already used, expired, minted for another app, unknown,
Waveform's access removed, the Silicon's sign-in ended) and how to mint a fresh one.

**Check the sign-in:**

```sh
waveform login status             # readable; exits 1 when signed out
waveform login status --json      # always exits 0
waveform login status --offline   # reads the saved sign-in only, no network
```

Signed out, `--json` prints `{"authenticated":false}` (sometimes with `reason` and `message`, for example
`legacy_session`). Signed in it prints:

```json
{"authenticated":true,"app_id":"waveform","uuid":"zQo","id":"si:scout","kind":"silicon","display_name":"Scout",
 "custodian":{"uuid":"aB1","id":"c:ada"},"expires_at":"2026-10-10T10:30:00Z","refresh_expires_at":"2026-11-09T10:00:00Z",
 "accounts_url":"https://accounts.teamofsilicons.com","api_url":"https://backend.waveform.teamofsilicons.com",
 "profile":"default","method":"slt","verified":true}
```

`uuid` is the account's permanent key; `id` (`c:…` or `si:…`) can change. `verified` is true when Waveform accepted
the sign-in just now. If Waveform cannot be reached, the saved sign-in is reported with `verified: false` and a
`warning`.

**Sign out:** `waveform logout` ends this sign-in at Silicon Accounts and deletes it from this machine. Other machines
and the website stay signed in.

**Refresh** is automatic: an access token lasts 30 minutes and is refreshed when less than 60 seconds remain, one
process at a time under a file lock (a refresh token works once; presenting a used one ends the whole sign-in).
`waveform refresh` refreshes now.

### Where the sign-in is kept

In `$SILICON_HOME/.waveform/dir/` (or `~/.waveform/dir/` when `SILICON_HOME` is not set), one private file per
Silicon Accounts origin: `accounts-session-<digest>.json`, mode `0600` in a `0700` directory, written atomically.
Use `--profile NAME` (or `WAVEFORM_PROFILE`) to keep several accounts on one machine
(`.waveform/dir/profiles/NAME/`). `waveform config home DIR` moves the state to `DIR/.waveform/dir`.

Sign-ins saved by Waveform 0.5 and earlier cannot be used any more: sign in again. `waveform logout` deletes them.

## Discover

```sh
waveform accounts --json
```

```json
{"app_id":"waveform","accounts_url":"https://accounts.teamofsilicons.com",
 "api_url":"https://backend.waveform.teamofsilicons.com","docs_url":"https://docs.waveform.teamofsilicons.com",
 "version":"0.6.0","proof_scopes":["waveform.tts","waveform.stt"],
 "sign_in":{"carbon":"waveform login","silicon":"silicon-accounts login --app waveform -q | waveform login --slt-stdin",
 "short_lived_token":"silicon-accounts login --app waveform -q"}}
```

It is offline and always exits 0. `waveform --help` lists every command; every command accepts `--help`.
`waveform docs [start|cli|api|client|accounts|testing|configuration]` prints the bundled guides.

## Speak

```sh
waveform tts 'Hello, Carbons and Silicons.'
waveform tts --text-file note.txt --voice-profile puck --lang en
waveform tts 'Welcome home.' --provider elevenlabs
```

The answer is JSON on stdout. `file_url` is the MP3's permanent URL in your Briefcase files (`apps/waveform`). When
Waveform cannot store it there (Briefcase down or refusing, no Briefcase access, or the call came from another app),
the speech still completes: `file_url` is null, `storage` is `{"status":"held","reason":…,"expires_at":…}` and
`audio_url` points at Waveform's copy, kept for 24 hours. Download it:

```sh
waveform audio REQUEST_ID --output speech.mp3     # --force replaces a file; --output - writes to a pipe
```

Before speaking, the CLI prints the request id on stderr. Follow it from another terminal with
`waveform jobs --job-id REQUEST_ID --wait`. A failed or interrupted request names the exact retry:
`--idempotency KEY --request-id ID` returns the finished result without running a provider twice.

## Transcribe

```sh
waveform stt-folder                                   # your Waveform folder in Briefcase
waveform stt https://…/apps/waveform/note.mp3 --language en
```

Upload the audio to your Briefcase files (your Waveform folder is the usual place), then pass its permanent URL.
Providers fall back automatically (Gemini, then gpt-transcribe, then Deepgram).

## Briefcase access

Waveform stores speech and reads transcription sources in your Briefcase files with a proof Silicon Accounts issues
for you. Your first speech request made while signed in gives it automatically; an app calling Waveform for you with a
proof can only use access you gave before.

```sh
waveform storage approve     # give it now (signed in as the account itself)
waveform storage status      # active, revoked or missing
waveform storage revoke      # take it back; speech is then held by Waveform
```

If transcription answers `storage_authorization_required`, run `waveform storage approve` and repeat the command with
the retry flags it printed.

## History, settings and keys

```sh
waveform jobs [--operation tts|stt] [--limit 1-100] [--cursor NEXT]   # content-free history, newest first
waveform jobs --job-id ID [--wait] [--timeout-ms 60000] [--poll-ms 500]
waveform preferences                                                  # provider orders, default voice
waveform preferences --voice-profile puck --tts-order elevenlabs,gemini --stt-order deepgram
waveform voice-profiles                                               # voices and their provider mappings
waveform provider-keys                                                # providers with a saved key
waveform provider-keys set gemini --key-file /private/gemini.key      # - reads stdin
waveform provider-keys delete gemini
waveform telemetry off                                                # Waveform's diagnostics for your account
```

Saved provider keys are encrypted and never shown again; Waveform's own keys are used for providers without one.

## The Silicons you look after

A Carbon who is the custodian of a Silicon can read and manage that Silicon's Waveform data:

```sh
waveform silicons                                  # the Silicons you look after that Waveform knows
waveform jobs --account si:scout
waveform preferences --account si:scout --tts-order gemini
waveform provider-keys set openai --key-file - --account si:scout
waveform storage status --account si:scout         # and storage revoke
waveform telemetry off --account si:scout
```

Nobody speaks as another account: `tts`, `stt` and `storage approve` always act as the signed-in account.

## Apps acting for an account

An app that holds a Silicon Accounts User verification proof for an account (issued for `receiving_app` `waveform`)
passes it with `--proof-file FILE` (`-` reads stdin) on `tts`, `stt`, `stt-folder` and `audio`. Scopes:
`waveform.tts` (speech and held audio), `waveform.stt` (transcription and the source folder). No sign-in is needed
or saved, and Waveform must allow the issuing app for that scope.

## Provider controls

TTS runs one provider by default: `--provider gemini|elevenlabs|openai`, or the first of `--provider-order` or your
saved order. `--auto-fallback` tries the others after a failure (not with controls). Controls for the selected
provider go in `--provider-options JSON` or `--provider-options-file FILE`:

- `gemini`: `voice`, `scene`, `audio_profile`, `director_notes`, `sample_context`.
- `elevenlabs`: `voice_id`, `model_id`, `stability`, `similarity_boost`, `style`, `speed`, `use_speaker_boost`, `seed`,
  `previous_text`, `next_text`, `apply_text_normalization` (`auto`, `on`, `off`).
- `openai`: `voice`, `model`, `instructions`, `speed` (`instructions` needs `model: "gpt-4o-mini-tts"`).

```sh
waveform tts 'Welcome home.' --provider gemini \
  --provider-options '{"gemini":{"scene":"A quiet evening at home","director_notes":"Warm, relaxed"}}'
```

A key for one request only: `--provider-key-file PROVIDER=FILE` (repeatable, `PROVIDER=-` reads stdin). It overrides
saved and Waveform keys for that provider and is never stored. Only one input per command may read stdin.

## Output, errors and exit codes

Results go to stdout; notices, progress and warnings go to stderr. With `--json`, stdout holds exactly one JSON
document (a line stream for `login`), and errors are `{"error":{"code","message","hint","details"}}`. Without it,
errors are `error:` and `hint:` lines on stderr. Exit codes: `0` success, `1` failure, `2` invalid arguments or input,
`3` not signed in or refused, `4` not found, `5` conflict, `6` rate limited.

## Configuration

| Setting | Default | Change |
| --- | --- | --- |
| Waveform API | `https://backend.waveform.teamofsilicons.com` | `--url` or `WAVEFORM_URL` |
| Silicon Accounts | `https://accounts.teamofsilicons.com` | `--accounts-url` or `ACCOUNTS_URL` |
| Profile | `default` | `--profile` or `WAVEFORM_PROFILE` |
| State home | `SILICON_HOME`, else `HOME` | `waveform config home DIR` |
| CLI diagnostics | on | `waveform config telemetry off` or `WAVEFORM_TELEMETRY=0` |

Plain `http://` is accepted only for this machine (`localhost`, `127.0.0.1`, `::1`). `waveform config show` prints
the effective settings. See [configuration](configuration.md) and [testing](testing.md).

## Report a bug

```sh
waveform report 'What happened, what you expected, how to reproduce it' --pr https://github.com/teamofsilicons/silicon-waveform/pull/123
```

The pull request is optional; Waveform is open source. Ten reports per account per hour. Keep the printed retry key
and repeat with `--idempotency KEY` if you are unsure the report arrived. Never include tokens or keys.
