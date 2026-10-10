# Sign in with Silicon Accounts

Waveform's only identity is [Silicon Accounts](https://accounts.teamofsilicons.com). Waveform's app id there is
`waveform`. This guide explains how each kind of caller gets in, what Waveform checks, and how Waveform works with
Briefcase for you.

## Accounts

An account is a Carbon (`c:ada`) or a Silicon (`si:scout`). Each has a permanent `uuid` (short and case-sensitive, for
example `zQo`) and a public id that can change. Waveform keys everything on the `uuid` and only shows the id. Every
Silicon has exactly one custodian, a Carbon.

Everything in Waveform belongs to the account that made it: speech history, preferences, saved provider keys, held
audio and Waveform's Briefcase access. There is no shared workspace.

## Getting in

| Caller | How | Credential Waveform sees |
| --- | --- | --- |
| A Carbon at the terminal | `waveform login` (device flow: approve a code in the browser) | `Authorization: Bearer <access token>` |
| A Silicon | `silicon-accounts login --app waveform -q \| waveform login --slt-stdin` | `Authorization: Bearer <access token>` |
| A Carbon on the website | the hosted sign-in pages (authorization code with PKCE); the website's server keeps the session | `Authorization: Bearer <access token>`, sent by the website's server |
| Another app, for one of its users | a User verification proof for `receiving_app` `waveform` | `Authorization: Proof sap_…` |

The CLI is Waveform's public client: it uses `client_id=waveform` and has no secret. A Silicon's short-lived token
(`slt_…`) works once, lasts 2 minutes and is bound to Waveform. Access tokens last 30 minutes; the CLI refreshes
them one process at a time, because a refresh token works once and presenting a used one ends the whole sign-in.

### What Waveform checks

- **Access tokens** are EdDSA JWTs. Waveform verifies them locally against Silicon Accounts' published keys
  (`/.well-known/jwks.json`, cached, fetched again when a token names an unknown key): the audience must be
  `waveform`, the issuer the Silicon Accounts Waveform trusts (`ACCOUNTS_URL`), and the token current.
- **Ended sign-ins** are refused at once: when Silicon Accounts reports that an account signed out, removed Waveform's
  access or was deleted, tokens issued before that moment stop working (`401 sign_in_ended`). Signing out of one
  machine (`waveform logout`) ends only that sign-in.
- **Sensitive changes** are also checked with Silicon Accounts at the moment they happen (token introspection):
  saving or deleting provider keys, a custodian changing a Silicon's settings, and giving or revoking Briefcase
  access.

## Apps calling Waveform for an account

An app that wants speech for one of its users (a Carbon or a Silicon) asks Silicon Accounts for a **User verification
proof**: `POST /v1/proofs/user-verification` with its own app credentials, `subject_token` = the user's current access
token at that app, `receiving_app` = `waveform`, and the scopes it needs. It obtains the user's agreement in its own
flow first (for a Silicon, its instruction is the agreement). It then calls Waveform with `Authorization: Proof sap_…`.

| Scope | Routes |
| --- | --- |
| `waveform.tts` | `POST /api/v1/tts`, `GET /api/v1/audio/{request_id}` |
| `waveform.stt` | `POST /api/v1/stt`, `POST /api/v1/stt/source-target` |

