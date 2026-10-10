# Silicon Waveform Rust client

```sh
cargo add silicon-waveform-client@0.6
```

`silicon-waveform-client` is a stateless, typed client for Waveform: text to speech and speech to text for Carbons
and Silicons. It signs accounts in with [Silicon Accounts](https://accounts.teamofsilicons.com) as Waveform's public
client, and calls the Waveform API with the account's access token or with a proof another app holds. It never asks
for a password, never needs an app secret, and never stores anything: you decide where tokens live.

## Sign in

`signin::SignIn` uses Waveform's app id (`waveform`) as `client_id`, with no secret.

```rust,no_run
use silicon_waveform_client::{DEFAULT_ACCOUNTS_URL, signin::{DeviceProgress, SignIn}};
# async fn carbon() -> Result<(), Box<dyn std::error::Error>> {
// A Carbon: the device flow. Show the code and the page, then wait for approval.
let sign_in = SignIn::new(DEFAULT_ACCOUNTS_URL)?;
let device = sign_in.start_device(Some("my tool")).await?;
println!("Open {} and confirm the code {}", device.browser_url(), device.user_code);
let tokens = sign_in.wait_for_device(&device, |progress| {
    if let DeviceProgress::Retrying { message } = progress { eprintln!("{message}") }
}).await?;
println!("signed in as {}", tokens.account.label());
# Ok(()) }
```

```rust,no_run
# use silicon_waveform_client::{DEFAULT_ACCOUNTS_URL, signin::SignIn};
# async fn silicon(slt: &str) -> Result<(), Box<dyn std::error::Error>> {
// A Silicon: exchange the short-lived token from `silicon-accounts login --app waveform -q`.
let tokens = SignIn::new(DEFAULT_ACCOUNTS_URL)?.exchange_slt(slt).await?;
# Ok(()) }
```

`wait_for_device` honours the poll `interval`, adds 5 seconds after each `slow_down`, keeps polling through passing
failures (reported to the callback) and ends with `SignInError::DeviceDenied` or `DeviceExpired` (codes last 10
minutes). A refused short-lived token is `SignInError::SltRefused` with an exact `SltRefusal` (`AlreadyUsed`,
`Expired`, `WrongApp`, `Unknown`, `NotAShortLivedToken`, `AccessRemoved`, `SignInEnded`); every error has `code()`,
`message()` and `hint()`.

`Tokens` holds the access token (30 minutes), the rotating refresh token, their expiry and the `Account` (`uuid`,
`id`, `kind`, `display_name`, `pfp_url`, a Silicon's `custodian`). Key accounts on `uuid`; show `id`.

- `sign_in.refresh(&refresh_token)` rotates the pair. The old refresh token stops working, and presenting a used one
  ends the whole sign-in (`SignInError::SignInEnded`), so refresh one at a time and store the new pair before using
  it.
- `sign_in.revoke(&refresh_token)` signs out (`POST /v1/oauth/revoke` with `client_id=waveform`).

`SignIn::new` accepts `https://` origins, and `http://` only for this machine.

## Call Waveform

```rust,no_run
use silicon_waveform_client::{Auth, Client, DEFAULT_API_URL, TtsRequest};
# async fn speak(access_token: String) -> Result<(), Box<dyn std::error::Error>> {
let waveform = Client::new(DEFAULT_API_URL, Auth::Anonymous)?.with_bearer(access_token);
let request = TtsRequest { text: "Hello, Carbons and Silicons.".into(), ..Default::default() };
let speech = waveform.tts(&request, "greeting-0001").await?;   // the idempotency key
match speech.file_url {
    Some(url) => println!("stored in Briefcase: {url}"),
    None => {
        // Held by Waveform (Briefcase down or refusing, no access, or a delegated call).
        let audio = waveform.held_audio(&speech.request_id).await?;
        std::fs::write("speech.mp3", &audio.bytes)?;
    }
}
# Ok(()) }
```

| Area | Methods |
| --- | --- |
| Discovery (no credential) | `accounts()` (app id, Silicon Accounts, docs), `capabilities()`, `contracts()` |
| The signed-in account | `me()`, `login_status()` (never fails on a refused token), `silicons()` |
| Speech | `tts`, `stt`, `stt_source_folder` (your Waveform folder in Briefcase), `held_audio` |
| History | `jobs`, `jobs_page(operation, limit, cursor)`, `job`, `wait_for_job` |
| Settings | `preferences`, `update_preferences`, `set_telemetry`, `voice_profiles` |
| Provider keys (write-only) | `provider_keys`, `put_provider_key`, `delete_provider_key` |
| Briefcase access | `approve_storage`, `storage`, `revoke_storage` |
| Reports | `report(message, pr, idempotency_key)` |

`TtsRequest` takes `voice_profile`, `lang`, `provider_order`, `auto_fallback` (false: only the first provider runs),
`provider_options` (typed controls for Gemini, ElevenLabs or OpenAI) and request-only `provider_keys`. `SttRequest`
takes the permanent Briefcase `file_url`, `language`, `provider_order` and `provider_keys`. `ProviderKeys` redacts
its values in `Debug`.

To follow a speech request while it runs, give it a UUID with `client.with_speech_request_id(id)?` and poll
`client.wait_for_job(&id.to_string(), timeout, interval)` concurrently; an initial 404 is retried while the request is
being authorised. Reuse the idempotency key (and the id) to retry: the finished result comes back without running a
provider twice.

## The Silicons a Carbon looks after

`client.for_account("si:scout")?` selects a Silicon the signed-in Carbon is the custodian of for history, settings,
provider keys and Briefcase access status or revocation (`?account=` on the wire). Waveform refuses anyone else
(`403 not_custodian`). Speech, `stt_source_folder` and `approve_storage` refuse a selected account before sending
anything: nobody speaks as another account. `for_self()` clears the selection.

## Apps acting for an account

An app holding a Silicon Accounts User verification proof for `receiving_app` `waveform` uses
`client.with_proof("sap_…")?`. Proofs work on `tts` and `held_audio` (scope `waveform.tts`) and on `stt` and
`stt_source_folder` (scope `waveform.stt`), and Waveform must allow the issuing app for the scope. See
[Sign in with Silicon Accounts](https://docs.waveform.teamofsilicons.com/accounts/).

## Errors

`Error` is `Invalid` (refused before sending), `Transport`, `Api(Box<ApiError>)` or `Decode`. `ApiError` keeps
Waveform's body: `status`, `code`, `message`, `hint`, `reason` (for example `token_expired`, `sign_in_ended`,
`proof_scope_missing`, a provider reason), `request_id`, `provider`, `provider_status` and `retry_after_seconds`.
`error.is_stale_credential()` is true for refusals a fresh access token cures: refresh once and retry.

## Version handshake

Every request sends `X-Waveform-API-Version: v1` and `X-Waveform-Protocol-Version: 1`; `contracts()` reads the
compatibility matrix and deprecation policy (`GET /api/contracts`). Client 0.6 works with Waveform contract 2.

## Upgrading from 0.5

- Sign-in moved to `signin::SignIn` (Silicon Accounts device flow and short-lived tokens). `Client::login`, `refresh`
  and `logout` are gone, and `accounts()` is the discovery call.
- Methods no longer take a workspace or an actor: each acts for the credential's account, or for a Silicon selected
  with `for_account`.
- `Auth` is `Anonymous`, `Bearer` or `Proof`; delegated access tokens are replaced by proofs.
- `LoginStatus` and `Me` carry an `Account` (`uuid`, `id`, `kind`, …).
- `TtsResponse` adds `audio_url` and `storage`; `file_url` is `None` when Waveform holds the audio.
- Test environments and the dependency updater were removed.

Docs: https://docs.waveform.teamofsilicons.com · Source: https://github.com/teamofsilicons/silicon-waveform
