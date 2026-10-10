# Waveform cutover to Silicon Accounts and Silicon Apps

The production runbook for Waveform 0.6.0: the service, the website and the CLI move from IAM and Honeycomb to
Silicon Accounts and Silicon Apps. Nothing here has been run against production. Every command that touches
production is marked **run at cutover** and is run by a Carbon (or with a Carbon's go-ahead), in this order.

Throughout: `ACCOUNTS=https://accounts.teamofsilicons.com`, `API=https://backend.waveform.teamofsilicons.com`. Secrets
are read into the shell without echo (`read -rs WAVEFORM_APP_SECRET`) or read on the host from Secrets Manager; never
put one on a command line, in SSM command text, or in this repository.

## 1. Order relative to the other apps

| Step | Who | Why this order |
| --- | --- | --- |
| 1. Silicon Accounts production has the earlier Carbons and Silicons imported, and the apps `waveform`, `briefcase` and `interface` exist | Accounts | Waveform's mapping and every proof need them. `waveform` already exists in production. |
| 2. Waveform's sign-in setup and webhook secret (section 2) | Waveform's owner | Before any Accounts-era client can sign in. |
| 3. Briefcase's Accounts-era release | Briefcase | Waveform stores and reads speech through Briefcase with User verification proofs. Released before it, every TTS is held by Waveform (`storage.reason` `briefcase_refused` or `briefcase_contract_unavailable`) and STT cannot read sources. |
| 4. Waveform service (section 3) and the Silicon Interface's switch to proofs, in the same window | Waveform, Interface | Interface's old path (`POST /api/v1/auth/login` with an IAM short-lived token) answers `410 route_retired` from this release. |
| 5. Waveform website (section 3, step 5) | Waveform | It signs Carbons in with Silicon Accounts and needs the migrated API. |
| 6. CLI 0.6.0 on Silicon Apps and crates.io (section 3, step 7) | Waveform's owner | Needs the migrated API (`/api/v1/me`, `/api/v1/accounts`). |
| 7. Silicons sign in again; the fleet runtime uses the new commands (section 6) | Stemcell, every Silicon | The 0.5 CLI cannot sign in any more. |

Waveform does not deliver through Ting and needs no Ting change. DM has no Waveform call today; when it adds voice
transcripts, add `dm` to `waveform.stt` in `WAVEFORM_PROOF_ISSUERS`.

## 2. Prepare (before the window)

### 2.1 Waveform at Silicon Accounts (run at cutover, or the day before)

As Waveform's owner, with the app secret in `WAVEFORM_APP_SECRET`. Read the sign-in setup and its version:

```sh
curl -fsS -u "waveform:$WAVEFORM_APP_SECRET" "$ACCOUNTS/v1/apps/waveform" \
  | jq '{config_version, redirect_uris: .signin_config.redirect_uris, allowed_origins: .signin_config.allowed_origins}'
```

Then write it back with the website's callback and origin added (arrays replace, so send every value you read), the
device flow on (Carbons: `waveform login`) and the public client on (Silicons: `waveform login --slt-stdin`, exchanged
with `client_id=waveform` and no secret):

```sh
curl -fsS -X PATCH -u "waveform:$WAVEFORM_APP_SECRET" "$ACCOUNTS/v1/apps/waveform/signin-config" \
  -H 'Content-Type: application/json' -H 'Idempotency-Key: waveform-cutover-signin-1' \
  -d '{"expected_version": VERSION_READ,
       "redirect_uris": ["…every uri read…", "https://waveform.teamofsilicons.com/auth/callback"],
       "allowed_origins": ["…every origin read…", "https://waveform.teamofsilicons.com"],
       "device_flow": true, "public_client": true}'
```

The website asks for no details beyond the profile; leave `required_fields` as they are.

Generate the webhook's signing secret now and set the URL only in the window (section 3), so Silicon Accounts does
not deliver to the old service meanwhile:

```sh
curl -fsS -X POST -u "waveform:$WAVEFORM_APP_SECRET" "$ACCOUNTS/v1/apps/waveform/webhook/generate-secret" \
  -H 'Idempotency-Key: waveform-cutover-webhook-secret-1' | jq -r .secret
```

It prints the `whsec_…` secret once. If Waveform already has a webhook secret you hold, keep it instead (a later
`PUT` of the URL keeps the stored secret).

### 2.2 Secrets

Add to the Secrets Manager record `silicon-waveform/production` (a private JSON file and `--secret-string file://…`,
never shell arguments):

| Key | Value |
| --- | --- |
| `WAVEFORM_APP_SECRET` | Waveform's `sa_app_…` app secret |
| `WAVEFORM_ACCOUNTS_WEBHOOK_SECRET` | the `whsec_…` secret from 2.1 |
| `WAVEFORM_PROOF_ISSUERS` | `waveform.tts=interface;waveform.stt=interface` (empty refuses every proof, so Interface could not call Waveform) |

Keep `WAVEFORM_ENCRYPTION_KEY` and `WAVEFORM_IDEMPOTENCY_DIGEST_KEY` exactly as they are: saved provider keys,
Briefcase access and idempotency records depend on them. Keep the IAM-era keys in the record until the cutover is
accepted (rollback needs them), then delete them (section 6).

### 2.3 Artifacts

- **Service:** run **Build native backend bundle** at the release commit; keep the archive and its SHA-256.
- **Website:** build `web/` for `linux/arm64`, push it to `silicon-waveform-production` in ECR with an immutable tag,
  and note the digest.
- **CLI:** push the tag `v0.6.0`; **Build Silicon Apps release** produces `waveform-silicon-apps-release` (one archive
  per target with `SHA256SUMS`). Optionally run **Verify Linux CLI on Amazon Linux 2023** on that run.

### 2.4 The mapping of earlier accounts

`waveform-api link-identities` points each Silicon Accounts uuid at the private key its earlier rows use, so history,
settings and saved provider keys appear under the account unchanged. It reads rows `iam_principal_id,accounts_uuid`.

1. List the earlier principals Waveform has data for (read-only, on the host; **run at cutover** or the day before):

   ```sh
   docker exec waveform-postgres psql -U postgres -d waveform -tAc \
     "SELECT DISTINCT public_id FROM waveform_actor_keys WHERE plane_id = '00000000-0000-0000-0000-000000000000' ORDER BY 1" \
     > /root/waveform-principals.txt
   ```

2. Map each earlier principal to its Silicon Accounts uuid, from the most exact source available:

   - **The identity import's own record** of which earlier principal became which account (the run that brought
     the earlier Carbons and Silicons into Silicon Accounts). Use it as is.
   - **Waveform's user base**, for Carbons imported into Waveform with `external_id` set to their earlier id
     (`POST /v1/apps/waveform/imports`; each row reports `account_uuid`). The user base keeps that link:

     ```sh
     curl -fsS -u "waveform:$WAVEFORM_APP_SECRET" "$ACCOUNTS/v1/apps/waveform/users?source=import&limit=100" \
       | jq -r '.items[] | select(.external_id != null) | "\(.external_id),\(.uuid)"'   # follow next_cursor
     ```

   - **Only as a last resort, the current id** (`by-id` matches current ids only, and an import may have given an
     account another id than its earlier handle). Check each answer against what you know of the account (its kind,
     its custodian for a Silicon) before using it:

     ```sh
     while read -r id; do
       curl -fsS -u "waveform:$WAVEFORM_APP_SECRET" "$ACCOUNTS/v1/accounts/by-id/$id" \
         | jq -r --arg id "$id" '"\($id),\(.uuid),\(.kind),\(.custodian.id // "")"'
       sleep 0.2      # 600 lookups per minute per app
     done < /root/waveform-principals.txt > /root/waveform-lookup-review.csv
     ```

   Write the result as `/root/waveform-identity-map.csv` with the header `iam_principal_id,accounts_uuid`. Leave out
   any principal you cannot map with confidence: its data stays untouched, and a later run with `--merge` links it
   after that account has signed in.