Waveform verifies every proof with Silicon Accounts (`POST /v1/proofs/verify`, with Waveform's own credentials) and
accepts it only when it is valid, a User verification proof, issued for `waveform`, carries the route's scope, and
comes from an app Waveform allows for that scope (`WAVEFORM_PROOF_ISSUERS`, for example
`waveform.tts=interface;waveform.stt=interface,dm`; nothing is allowed by default). Waveform then acts as the proof's
account and records the app on the job (`issuing_app` in history). Other routes refuse proofs: settings, keys and
Briefcase access are managed by the account itself.

Because Waveform holds no Waveform sign-in for that account, it cannot create Briefcase access from a proof. TTS
therefore answers with the audio held by Waveform (`storage.status: "held"`, `storage.reason: "delegated_call"`, and an
`audio_url` the app downloads with the same proof). STT reads the source with the Briefcase access the account gave
Waveform earlier, or answers `403 storage_authorization_required`.

The CLI passes a proof with `--proof-file FILE` on `tts`, `stt`, `stt-folder` and `audio`. The Rust client uses
`Client::with_proof`.

## Waveform and Briefcase

Waveform stores generated MP3s in the account's own Briefcase files (folder `apps/waveform`) and reads transcription
sources from them. For that, Waveform asks Silicon Accounts for a User verification proof for `receiving_app`
`briefcase` with the scopes `briefcase.uploads.reserve`, `briefcase.uploads.commit`, `briefcase.entries.list` and
`briefcase.files.read`, made from the account's own Waveform sign-in. A speech request made while signed in is the
account's agreement, so the first one gives Waveform this access automatically.

| Action | CLI | API |
| --- | --- | --- |
| Give or replace access | `waveform storage approve` | `POST /api/v1/storage-authorizations` |
| Show it (`active`, `revoked`, `missing`) | `waveform storage status` | `GET /api/v1/storage-authorizations` |
| Revoke it | `waveform storage revoke` | `DELETE /api/v1/storage-authorizations` |

Waveform keeps the proof encrypted, refreshes it one request at a time, and revokes it when the account revokes it,
signs out everywhere, removes Waveform's access, or is deleted. When Briefcase is down or refuses, or no access was
given, speech still completes and Waveform holds the MP3 for 24 hours; `storage.reason` says why.

## The Silicons you look after

A Carbon who is the custodian of a Silicon can select it with `?account=si:…` (CLI: `--account si:…`) to read its
history, read and change its preferences, list, save and delete its provider keys, and see or revoke Waveform's
Briefcase access for it. A custodian never acts as the Silicon: it cannot run speech for it, give Briefcase access for
it, or download its audio. `GET /api/v1/silicons` (CLI: `waveform silicons`) lists the Silicons the Carbon looks after
that Waveform knows. A Silicon can only act for itself.

## Account events

Silicon Accounts sends signed events to `POST /webhooks/accounts`. Waveform verifies the signature over the raw body,
ignores repeats of an event id, and applies:

| Event | What Waveform does |
| --- | --- |
| `account.id_changed` | shows the new id everywhere |
| `account.updated` | updates the cached name and photo |
| `silicon.custodian_changed` | moves custodian powers to the new custodian |
| `membership.signed_out` | one machine signed out (`app_revoked`): nothing else changes; any other reason ends every earlier Waveform sign-in and Waveform's Briefcase access for the account |
| `membership.access_removed` | ends every earlier Waveform sign-in and Waveform's Briefcase access |
| `account.deleted` | deletes the account's history, settings, keys, held audio and Briefcase access |

## Errors

Refused credentials answer `401` (or `403` for a proof that is valid but not enough) with an exact `reason`:

| Reason | Meaning |
| --- | --- |
| `missing_credentials`, `malformed_authorization` | send `Authorization: Bearer <token>` or `Authorization: Proof sap_…`, once |
| `token_expired` | refresh and retry (the CLI does this by itself) |
| `wrong_audience` | the token was issued to another app; sign in to Waveform |
| `wrong_issuer` | the token comes from another Silicon Accounts deployment (check `ACCOUNTS_URL`) |
| `sign_in_ended`, `token_inactive` | the sign-in ended; sign in again |
| `invalid_proof` | Silicon Accounts says the proof is unknown, expired, revoked or for another app |
| `proof_scope_missing`, `proof_issuer_not_allowed`, `proof_not_accepted` | the proof lacks the scope, the app is not allowed for it, or the route needs the account's own token |

A custodian selecting an account it does not look after gets `403 not_custodian`; an unknown id is
`404 account_not_found`.
