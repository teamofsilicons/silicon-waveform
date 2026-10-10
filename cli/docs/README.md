# Silicon Waveform

Turn text into speech, or transcribe audio in your Briefcase files, as a Carbon or a Silicon. Start with the CLI; use
the Rust client or the HTTP API when building an integration. Waveform signs you in with
[Silicon Accounts](https://accounts.teamofsilicons.com).

## Install

```sh
silicon-apps install waveform
```

Silicon Apps installs the CLI and keeps it up to date. New to Silicon Apps? Bootstrap it from
https://apps.teamofsilicons.com/install.sh.

## Sign in and speak

A Carbon approves a code in the browser:

```sh
waveform login
waveform tts 'Hello, Carbons and Silicons.'
```

A Silicon hands over a short-lived token instead:

```sh
silicon-accounts login --app waveform -q | waveform login --slt-stdin
waveform login status --json
waveform tts 'Hello, Carbons and Silicons.'
```

The answer contains the MP3's permanent URL in your Briefcase files. Transcribe audio already there with
`waveform stt FILE_URL`. Waveform never needs your password or your Silicon's keys, and everything you do belongs to
your account: history, settings, provider keys and audio. A Carbon can manage the Silicons it looks after with
`--account si:<id>`.

`waveform accounts --json` prints Waveform's app id (`waveform`) and where to sign in, offline.

## Choose your path

- [CLI guide](cli.md): signing in, speech, transcription, history, settings and the Silicons you look after.
- [Sign in with Silicon Accounts](accounts.md): sign-in, proofs from other apps, Briefcase access, account events.
- [API guide](api.md): HTTP authentication, speech requests and storage results.
- [Rust client](client.md): build Waveform into your app.
- [Testing](testing.md): run a development Waveform with deterministic speech fixtures.
- [Configuration](configuration.md): CLI settings, service settings and bug reports.
- [Voice profiles](voice-profiles.md): the available voices and provider mappings.
- [API compatibility](api-contracts.md): negotiation, consumer contracts and sunset policy.

Want a Silicon to do it? Ask: "Install Waveform with `silicon-apps install waveform`, sign in with
`silicon-accounts login --app waveform -q | waveform login --slt-stdin`, and generate speech for this text."

Repository: https://github.com/teamofsilicons/silicon-waveform

Rust package: https://crates.io/crates/silicon-waveform-client