3. Rehearse on a copy (recommended): restore the newest backup into a scratch PostgreSQL, then run the new
   release's `waveform-api migrate` and `waveform-api link-identities --file waveform-identity-map.csv --dry-run`
   against it. The JSON report counts `linked`, `already_linked`, `unmatched` (principals with no Waveform data) and
   lists `conflicts`; the real run applies nothing while any conflict exists.

The file holds only public ids and uuids; still keep it root-only (`chmod 600`).

### 2.5 The accounts env file on the host (run at cutover)

The cutover tool reads the new settings from a root-only file, never from arguments. Write it on the host from the
secret (this script holds no secret, so it is safe in SSM):

```sh
python3 - <<'PY'
import json, os, subprocess
secret = json.loads(subprocess.check_output(["aws", "secretsmanager", "get-secret-value", "--region", "us-east-1",
    "--secret-id", "silicon-waveform/production", "--query", "SecretString", "--output", "text"]))
fd = os.open("/root/waveform-accounts.env", os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
with os.fdopen(fd, "w") as out:
    for key in ("WAVEFORM_APP_SECRET", "WAVEFORM_ACCOUNTS_WEBHOOK_SECRET", "WAVEFORM_PROOF_ISSUERS"):
        if secret.get(key):
            out.write(f"{key}={secret[key]}\n")
print("wrote /root/waveform-accounts.env")
PY
```

