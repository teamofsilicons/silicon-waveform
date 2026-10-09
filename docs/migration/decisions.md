# Waveform migration decisions (Silicon Accounts + Silicon Apps)

This is the record of judgement calls made while moving Waveform from IAM and Honeycomb to Silicon Accounts and
Silicon Apps, for the Carbon to review. The Carbon was asleep and asked for no questions, so every open point was
decided here. The brief's decisions D1–D9 and the app decisions (`apps/waveform.md`) are not repeated unless this
file had to make them concrete. Root `decisions.md` is append-only; its new entry points here.

## Identity

- **Key.** The Accounts `uuid` (short, case-sensitive text such as `zQo`) is the identity key everywhere. It is stored
  in `text` columns and compared exactly; it is never lowercased or cast to `uuid`.
- **Private row key kept.** Every existing table keeps its private `storage_actor_id uuid` (jobs, preferences,
  provider keys, idempotency records, bug reports). The new `waveform_account_keys(account_uuid text PK,
  storage_actor_id uuid UNIQUE)` maps an account to that key, created on first sight. History, idempotency and the
  ciphertext associated data of saved provider keys therefore stay intact.
- **Account cache.** `waveform_accounts` caches, per account uuid: kind, current public id, display name, photo URL,
  the custodian `{uuid, id}` of a Silicon, a profile version, and the revocation marker (`revoked_before`). It is
  filled from token claims, proof verifications, `GET /v1/accounts/{uuid}` lookups (cached; Accounts allows 600 per
  minute per app) and webhooks.
- **Public ids given by callers** (`?account=si:scout`) are resolved to a uuid through the cache and then
  `GET /v1/accounts/by-id/{id}`. An unknown id is refused with `404 account_not_found` and a message naming the id.

## No Teams (old concept → new behaviour)

| Old (IAM era) | New (Accounts era) |
| --- | --- |
| `X-Org-ID` required on every request | Removed. The signed-in account is the scope. An `X-Org-ID` sent by an old client is ignored. |
| Org membership checked online on every request | No membership concept. Any signed-in Carbon or Silicon may use Waveform for its own account. |
| Job history keyed by (plane, org, actor) | Owned by the account that made the request. Reads ignore `org_id`, so every former org's rows for that account appear together. New rows write the org sentinel `@`. |
| Preferences and provider keys per (org, actor) | Per account. New writes use `@`. The operator command consolidates legacy multi-org rows (newest preference wins; newest key per provider wins, re-sealed). |
| Idempotency scope (plane, actor, org, operation, key) | (account, operation, key) with org `@`. Legacy rows expire under the 24 h TTL. |
| Bug-report quota per actor | Per account (10 per hour). |
| Org-owned test environments, Honeycomb lifecycle | Removed (routes, code, headers). Tables and their data are kept untouched. |
| Org roles, tags, trust, reports_to | Nothing in Waveform used them beyond the IAM snapshot; removed. |
| "Visible to the org" | Visible to the requester, and for a Silicon's requests to its custodian (content-free history and settings only). |

