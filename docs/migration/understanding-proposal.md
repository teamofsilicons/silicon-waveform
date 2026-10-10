# UNDERSTANDING.md: proposed changes for Silicon Accounts and Silicon Apps

`UNDERSTANDING.md` is the Carbon's file and was not edited. These are the passages that no longer match Waveform
after the move from IAM and Honeycomb to Silicon Accounts and Silicon Apps, each with proposed text to paste in its
place. Everything not listed stays as it is (TTS and STT providers and order, requests, voice profiles, jobs, BYOK,
email, versioning, the CLI's grammar and reports, docs, telemetry, configurability).

## Glossary

Replace the three definitions with:

> `Carbon` - a person's account.
> `Silicon` - an agent's account. Every Silicon has exactly one custodian, a Carbon.
> `Account` - a Carbon or a Silicon in Silicon Accounts. It has a permanent uuid and a public id (`c:saket`,
> `si:cos`) that can change.

(The `Org` definition goes: there are no organizations any more.)

## Login

Replace the section with:

> Signing in is handled entirely by Silicon Accounts (https://accounts.teamofsilicons.com). Waveform is the app
> `waveform` there and keeps its app secret on the server only. Carbons sign in to the website through the hosted
> Silicon Accounts pages, and to the CLI with a device code they approve in the browser. Silicons never see a page:
> they run `silicon-accounts login --app waveform -q` and hand the short-lived token to Waveform. Use the official
> silicon-accounts-client at all times (https://crates.io/crates/silicon-accounts-client).
>
> Silicon Accounts posts signed events to backend.waveform.teamofsilicons.com/webhooks/accounts whenever someone
> signs out, removes Waveform's access, changes their id or their Silicon's custodian, or deletes their account.

## How it works

Add after the paragraph about requests being attached to a Silicon or Carbon:

> Everything belongs to the account that made it: its jobs, settings, provider keys and audio. A Carbon can see and
> manage the Waveform history and settings of the Silicons it is the custodian of, but never generates speech as
> them. Other apps can ask Waveform for speech for one of their users with a Silicon Accounts User verification
> proof (scopes `waveform.tts` and `waveform.stt`); Waveform only accepts proofs from the apps it allows.

## Storing TTS voice

Replace the two paragraphs with:

> For each TTS request the generated speech is stored in the requesting account's own Briefcase files, in its
> Waveform folder. Waveform asks Silicon Accounts for a User verification proof for Briefcase (create and read files
> in that folder) and calls Briefcase with it, then sends the link back to the requestee. If Briefcase cannot take
> the file, the speech still completes: Waveform keeps the MP3 for 24 hours, serves it to the same account, and says
> why it is not in Briefcase.

## Testing Environment (the whole section, down to "Webhooks and External Actions")

Replace with:

> # Testing
>
> There are no hosted test environments. To test, run a development Waveform (on a machine or in CI) signed in with
> a development Silicon Accounts. Production refuses this mode.
>
> In that mode, instead of doing TTS or STT, Waveform returns a stored TTS of the text [(Hey, this is the test
> enviorment of silicon waveform, …)] for TTS, and for STT the text [(Hey, this is the test enviorment of silicon
> waveform, if you are reading this, STT worked. …)] for whatever audio file it is. Each voice profile has a
> prerecorded Gemini demo of the same message; the resolved account default or per-request voice profile selects
> the clip. No paid TTS or STT call ever runs there, and no real emails are sent.

(Keep the two quoted texts exactly as they are in the current file.)

## Rust Package & CLI

- "Everyone should be able to use the CLI/Rust Package (carbons, silicons, org, access keys, api keys, …)": drop
  "org".
- Replace "Testing in the test enviorment should also be possible via both cli, and the package." and the paragraph
  "Testing enviorment in cli …" with:

  > The CLI and the package can be pointed at a development Waveform and a development Silicon Accounts
  > (`--url`, `WAVEFORM_URL`, `ACCOUNTS_URL`), so everything can be tested without production.

- Replace "--- logging in via cli ---" and its paragraphs with:

  > For logging in via the cli or the package you never ask for credentials. A Carbon runs `waveform login`, which
  > shows a code to approve on the Silicon Accounts website. A Silicon hands over a short-lived token:
  > `silicon-accounts login --app waveform -q | waveform login --slt-stdin` (`waveform login <slt>` works too).
  > The short-lived token is exchanged for an access and refresh token, kept in `{home_dir}/.{appname}/dir`, which
  > `waveform config home {location}` configures (if it is not a directory, give an error: not a directory).

- In the list of specific endpoints, replace "2) `iam --json` …" with:

  > 2) `accounts --json`: `waveform accounts --json` returns `app_id` alongside other information.

## Cli experience

- "CLI is the primary way to interact with IAM Apps." becomes "CLI is the primary way to interact with our apps."
- "it should just ask for short lived tokens that the user can generate from the official iam cli, or from the web
  where the the user is sent to auth concent screen" becomes "it should just ask for a short-lived token from
  `silicon-accounts login --app <app> -q`, or show a device code a Carbon approves on the Silicon Accounts website".
- Replace the three commands every app CLI must support with:

  > `app --help` explains how to use the app.
  > `app accounts --json` gives {app_id: "...", ...}.
  > `app login status --json` tells if it's {authenticated: true, ...} and which Carbon or Silicon it is.

- "On the docs page, show `honeycomb install 'waveform'` to install the CLI" becomes "On the docs page, show
  `silicon-apps install waveform` to install the CLI".

## Docs and Telemetry

"Since all IAM apps can both be used as is …" becomes "Since all our apps can both be used as is …", and "All IAM
apps use Space Station" becomes "All our apps use Space Station".

## Updates

Replace the section with:

> For each Waveform release, build one .tar.gz per target with apps.yaml at the archive root (app_id `waveform`, the
> release version, command `waveform`, and that target's prebuilt CLI) for Linux, Windows and macOS on x86_64 and
> aarch64. Run `silicon-apps validate` and then `silicon-apps pack`. Refer to the Silicon Apps docs
> (https://developers.teamofsilicons.com/docs/apps) for the package format. Silicon Apps installs and updates the
> CLI; Waveform never updates itself.

## Identifier schema

Replace the last two sentences of the first paragraph ("Silicon IDs and application IDs do not contain an
organisation component. Organisation membership and application ownership are stored separately under `org_id`.")
with:

> Every account also has a permanent uuid, a short case-sensitive string such as `zQo`. Waveform keys everything on
> the uuid and shows the id, because an id can change.