## 3. The window (run at cutover)

1. **Upload** the backend archive, `deploy/native/accounts_cutover.py`, `upgrade.py` and `install-frontend.py` to the
   host as for earlier releases (S3 and SSM; `deploy/aws/ssm.py`).

2. **Set the webhook URL**, picking every update (`"events": null`: Waveform needs `custodian_change`, which the
   recommended picks of `silicon-apps webhook waveform set` leave out). The stored secret is kept:

   ```sh
   curl -fsS -X PUT -u "waveform:$WAVEFORM_APP_SECRET" "$ACCOUNTS/v1/apps/waveform/webhook" \
     -H 'Content-Type: application/json' -H 'Idempotency-Key: waveform-cutover-webhook-1' \
     -d "{\"url\": \"$API/webhooks/accounts\", \"events\": null}"
   curl -fsS -u "waveform:$WAVEFORM_APP_SECRET" "$ACCOUNTS/v1/apps/waveform/webhook"   # events: null, secret_set: true
   ```

   Deliveries made before the new API answers are retried by Silicon Accounts; replay any that failed afterwards with
   `POST /v1/apps/waveform/webhook/replay {"status": "failed"}`.

3. **Move the API** on the host, as root:

   ```sh
   python3 accounts_cutover.py \
     --archive /root/waveform-backend-REVISION-linux-aarch64.tar.gz --sha256 ARCHIVE_SHA256 \
     --source-revision FULL_40_CHARACTER_REVISION \
     --backup-bucket silicon-waveform-standalone-backups-c9ap0zqahvyr \
     --accounts-env /root/waveform-accounts.env \
     --mapping /root/waveform-identity-map.csv
   ```

   It checks everything first (the bundle, the accounts env file, that `native.env` is still the IAM-era one, that
   Silicon Accounts accepts the app credentials, the running API's readiness), backs up `native.env`, the unit, the
   release link and the database (locally and to S3), then with the API stopped writes the new `native.env`, applies
   migrations `0015`–`0017` (`waveform-api migrate`), links the mapping (dry run first; any conflict stops and rolls
   back), starts the API, waits for readiness, and asks Silicon Accounts for a test webhook delivery. Exit 0: done;
   exit 3: released but the webhook test failed (fix the URL or secret, then check it again); anything else: it
   rolled back (section 5). [`deploy/native/README.md`](../../deploy/native/README.md) explains each step.

   `native.env` loses `WAVEFORM_IAM_*`, `WAVEFORM_WEBHOOK_SIGNING_SECRET`, `WAVEFORM_WEBHOOK_KEY_VERSION`,
   `WAVEFORM_WEBHOOK_PUBLIC_URL`, `WAVEFORM_HONEYCOMB_SERVICE_TOKEN`, `SILICON_IAM_CLIENT_AUTO_UPDATE`,
   `BRIEFCASE_CLIENT_AUTO_UPDATE`, `WAVEFORM_BRIEFCASE_CDN_ORIGIN` and `WAVEFORM_BRIEFCASE_AUDIENCE` (the service
   refuses the last one at start), gains `ACCOUNTS_URL`, `WAVEFORM_APP_SECRET`, `WAVEFORM_ACCOUNTS_WEBHOOK_SECRET` and
   `WAVEFORM_PROOF_ISSUERS`, and `WAVEFORM_BRIEFCASE_APP_ID` changes from `waveform` to Briefcase's app id,
   `briefcase` (the old value is refused at start). Every other line stays byte for byte.

4. **Release the Silicon Interface** with User verification proofs for `receiving_app` `waveform` (scopes
   `waveform.tts` and/or `waveform.stt`) on `POST /api/v1/tts`, `POST /api/v1/stt`, `POST /api/v1/stt/source-target`
   and `GET /api/v1/audio/{request_id}`. TTS answers to a proof carry `storage.status = "held"` and an `audio_url`
   Interface downloads with the same proof. Interface's voice transcripts copy the recording into the account's
   Waveform folder (`apps/waveform`, from `POST /api/v1/stt/source-target`) before calling `POST /api/v1/stt`; on
   Briefcase's Accounts-era release an app's proof reaches only its own `apps/{app}/` folder, so that copy needs
   Briefcase's first-party exception for Interface (Briefcase's part 3 decision) or another hand-over. Check it in the
   integration run before this window: without it, voice transcripts from Interface fail while Waveform's own STT
   (sources in `apps/waveform`) works.

