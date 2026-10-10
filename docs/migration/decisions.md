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
  minute per app) and webhooks, and every account is looked up again at least once a day (the background account
  check, see [Fix stage](#fix-stage-review-findings)). The cache never outranks Silicon Accounts: a public id
  follows the newest verified evidence, and a cached custodian is trusted only while it is recent.
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
  (`GET /api/v1/me` waits for that first lookup, at most 2 s, so a Silicon's first answer already names its
  custodian) and kept up to date by `silicon.custodian_changed`, by the Silicon's own requests (a background re-check
  once the cached answer is older than 10 minutes) and by the daily account check. A cached custodian stands for a
  Carbon's reads only while Silicon Accounts confirmed it in the last 10 minutes; past that Waveform asks again
  before answering, and every custodian write (preferences, provider keys, Briefcase revoke) asks Silicon Accounts
  first and is refused when it cannot be asked. A Carbon selecting a Silicon whose cached custodian is someone else
  is re-checked at most once a minute before being refused.
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
name and photo; keep the uuid ↔ storage-key row as a tombstone so the deleted account's tokens stay refused. The
account's bug reports are deleted too (superseding the earlier "they stay": a queued report would otherwise still be
mailed after the deletion, and a delivered one is the inbox's copy, not Waveform's). Briefcase copies are the
account's files and Briefcase handles them. Silicon Accounts announces a deletion only to apps the account is a live
member of, so the daily account check finds the others (see [Fix stage](#fix-stage-review-findings)), and a
sign-out or deletion that arrives before the account's first request is kept for it.

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

## Client and CLI (stage 2)

- **Client shape (0.6.0, breaking).** `signin::SignIn` (Waveform's public client: device flow, short-lived token
  exchange, refresh, revoke) and `Client` (`Auth::{Anonymous, Bearer, Proof}`, `for_account` for a custodian). No
  method takes an organization or an actor. `iam()` became `accounts()`; `login`/`refresh`/`logout` moved to
  `SignIn` because the service no longer has sign-in routes. The updater module and test-environment methods are
  deleted. README examples compile as doctests.
- **Two Accounts calls are made directly.** `silicon-accounts-client` 0.4.0 has no secretless short-lived token
  exchange and its public-client revoke accepts only first-party ids, so `SignIn` posts `grant_type=…:slt` with
  `client_id=waveform` and `POST /v1/oauth/revoke` (`token`, `token_type_hint=refresh_token`, `client_id`) itself.
  The device flow and refresh use the client's `app_device_authorize`/`app_device_poll`/`refresh_app_public_client`.
- **Refusal reasons for short-lived tokens** are classified from Silicon Accounts' exact `error_description`
  (checked against its source): `slt_already_used`, `slt_expired`, `slt_wrong_app` (names the app), `slt_unknown`,
  `slt_not_a_short_lived_token` (refused before sending; an old `oac_` token lands here), `slt_access_removed`,
  `slt_sign_in_ended`. Every hint ends with `silicon-accounts login --app waveform -q | waveform login --slt-stdin`.
- **Session file.** `<state dir>/accounts-session-<first 8 bytes of sha256(accounts origin), hex>.json`, one per
  profile and Silicon Accounts origin, so a development sign-in never replaces production. `0600` in a `0700`
  directory, written to a temp file, synced and renamed. A `.lock` file beside it serialises sign-in, refresh and
  sign-out (30 s wait, then a precise `sign_in_locked` error). A command refreshes when fewer than 60 s remain, and
  once more when Waveform calls a token stale (`token_expired`, `sign_in_ended`, `token_inactive`,
  `unknown_signing_key`); a process that waited for the lock uses the pair another process just saved instead of
  refreshing again. A refresh that Silicon Accounts refuses with `invalid_grant` deletes the dead sign-in.
- **Earlier sign-ins** (`session-*.json` from 0.5) are never read: status reports `legacy_session`, commands say
  "sign in again", and `waveform logout` deletes them together with their lock files and the cached test secrets in
  `test-environments-*.json`. A stray `session-*.lock` left by an earlier sign-out is not reported as a sign-in.
- **Signing in again replaces the saved sign-in and revokes the replaced one** at Silicon Accounts (a failure to
  revoke is a warning). Profiles (`--profile`) remain the way to keep several accounts on one machine.
- **`login status`.** `--json` always exits 0, including for an invalid profile or an unreadable file (then
  `{"authenticated":false,"reason":…,"message":…}`). Signed in, it adds `app_id`, `custodian`, `accounts_url`,
  `api_url`, `profile` and `method` (`device`|`slt`) to the required fields. When Waveform or Silicon Accounts
  cannot be reached it reports the saved sign-in with `verified:false` and a `warning` instead of failing; when
  Waveform refuses the token for a reason a refresh cannot cure it reports `authenticated:false` with that reason.
- **Device flow.** The sign-in is labelled `waveform CLI on <$HOSTNAME|$COMPUTERNAME>` (`--label` overrides). Text
  mode prints the page and the code on stderr and the result on stdout; `--json` prints one JSON object per line
  (`device_code`, `slow_down`, `retrying`, then `signed_in` or the error). Network failures while polling are retried
  until the code expires. `WAVEFORM_TEST_POLL_UNIT_MS` scales the poll interval for tests and is honoured only for a
  loopback Silicon Accounts.
- **Command tree.** `provider-key-set|delete` became `provider-keys set|delete` (with `list`, the default);
  `storage start|status|complete` became `storage approve|status|revoke`; new `audio` (held MP3s), `stt-folder`,
  `silicons`, `config show`, `tts --text-file`. `--account si:…` selects a Silicon on `jobs`, `preferences`,
  `provider-keys`, `storage status|revoke` and `telemetry`. `--proof-file` replaces `--obo-app/--obo-token-file`.
- **Retired spellings fail precisely (exit 2)** instead of with clap's generic error: `--org`/`--organization`/
  `--actor` (`organizations_removed`), `--test`/`--app-secret-file`/`test-env` (`test_environments_removed`),
  `--obo-*` (`delegated_tokens_removed`), `daemon`/`config auto-update` (`updater_removed`), the renamed commands
  (`command_renamed`), and `--account` on `tts|stt|stt-folder|audio` (`speech_runs_as_yourself`) or
  `storage approve` (`approval_by_the_account_only`). `WAVEFORM_TEST` in the environment only prints a warning (the
  command still runs, against `--url`); `WAVEFORM_ORG`/`SILICON_ORG` are ignored silently because Silicon runtimes
  set them for other apps.
- **Transition aliases** (one minor release, mentioned only here and in `cutover.md`): hidden `waveform iam` prints
  exactly the `accounts --json` object whether or not `--json` is given; the positional `waveform login <SLT>` stays
  documented as an alias of `--slt`.
- **Exit codes** are the Silicon Accounts CLI's: 0 ok, 1 failure, 2 invalid input, 3 not signed in or refused,
  4 not found, 5 conflict, 6 rate limited. With `--json` every error, including usage errors, is
  `{"error":{"code","message","hint","details"}}` on stdout.
- **Docs.** `waveform docs iam` became `waveform docs accounts` (new `docs/accounts.md`); `docs/iam.md` moved to
  `docs/history/`. `scripts/sync-cli-docs.py` now also keeps `cli/README.md` = `docs/cli.md` and
  `client/README.md` = `docs/client.md`, so the CLI help, the crate README and the docs site cannot drift. The
  docs-site navigation lists `accounts.md` instead of `iam.md`.
- **Service fix found by the live run.** An `?account=` id that Silicon Accounts rejects as malformed
  (`422 invalid_id`, e.g. too long) answered `503 dependency_unavailable`; it now answers `404 account_not_found`
  naming the id (real outages stay 503).

## Ship stage (packaging, CI, deployment, docs)

### Packaging

- **One archive per target**, as the brief's convention says: `dist/apps/waveform-<version>-<target>.tar.gz` with
  `apps.yaml` listing only that target and the binary at `bin/waveform` (`bin/waveform.exe` on Windows), plus a
  `.sha256`. The survey's per-target-set layout (`bin/<target>/waveform`, one archive for several targets) was not
  used: uploads are per target and each archive must validate on its own.
- **`scripts/package-apps.sh` is a thin entry point** that finds Python 3.9+ and runs `scripts/package_apps.py`, so the
  logic (manifest rendering, native-format checks, discovery, archive checks) has unit tests
  (`scripts/test_package_apps.py`, run in CI). The template is `packaging/apps.yaml.in` (comments stripped when
  rendered).
- **What the packager refuses:** a version that is not strict `x.y.z` or differs from `cli/Cargo.toml`; a binary that
  is not a native executable for the target (ELF/Mach-O/PE class and machine checked); a Linux binary with a dynamic
  loader (the fleet runs many distributions, so Linux packages must be the static musl build, as the release workflow
  makes them); and, when the binary can run on the packing machine, wrong answers to `--help`, `accounts --json`
  (`app_id` exactly `waveform`), `login status --json` (`authenticated` false in an empty home) or a `--version` that
  differs from the manifest. "Can run" is decided by trying: another operating system, or `Exec format error`/`Bad
  CPU type`, means it cannot. `PACKAGE_DISCOVERY=require` (the release workflow) turns "cannot run here" into an
  error; `auto` (local default) packs with a note. There is no option to skip the check for a runnable binary.
- **The discovery commands run in a clean environment** (empty `HOME`/`SILICON_HOME`/`USERPROFILE`, minimal `PATH`,
  nothing else inherited), like a validation worker.
- **The packer is hermetic:** `silicon-apps validate`/`pack` run with `--home` set to an empty temporary directory,
  `SILICON_APPS_NO_DAEMON=1`, and without `APPS_TOKEN`/telemetry keys, so packing on a machine that is signed in to
  production Silicon Apps (like the Carbon's) reads no session and starts no updater. After packing, the archive's
  members and bytes are compared with the staged files and the extracted archive is validated again.

### CI

- **`release.yml` packs each target on its own runner** (after building and testing there) instead of packing all
  targets in one Linux job, so every archive's discovery commands really ran on its target, and Linux/macOS archives
  keep their executable mode (`silicon-apps pack` on Windows writes 0644). Each runner installs
  `silicon-apps-cli` 0.2.0 with `cargo install --locked` (cached by `rust-cache`). A final job checks every archive's
  checksum and members and writes `SHA256SUMS`; the artifact is `waveform-silicon-apps-release`. A first job checks the
  tag against `cli/Cargo.toml` before anything builds.
- **Toolchains are the ones the repository already used:** Rust 1.98.0 and static musl for Linux (`musl-gcc`), MSVC
  for Windows, Apple targets on macOS runners. The brief's "zigbuild glibc baseline" example did not apply: Waveform
  ships static musl Linux binaries since 0.3.1.
- **`ci.yml` gains a `silicon-apps-package` job** that builds the static Linux CLI, installs the packer and packs
  `linux-x86_64` with discovery required, so a broken manifest or discovery command fails on every change, and runs
  the packaging and deploy-tool tests. The old website's jobs stay (minus the manifest-copy checks) until the web
  stage replaces `frontend/` with `web/`; `backend-release.yml`'s website job likewise still builds `frontend/`.
- **The Amazon Linux smoke** now downloads `waveform-apps-<target>`, checks the archive's checksum and runs the three
  discovery commands from it in a read-only container.

### Deployment

- **A dedicated one-time tool, `deploy/native/accounts_cutover.py`,** moves the native API, as the IAM 3 cutover had
  its own tool. `upgrade.py` stays the tool for later releases and now refuses an IAM-era `native.env` (pointing to
  the cutover tool) and checks Waveform's app credentials with Silicon Accounts before stopping anything
  (`--skip-accounts-check` for an Accounts outage).
- **The cutover's rollback restores the database.** The previous binary runs `sqlx::migrate!()` with default settings
  and refuses to start on a database that has migrations it does not know (`0015`–`0017`), so restoring files alone
  cannot bring it back. On failure the tool renames the migrated database to `waveform_cutover_<stamp>` (kept, never
  dropped: no data is deleted) and restores the pre-cutover dump into a new `waveform` database owned by `waveform`.
  This was exercised against PostgreSQL with a real migrated Waveform database (progress.md).
- **Migrate and link before the new API starts.** The tool runs `waveform-api migrate` and, given a mapping,
  `waveform-api link-identities` (dry run first, any conflict aborts and rolls back) with the API stopped, through
  `systemd-run` as the service user with the service's environment and private resolver, so no account can sign in
  before its data is linked and `--merge` is only needed for principals linked later.
- **Secrets never pass through arguments.** The new settings come from a root-only `0600` file with an allow-list of
  keys; the runbook writes it on the host from Secrets Manager with a script that holds no secret.
- **The webhook picks every update (`"events": null`).** Silicon Apps' `PUT /v1/apps/{app}/webhook` and
  `silicon-apps webhook APP set` without `--event` pick five recommended updates that leave out `custodian_change`,
  which Waveform needs for its custodian cache; Silicon Accounts' own `PUT` keeps earlier picks when `events` is
  omitted. The runbook therefore generates the secret first (`generate-secret`) and sets the URL with
  `"events": null` during the window, after which the tool asks for a test delivery (exit 3 if it does not arrive;
  the API stays up).
- **The website container** follows the web kit's contract: the Next.js standalone server on port 4325 (so Caddy's
  route stays), `APP_ID`, `ACCOUNTS_URL`, `APP_API_URL`, `PUBLIC_URL` as plain values, and `APP_SECRET` (copied from
  `native.env`'s `WAVEFORM_APP_SECRET`: the website and the API are the same app) and `SESSION_SECRET` (generated
  once, kept) in `/etc/waveform/web.env` through `--env-file`. No session directory is mounted (sealed cookies); the
  earlier one is left on the host. Read-only with tmpfs `/tmp` and `/app/.next/cache`, 256 MiB, ready when
  `/auth/session` answers. The web stage's image must meet this.
- **Retired tooling deleted** (it stays in git history): the IAM 3 cutover (`canonical_upgrade.py`, its test, the IAM
  identity importer and its checks), `scripts/verify-identifier-migration.py`, the mock-IAM e2e harness
  (`scripts/e2e-local-stack.py`, which already read a deleted fixture) and `scripts/e2e-provider-controls-http.py`
  (drove the earlier website's gateway), the Honeycomb packager, manifest sync and install shim. The e2e stage writes
  the Accounts-era scripts.

### Documentation

- **History moves, nothing is rewritten in it.** Dated reports, the IAM and Honeycomb guides, the 0.5 implementation
  record, the earlier release notes and the dated deploy records are in `docs/history/` (with links repointed). The
  docs site publishes neither `docs/history/` nor `docs/migration/`; the CLI bundles neither.
- **`docs/IMPLEMENTATION.md` is not recreated.** The current implementation and verification state is in
  `docs/migration/progress.md` until the release and in the release notes after it; a separate status page went stale
  before.
- **The `X-Org-ID` header is still named** in the API docs, once, to tell old integrations it is ignored. The client's
  upgrade notes say `accounts()` is the discovery call without naming the removed method.
- **The mapping comes from the most exact source first:** the identity import's own record, then Waveform's user base
  (`external_id` of imported Carbons), and only then `by-id` lookups checked by hand, because an import may give an
  account another id than its earlier handle and `by-id` matches current ids only.
- **macOS and Windows archives** are built and checked but not uploaded: Silicon Apps has only Linux validation
  workers today and a release's packages cannot change, so those targets join a later release.

## End-to-end stage (stage 4)

### The local stack and the scripts

- **`scripts/dev-accounts.sh` runs everything with one idempotent command** (Python behind a shell entry point, like
  the packager, because it handles JSON and HTTP). Ports from Waveform's block: the API on 4161, a Briefcase stand-in
  on 4162. Database `waveform_e2e` on `127.0.0.1:5460` (`--fresh-db` drops and recreates it; names that do not start
  with `waveform_` are refused). Encryption keys are generated once into `.mig/dev/` (`0600`) and reused, so stored
  Briefcase proofs survive restarts. The stack description comes from `WAVEFORM_DEV_STACK` or the untracked
  `.mig/dev-accounts.env`, so no machine-specific path is committed. The script refuses a Silicon Accounts that is not
  on this machine.
- **Webhook:** `POST /v1/apps/waveform/webhook/generate-secret`, then `PUT /v1/apps/waveform/webhook` with
  `"events": null` (every update, including custodian changes). The secret is reused while the URL still points at
  the API; a signed ping must arrive, otherwise the script makes a new secret and restarts the API once. `stop` hands
  the webhook back to the stack's fake app (`POST {fake_apps_url}/waveform/_connect-webhook`, which keeps its own new
  secret), so the shared stack is left as it was found (`--keep-webhook` skips that).
- **Sign-in setup:** the script only adds what local runs need, read-modify-write with `expected_version`. On the
  shared stack Waveform offered only Apple and Google, so hosted email sign-in failed with `method_not_enabled`; the
  script enabled `methods.email` (config version 2 → 3). The device flow, public client, callbacks and origins were
  already there. Email stays enabled: Waveform's web stages sign in the same way.
- **A Briefcase stand-in, not Briefcase itself:** this stage owns no other app's ports. The stand-in follows
  Briefcase's Accounts-era branch (as of `1fb52fd`): every proof is verified with Silicon Accounts using Briefcase's
  own development credentials; an app sees only `apps/{app}/`; an empty `parent_path` and a listing without a parent
  mean that folder; the byte transfer takes only the upload capability; permanent URLs are `{origin}/{owner id}/{path}`.
  `WAVEFORM_BRIEFCASE_BASE_URL` points the dev stack at a running Briefcase instead (the integration run).
- **`scripts/e2e-accounts.sh`** uses new identities every run (`waveform-e2e-{c1,c2,s1}-<HHMMSS>`), at most eight email
  codes per address, and waits 45 s once when the stack rate-limits codes. Sharing and "Silicons are not open to the
  world" are recorded as not applicable: Waveform has nothing to share and no inbound action.

### Briefcase proofs (defects the run found, fixed)

- **A refused proof is renewed, once.** A User verification proof stands on the sign-in it was issued from; one
  machine's `waveform logout` or the website's sign-out (`app_revoked`, which correctly does not end the account's other
  sign-ins) ends it at Silicon Accounts, and Briefcase refuses it before its token expires. When Briefcase answers 401
  to a stored proof, Waveform refreshes it under the account's lock: a live proof gets a new token; Silicon Accounts'
  `details.reason` decides the rest:

  | Refresh answer | Waveform |
  | --- | --- |
  | 200 | save the rotated pair, retry with the new token |
  | 410 `revoked_by_account` / `revoked_by_owner` | mark the access revoked (`revoked_at_accounts`); speech is held until the account approves |
  | any other refusal (`sign_in_revoked`, `sign_in_expired`, `refresh_token_reuse`, `access_removed`, expired…) | mark it ended (`proof_ended`) and issue a new one from the caller's own token, then retry |
  | unreachable | keep the refusal (`briefcase_refused`) |

  A request with an app's proof has no Waveform token for the account, so it can only refresh; otherwise it answers
  `storage_authorization_required`. Another request's renewal is reused instead of refreshing twice.
- **Revocations stick.** After `revoked_by_account`, `revoked_by_custodian` or `revoked_at_accounts`, speech is held with
  `storage_not_authorized` and nothing is issued until `POST /api/v1/storage-authorizations` (`waveform storage
  approve`), as the CLI help already promised. Every other end (expiry, a sign-in ended, access removed and given back)
  is replaced on the account's next direct request ("the first speech request gives it").
- **A new issuance gets a new proof.** The issue idempotency key was derived from the subject token alone, so
  approving again with the same token after a revoke got the revoked proof replayed by Silicon Accounts and stored as
  active. The key now also names the proof being replaced (a retry of one issuance still gets the same proof).
- **The STT folder is learnt from its contents.** Briefcase hides `apps/` from an app, so listing it to find
  `apps/waveform` always failed. Waveform now takes the folder id from the `parent_id` of an entry inside it (and still
  reads a Briefcase that lists the container). An empty folder cannot be seen that way: `404 briefcase_folder_not_found`
  with a hint (store one speech there first, or upload with your own Briefcase access). Recorded under "Blocked on".

### Speech routes refuse `?account=`

`/tts`, `/stt`, `/stt/source-target` and `/audio/{request_id}` answered as the caller and ignored `?account=si:…`. They
now answer `400 speech_runs_as_yourself` with a hint, like the CLI: a custodian never speaks as a Silicon or fetches its
audio, and silence could make an integration believe it had.

### `/me` shows the account's name and photo

Access tokens carry no name or photo and app lookups return neither, so `/me` showed `null` until an `account.updated`
arrived. `/me` now reads them once from Silicon Accounts' userinfo with the account's own token (at most every 10
minutes while unknown; a profile version older than the cached one is ignored).

### Checked and kept

- Revocation markers compare a token's whole-second `iat` with the event's millisecond time, so a sign-in made within
  the same second after an end is refused once (signing in again a second later works, as the run shows). This is
  the safe side; kept.
- Selecting a Silicon by `si:` id always asks Silicon Accounts; by uuid it uses the cache, but only for reads and
  only while Silicon Accounts confirmed the custodian in the last 10 minutes. (Superseded: this stage first kept a
  former custodian's access by uuid until a webhook or a re-check that never ran on that path; the fix stage closed
  it, see [Fix stage](#fix-stage-review-findings).)