**Custodian powers (bounded).** A Carbon who is the custodian of a Silicon can, by selecting that Silicon with
`?account=si:…`: read its job history, read and change its preferences, list/set/delete its provider keys (values
stay write-only), and view or revoke Waveform's Briefcase access for it. A custodian can never run speech as the
Silicon, approve Briefcase access for it (that needs the Silicon's own sign-in), or download its audio. A Silicon can
only select itself. The "circle" helper (`circle(C)` = C and the Silicons it looks after; `circle(S)` = S, its
custodian and its custodian's other Silicons) is implemented in the account directory; Waveform's authorisation uses
the Carbon side of it only, because the app decisions give a Silicon no rights over its custodian's or siblings'
things. `GET /api/v1/silicons` lists the Silicons the caller looks after that Waveform knows about.

**Silicons are not open to the world.** Waveform has no inbound messages, invitations or shares, so nothing from
outside a Silicon's circle can reach it through Waveform. Nothing to add.

## Authentication

- **Bearer.** `Authorization: Bearer <JWT>` verified locally: EdDSA only, `aud` = `WAVEFORM_APP_ID`,
  `iss` = `ACCOUNTS_URL` (trailing slash ignored), `exp`/`nbf` with the client's 30 s leeway. The JWKS is cached and
  refetched when a token names an unknown `kid` (at most once every 30 s) and at least hourly.
- **Revocation markers.** `membership.signed_out` (any reason except `app_revoked`), `membership.access_removed` and
  `account.deleted` set `revoked_before` to the event time. A bearer token whose `iat` is earlier is refused with
  `401 sign_in_ended`. `app_revoked` (one machine's `waveform logout`, or the web's sign-out) does not end the
  account's other sign-ins.
- **Introspection** (cached ≤ 30 s per token digest) on the sensitive routes: provider-key writes and deletes,
  preference changes made by a custodian for a Silicon, Briefcase access approve and revoke.
- **Proofs.** `Authorization: Proof sap_…` is accepted on `POST /api/v1/tts` (scope `waveform.tts`),
  `POST /api/v1/stt` and `POST /api/v1/stt/source-target` (scope `waveform.stt`), and `GET /api/v1/audio/{request_id}`
  (scope `waveform.tts`). Each call is verified with `POST /v1/proofs/verify` using Waveform's own credentials and
  must be `valid`, a User verification, for `receiving_app = waveform`, carry the route's scope, and come from an
  issuer allowed for that scope in `WAVEFORM_PROOF_ISSUERS` (`waveform.tts=interface;waveform.stt=interface,dm`).
  The default is empty, which refuses every proof. Waveform then acts as `user.uuid` and records the issuing app on
  the job (`waveform_jobs.issuing_app`).
- **Removed routes:** `/api/v1/iam`, `/api/v1/auth/login|refresh|logout`, `/api/v1/testing-environment(s)…`,
  `/internal/honeycomb/…`, `/internal/contracts/{v}/deprecate` (now an operator subcommand), `/webhooks/`, `/webhook/`,
  `/api/v1/storage-authorizations/{id}` and `/{id}/complete` (the IAM consent flow). The CLI talks to Accounts
  directly as a public client and the web BFF does the code exchange. The sign-in routes an old client would call
  (`/api/v1/iam`, `/api/v1/auth/login|refresh|logout`, `/api/v1/testing-environment(s)`, `/webhooks/`, `/webhook/`)
  answer `410 route_retired` with a hint naming what replaced them, instead of a bare 404.
- **The authorized actor** carries the account uuid, kind, current id, the credential's scopes and, for bearer
  tokens, the token family (`fid`, one sign-in). Speech requests log the account uuid, the issuing app and the
  token family (never a token).
- **A Silicon's custodian** is learned from `GET /v1/accounts/{uuid}` the first time Waveform sees the Silicon
  (in the background, re-checked every 10 minutes and on `silicon.custodian_changed`); `GET /api/v1/me` waits for
  that first lookup so a Silicon's first answer already names its custodian. A Carbon selecting a Silicon whose
  cached custodian differs is re-checked with Accounts at most once a minute before being refused.
- **Removing Waveform's access** (`membership.access_removed`) ends Waveform's own sign-ins and its Briefcase proof
  for the account. A proof another app presents for that account still works while Accounts verifies it: it
  stands on that app's own grant, and Accounts is the authority on it.
- **Kept or added routes:** `GET /api/v1/auth/me` (and alias `GET /api/v1/me`), public `GET /api/v1/accounts`
  (discovery), `GET /api/v1/silicons`, `POST|GET|DELETE /api/v1/storage-authorizations`,
  `GET /api/v1/audio/{request_id}`, `POST /webhooks/accounts`.

## Briefcase (Waveform → Briefcase)

- Waveform asks Accounts for a **User verification proof** for `receiving_app = briefcase` with the scopes in
  Briefcase's catalogue that Waveform needs: `briefcase.uploads.reserve`, `briefcase.uploads.commit`,
  `briefcase.entries.list`, `briefcase.files.read` (the old OBO endpoint ids, D3). The `subject_token` is the caller's
  current Waveform access token, so only a direct (bearer) request can create one; the speech request itself is the
  account's agreement (app decisions).
- The proof and its rotating refresh token are stored encrypted (XChaCha20-Poly1305 with `WAVEFORM_ENCRYPTION_KEY`,
  context `storage-proof/{storage_actor_id}/{receiving_app}`) in `waveform_storage_proofs`, reused while the token
  has more than 60 s left, refreshed single-flight under a row lock with an `Idempotency-Key` derived from the
  refresh token (the new pair is saved before use), and revoked at Accounts on `membership.signed_out` (not
  `app_revoked`), `membership.access_removed`, `account.deleted`, and `DELETE /api/v1/storage-authorizations`.
- **Delegated calls** (another app calls Waveform with a proof): Waveform holds no Waveform token for that account, so
  it does not create a Briefcase proof. TTS skips Briefcase and answers with `storage.status = "held"`,
  `storage.reason = "delegated_call"` and an `audio_url` the calling app can fetch with its proof. STT must read the
  source from Briefcase, so it uses the account's stored Briefcase proof when there is one (created earlier by the
  account's own use of Waveform, or `waveform storage approve`); without one it answers `403
  storage_authorization_required` with a hint naming that command.
- **Briefcase down or refusing:** TTS still completes. The MP3 is held by Waveform for the idempotency retention
  (`WAVEFORM_IDEMPOTENCY_TTL_SECONDS`, 24 h by default) in `waveform_held_audio`, served at
  `GET /api/v1/audio/{request_id}` to the requesting account only, and the response says why
  (`storage.reason`: `briefcase_unavailable`, `briefcase_timeout`, `briefcase_refused`, `briefcase_contract_unavailable`,
  `storage_not_authorized`, `delegated_call`). `file_url` is `null` in that case. This is an additive change to the v1
  TTS response.
- **Briefcase's Accounts-era wire contract is not published yet** (its migration runs in parallel). The adapter keeps
  the Briefcase 3.0 delegated routes and JSON manifests (`/api/v1/obo/uploads/reserve|commit`,
  `PUT /api/v1/obo/uploads/{id}/content` with the upload capability, `/api/v1/obo/entries/list`,
  `/api/v1/obo/files/read`) and changes only what the brief settles: `Authorization: Proof sap_…` replaces
  `X-App-ID` + `X-IAM-OBO-Access-Token`, and there is no `X-Org-ID`. Assumptions, all isolated in
  `src/infrastructure/briefcase.rs` for the integration stage to reconcile: generated audio lands in the account's
  app folder `apps/waveform` (`WAVEFORM_BRIEFCASE_APP_FOLDER`), `parent_path: ""` selects that folder, and a permanent
  URL is `{origin}/{owner}/{drive path}` (one owner segment) while legacy `{origin}/org/{org}/{path}` URLs still
  resolve. The vendored IAM-era `briefcase-client` 3.0.0 is removed; Waveform switches to Briefcase's Accounts-era
  client crate when it is published.

## Inbound proofs and the Silicon Interface

Interface signed Carbons and Silicons into Waveform with an IAM SLT at `/api/v1/auth/login`. That route is gone.
Interface must switch, at the same release, to User verification proofs for `receiving_app = waveform` with scopes
`waveform.tts` / `waveform.stt` (recorded in `cutover.md`). DM has no Waveform call in its code today; `dm` is in the
suggested `waveform.stt` allow-list for when it adds voice transcripts.

## Webhook

`POST /webhooks/accounts` (register with `PUT /v1/apps/waveform/webhook`). Raw-body verification with
`verify_and_parse_webhook` (5-minute tolerance) against `WAVEFORM_ACCOUNTS_WEBHOOK_SECRET`, then
`WAVEFORM_ACCOUNTS_WEBHOOK_SECRET_PREVIOUS` during a rotation. 401 for a missing/stale/bad signature, 400 for a body
that is not an Accounts event, 503 when no secret is configured. Dedupe on `event_id` in
`waveform_accounts_webhook_events` (text key; the event id is not assumed to be an RFC 4122 UUID). Unknown types answer
200 and are logged. Ordering uses `occurred_at` and, for `account.updated`, `account.version`.

**`account.deleted`:** delete the account's provider keys, Briefcase proofs (revoked at Accounts first), preferences,
job history, idempotency records and held audio; delete its legacy `waveform_actor_keys` rows and its `waveform_identity_links` rows; clear the cached id,
name and photo; keep the uuid ↔ storage-key row as a tombstone so the deleted account's tokens stay refused. Bug
reports already delivered to the bug-report inbox are not Waveform product data; they stay, but their storage key no longer
maps to any id. Briefcase copies are the account's files and Briefcase handles them.

## Linking earlier accounts (`link-identities`)

`waveform-api link-identities --file mapping.csv [--dry-run] [--merge] [--relink]` reads
`iam_principal_id,accounts_uuid` rows (an optional header, blank lines and `#` comments allowed; a principal is the
earlier public id `c:…`/`si:…` or the earlier principal UUID). For each row it points the Accounts uuid at the private
storage key that principal's rows already use (`waveform_account_keys`, source `link_identities`, recorded in
`waveform_identity_links`), so history, idempotency records and saved keys appear under the account unchanged. It then
consolidates the account's settings from every former org into the Accounts-era `@` rows (newest preference wins;
newest key per provider wins and is re-sealed with `WAVEFORM_ENCRYPTION_KEY`; without the key, keys stay on their
legacy rows and are counted as skipped) and marks earlier Briefcase grants invalidated. Nothing is deleted.