5. **Replace the website**: `python3 install-frontend.py --image <website image digest>`. It writes
   `/etc/waveform/web.env` (the app secret from `native.env`, a new session secret) and checks `/auth/session`.

6. **Verify** (section 4).

7. **Publish the CLI**: upload the Linux archives, release, test the development release, promote, then publish the
   crates (client first):

   ```sh
   silicon-apps upload waveform --target linux-x86_64 waveform-0.6.0-linux-x86_64.tar.gz
   silicon-apps upload waveform --target linux-aarch64 waveform-0.6.0-linux-aarch64.tar.gz
   silicon-apps release waveform --version 0.6.0 --package LINUX_X86_64_ID --package LINUX_AARCH64_ID
   silicon-apps install 'waveform>dev' && waveform accounts --json && waveform login
   silicon-apps promote waveform DEVELOPMENT_RELEASE_ID --version 0.6.0
   cargo publish --manifest-path client/Cargo.toml && cargo publish --manifest-path cli/Cargo.toml
   ```

   Keep the macOS and Windows archives for a later release, when Silicon Apps has validation workers for them.

## 4. Verify (run at cutover)

```sh
curl -fsS "$API/health/ready"
curl -fsS "$API/api/v1/accounts"                                   # app_id waveform, accounts_url production
curl -s -o /dev/null -w '%{http_code}\n' "$API/api/v1/me"          # 401
curl -s -X POST "$API/api/v1/auth/login" | jq .error.code          # "route_retired"
sudo cat /etc/waveform/native-release.json                          # identity silicon-accounts, webhook_test delivered
```

- **A Carbon** with the 0.6.0 CLI: `waveform login` (approve the code), `waveform me`, `waveform jobs` (earlier history
  is there when the account was linked), `waveform tts 'Cutover check'` (`storage.status` `stored`, or `held` with a
  reason while Briefcase is not on Silicon Accounts yet), `waveform storage status`.
- **A Silicon:** `silicon-accounts login --app waveform -q | waveform login --slt-stdin`, then
  `waveform login status --json` (`authenticated: true`, its uuid, id and custodian) and `waveform tts …`.
- **Its custodian:** `waveform silicons`, `waveform jobs --account si:<id>`.
- **The website:** sign in at https://waveform.teamofsilicons.com and see the same history.
- **The webhook:** `GET /v1/apps/waveform/webhook/deliveries?status=failed` is empty.
- **Interface:** a voice message gets its transcript.

## 5. Roll back

**Automatic.** When anything fails after the API stopped, `accounts_cutover.py` puts back the previous release,
`native.env` and unit, renames the migrated database to `waveform_cutover_<stamp>` (kept, never dropped), restores the
pre-cutover dump into a new `waveform` database, starts the previous API and checks it. A full restore is needed
because the previous binary refuses to start on a database with migrations it does not know.

