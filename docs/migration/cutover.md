# Waveform cutover to Silicon Accounts

Written by the service stage with what the service needs. The ship stage completes it with the exact production
deploy, website, CLI and Silicon Apps steps. Nothing here has been run against production.

## 1. Before the release

1. **Waveform's app at Silicon Accounts** (`waveform`). Put its app secret in the secret store as
   `WAVEFORM_APP_SECRET`.
2. **Webhook.** As Waveform's owner (or with the app credentials):
   `PUT /v1/apps/waveform/webhook {"url":"https://backend.waveform.teamofsilicons.com/webhooks/accounts"}` with an
   `Idempotency-Key`. Store the `whsec_…` it returns (once) as `WAVEFORM_ACCOUNTS_WEBHOOK_SECRET`. Leave `events`
   unset: Waveform needs every update.
3. **Proof issuers.** Set `WAVEFORM_PROOF_ISSUERS=waveform.tts=interface;waveform.stt=interface` (add `dm` to
   `waveform.stt` when DM starts sending voice notes). Empty refuses every proof.
4. **Silicon Interface must ship at the same time.** Its old path (`POST /api/v1/auth/login` with a short-lived
   token) answers `410 route_retired` from this release. Interface must instead ask Silicon Accounts for a User
   verification proof (`receiving_app` `waveform`, scopes `waveform.tts` and/or `waveform.stt`) and call Waveform
   with `Authorization: Proof sap_…` on `POST /api/v1/tts`, `POST /api/v1/stt`, `POST /api/v1/stt/source-target`
   and `GET /api/v1/audio/{request_id}`. TTS answers called this way carry `storage.status = "held"` and an
   `audio_url` Interface downloads with the same proof.
5. **Briefcase.** Waveform calls Briefcase with User verification proofs (`Authorization: Proof sap_…`, no org
   header) on the routes listed in `decisions.md`. Until Briefcase's Accounts-era release is live, every TTS is held
   by Waveform (`storage.reason` `briefcase_refused` or `briefcase_contract_unavailable`) and STT cannot read
   sources. Release Waveform after Briefcase, or accept held audio for that window.
6. **Environment.** Add the variables in `docs/configuration.md` ("Silicon Accounts"). `WAVEFORM_BRIEFCASE_APP_ID`
   changes meaning: it is now Briefcase's app id (`briefcase`); the old value `waveform` is refused at start.
   Delete `WAVEFORM_BRIEFCASE_AUDIENCE` (refused at start) and these variables, which are ignored with a warning:
   `WAVEFORM_IAM_BASE_URL`, `WAVEFORM_IAM_APP_ID`, `WAVEFORM_IAM_APP_SECRET`, `WAVEFORM_IAM_TOKEN_INTROSPECTION_PATH`,
   `WAVEFORM_IAM_OBO_VERIFY_PATH`, `WAVEFORM_IAM_AUDIENCE`, `WAVEFORM_IAM_TTS_ACTION`, `WAVEFORM_IAM_STT_ACTION`,
   `WAVEFORM_IAM_TIMEOUT_SECONDS`, `WAVEFORM_WEBHOOK_SIGNING_SECRET`, `WAVEFORM_WEBHOOK_KEY_VERSION`,
   `WAVEFORM_WEBHOOK_PUBLIC_URL`, `WAVEFORM_HONEYCOMB_SERVICE_TOKEN`, `SILICON_IAM_CLIENT_AUTO_UPDATE`,
   `WAVEFORM_BRIEFCASE_CDN_ORIGIN`. Keep `WAVEFORM_ENCRYPTION_KEY` and `WAVEFORM_IDEMPOTENCY_DIGEST_KEY` unchanged:
   saved provider keys and idempotency records depend on them.

## 2. Release

1. Take a database snapshot.
2. Deploy. Migrations `0016_accounts_identity` and `0017_api_contract_2` only add tables, columns, an index and the
   contract version; they apply on start (or with `waveform-api migrate`). `0016` lists every earlier principal with
   Waveform data in `waveform_identity_links`, not yet linked.
3. `POST /v1/apps/waveform/webhook/test` and check the delivery succeeded.

## 3. Link earlier data to accounts

1. Get `mapping.csv` with rows `iam_principal_id,accounts_uuid` for the earlier Carbons and Silicons (from the
   import of earlier identities into Silicon Accounts). The principals Waveform still needs are:
   `SELECT iam_principal_id FROM waveform_identity_links WHERE accounts_uuid IS NULL ORDER BY 1;`
2. `waveform-api link-identities --file mapping.csv --dry-run` and read the JSON report: `linked`, `unmatched`
   (principals with no Waveform data), `conflicts` (nothing is applied while any exist).
3. Resolve conflicts (`--merge` for an account that already signed in with no data yet, `--relink` to correct an
   earlier run of this command; see `decisions.md`), then run it without `--dry-run`. It is one transaction and
   re-running it is harmless.
4. Until an account is linked, its first sign-in gets a new, empty Waveform key, which `--merge` later points at the
   earlier data. Link before announcing the release to keep that window short.

## 4. Afterwards

- Saved provider keys are re-sealed into the account rows by the link step; keys of a principal missing from the
  mapping stay on their earlier rows, unused and unchanged.
- Earlier Briefcase grants were made for the earlier sign-in system and are marked invalidated. Each account's
  first TTS after the release creates Waveform's new Briefcase access (or `waveform storage approve`).
- Earlier test-environment tables and webhook-event tables stay untouched; drop them later only by a Carbon's
  decision.