Everything runs in one transaction and prints a JSON report (`linked`, `already_linked`, `unmatched`, `conflicts`,
consolidation counts); any conflict rolls the whole run back and exits 2. Re-running the same file is a no-op.
Conflicts it refuses unless told otherwise:

- the account already signed in after the cutover started and has its own, still empty Waveform key: `--merge`
  points it at the earlier data instead (refused when that new key already has data, which needs a manual decision);
- the earlier data is already linked to a different account by an earlier run of this command: `--relink` moves it
  (never a link Waveform made at sign-in).

So a wrong mapping can be corrected before cutover by re-running with a fixed file and `--relink`.

## Configuration

`ACCOUNTS_URL` (public origin and JWT issuer, default `https://accounts.teamofsilicons.com`), `ACCOUNTS_API_URL`
(server-to-server origin, default `ACCOUNTS_URL`), `WAVEFORM_APP_ID` (default `waveform`), `WAVEFORM_APP_SECRET`
(required), `WAVEFORM_ACCOUNTS_WEBHOOK_SECRET` (required in production), `WAVEFORM_ACCOUNTS_WEBHOOK_SECRET_PREVIOUS`,
`WAVEFORM_ACCOUNTS_TIMEOUT_SECONDS` (5), `WAVEFORM_PROOF_ISSUERS`, `WAVEFORM_PUBLIC_URL` (absolute base for
`audio_url`), `WAVEFORM_BRIEFCASE_APP_ID` (now Briefcase's app id, the receiving app; default `briefcase`),
`WAVEFORM_BRIEFCASE_APP_FOLDER` (default `apps/waveform`), `WAVEFORM_SPEECH_FIXTURES` (development and test only).
Plain `http://` is accepted only for loopback hosts (`localhost`, `*.localhost`, `127.0.0.0/8`, `::1`), and never in
production. `WAVEFORM_BRIEFCASE_AUDIENCE` is refused at boot with a rename message, and a
`WAVEFORM_BRIEFCASE_APP_ID` equal to Waveform's own app id (its old meaning) is refused too. Obsolete IAM and
Honeycomb variables are ignored with one warning at boot naming them.

## Test environments

Honeycomb test environments are removed (D5). The deterministic fixture providers (the prerecorded clips) stay
behind `WAVEFORM_SPEECH_FIXTURES=true`, which the service refuses in production. They replace every paid provider for
the whole process, so local and CI end-to-end runs need no provider keys. Saved or request provider keys are accepted
in that mode and never used, so provider-key flows can be exercised end to end too.

## Versions

Waveform 0.6.0 (service, and later the client and CLI). The API path stays `/api/v1` (app decisions); the contract
version becomes `2.0.0` (`X-Waveform-Contract-Version`, `GET /api/contracts`), and the compatibility matrix is
client/CLI `>=0.6.0, <0.7.0`.

## Build

- `silicon-accounts-client = "0.4.0"` from crates.io. `vendor/silicon-iam-client` and `vendor/briefcase-client` are
  deleted. The client and CLI crates (rewritten by the next stage) temporarily take `silicon-iam-client = "=5.0.0"`
  from crates.io instead of the deleted vendor path, so they keep compiling until that stage removes IAM from them.
- `refresh_proof` in `silicon-accounts-client` 0.4.0 cannot send an `Idempotency-Key`, so Waveform calls
  `POST /v1/proofs/refresh` directly with its app credentials and a key derived from the refresh token; every other
  Accounts call goes through the client.