**By hand, after a release that succeeded** (run at cutover, only with a Carbon's decision). Everything written since
the cutover (history, settings, keys saved since, held audio, linking) is lost, so prefer a forward fix once real
traffic has flowed. On the host, as root, with `B=/var/backups/waveform/accounts-cutover-<stamp>-<revision>`:

```sh
systemctl stop waveform-api
ln -sfn "$(cat $B/current-link.txt)" /opt/waveform/.current-rollback && mv -T /opt/waveform/.current-rollback /opt/waveform/current
install -m 600 $B/native.env /etc/waveform/native.env
install -m 644 $B/waveform-api.service /etc/systemd/system/waveform-api.service
systemctl daemon-reload
docker exec waveform-postgres psql -U postgres -d postgres -v ON_ERROR_STOP=1 \
  -c "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = 'waveform' AND pid <> pg_backend_pid()" \
  -c "ALTER DATABASE waveform RENAME TO waveform_rolled_back_$(date -u +%Y%m%d%H%M)" \
  -c "CREATE DATABASE waveform OWNER waveform"
docker exec -i waveform-postgres pg_restore -U postgres -d waveform --exit-on-error --single-transaction < $B/postgres.dump
systemctl reset-failed waveform-api && systemctl start waveform-api
curl -fsS "$API/health/ready"
```

Then put back the previous website (the unit saved as `/etc/waveform/frontend.service.before-accounts-web`,
`systemctl daemon-reload && systemctl restart waveform-frontend`), roll the Silicon Interface back to its IAM flow,
and remove the webhook URL (`DELETE /v1/apps/waveform/webhook`) or leave its deliveries failing until the next attempt.
A Silicon Apps release cannot be rolled back to an earlier Waveform there (earlier versions were never on Silicon
Apps): withdraw it (`silicon-apps withdraw waveform RELEASE_ID --reason "…"`) and installs fail with that reason until a
new release.

## 6. After the cutover

### Silicons still running the 0.5 CLI from Honeycomb

They stop working against the new service, by design, and say why: the 0.5 CLI signs in through
`POST /api/v1/auth/login`, which now answers `410 route_retired` with the replacement in its hint, and its saved IAM
tokens are refused (`401`, not a Silicon Accounts token). Honeycomb no longer updates them. Each one (or its runtime)
must:

1. remove the Honeycomb-installed `waveform` (`honeycomb uninstall waveform` while Honeycomb is still installed, or
   delete that binary), so the Silicon Apps one is the `waveform` on `PATH`;
2. `silicon-apps install waveform`;
3. sign in: `silicon-accounts login --app waveform -q | waveform login --slt-stdin`.

Sign-ins saved by 0.5 (`.waveform/dir/session-<digest>.json`) are never used: `waveform login status` reports
`"reason": "legacy_session"` and `waveform logout` deletes them, together with the cached test secrets
(`test-environments-*.json`). Every machine signs in again once; Carbons run `waveform login`.

### Transition aliases (one minor release)

The Silicon runtime (Stemcell's `silicon connect`) still runs `waveform iam --json` and `waveform login <SLT>`. 0.6.x
keeps both: hidden `waveform iam` prints exactly the `waveform accounts --json` object, and the positional
`waveform login <SLT>` is an alias of `--slt`. The token must come from `silicon-accounts login --app waveform -q`; an
IAM `oac_…` token is refused before anything is sent (`slt_not_a_short_lived_token`). Drop `waveform iam` in 0.7.

### Clean-up (each by a Carbon's decision)

- Delete the IAM-era keys from the Secrets Manager record (`WAVEFORM_IAM_APP_SECRET`, `WAVEFORM_WEBHOOK_SIGNING_SECRET`,
  `WAVEFORM_HONEYCOMB_SERVICE_TOKEN`) once rollback is no longer wanted; the backups under
  `/var/backups/waveform/accounts-cutover-*` hold the old `native.env` with them, so remove those too.
- Ask IAM's owners to remove Waveform's IAM app registration and its webhook to `/webhook/`, and Honeycomb's owners
  the `tos>waveform` catalog entry and test environments.
- The IAM-era tables (`waveform_actor_keys`, `waveform_storage_grants`, `waveform_environments`, the lifecycle and
  IAM webhook-event tables) and the earlier website's session store (`/var/lib/waveform/frontend-sessions`) are kept
  untouched; drop them later only by a Carbon's decision. The renamed `waveform_cutover_*` or
  `waveform_rolled_back_*` databases, if any, likewise.
- Earlier Briefcase grants were made for IAM and are marked invalidated by the linking. Each account's first TTS
  after the cutover gives Waveform its new Briefcase access (or `waveform storage approve`).
- Saved provider keys are re-sealed into the account rows by the linking; keys of a principal missing from the
  mapping stay on their earlier rows, unused and unchanged, until a later `link-identities` run links them.

## 7. Fleet and Ting prerequisites

- **Ting:** none. Waveform does not deliver through Ting and has no `WAVEFORM_TING_URL`.
- **Silicon runtime (Stemcell):** install Waveform with `silicon-apps install waveform` instead of
  `honeycomb install`; sign in with `silicon-accounts login --app waveform -q | waveform login --slt-stdin` (needs the
  Silicon's STK in `ACCOUNTS_STK` for `silicon-accounts`); discover with `waveform accounts --json`. Its voice-note
  instructions (`stemcell/silicon/tools.md`) pass `--org`/`--actor`, which 0.6 refuses with exit 2
  (`organizations_removed`, naming the fix): drop them. Its `app_configs.waveform.default_tts_provider: google` is not
  a Waveform provider id: use `gemini`.
- **Silicon Interface:** the switch to proofs in section 3, step 4, in the same window.
- **Briefcase:** its Accounts-era release first (section 1), with `waveform` allowed for `briefcase.uploads.reserve`,
  `briefcase.uploads.commit`, `briefcase.entries.list` and `briefcase.files.read` in `BRIEFCASE_PROOF_ISSUERS` (its
  production value in Briefcase's decisions does). Waveform's assumptions about Briefcase's routes are listed in
  [decisions](decisions.md#briefcase-waveform--briefcase); the end-to-end run checked them against a stand-in built
  from Briefcase's Accounts-era branch (proof checks, `apps/{app}/` confinement, `{origin}/{owner id}/{path}` URLs),
  and the integration run checks them against Briefcase itself.

Review correction: identity mappings are verified online for existence and kind even on dry runs. Review every source-to-target identity manually; matching mutable handles alone does not establish ownership. `--merge` cannot merge two different legacy identities. `--relink` is only a pre-use cutover correction: it refuses activity written after the previous link and preserves the old account's sign-out/deletion fence.

## Accounts 128-bit UUID backfill

Stop Waveform API/workers alongside the coordinated Accounts cutover and back up PostgreSQL. Apply release migrations through0020 first. Obtain the single persisted Accounts CSV with exact header `old_uuid,new_uuid,kind`; it includes deleted accounts. Do not independently generate UUIDs.

```sh
WAVEFORM_DATABASE_URL=... python3 scripts/migrate_account_uuids.py /secure/accounts-uuid-map.csv
WAVEFORM_DATABASE_URL=... python3 scripts/migrate_account_uuids.py /secure/accounts-uuid-map.csv --apply
```

Inspect the dry-run counts, then apply the same file. The importer uses one transaction and records immutable mappings; repeating the same file changes nothing, while collisions or a changed mapping roll back. Keep the export with the coordinated backup. Rollback requires restoring Accounts and every app backup together, before writers resume. Never reverse only Waveform while Accounts uses new subjects.

Private Waveform storage UUIDs and encrypted provider data remain intact. Old subjects are fenced; the new token identifies the existing storage key. Accounts revokes account token/proof families at this cutover: browsers/CLIs must sign in again. Saved Briefcase proof metadata remains visible with `account_uuid_migrated`; the account must approve storage again. Deploy this release before issuing UUID-shaped subjects and verify new sign-in → existing history/key access → explicit storage approval → actual Briefcase write/read. Existing human public handles and file URLs remain unchanged.

Preview refuses every legacy account reference absent from the shared export or its persisted mapping; canonical accounts already present are allowed. The permanent mapping ledger gates bearer/proof authentication independently of temporary pending-event cleanup. Signed webhooks naming a retired subject or custodian are recorded as ignored. Waveform has no outgoing signed account-event queue; inbound receipts contain only content-free metadata. No historical audio, request body or signed event payload is rewritten.
