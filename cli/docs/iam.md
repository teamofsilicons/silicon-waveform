# IAM integration

> **Integration preview for Waveform 0.5.0 / IAM 5.0.0 / Briefcase 3.0.0.** These guides precede the coordinated runtime rollout. Upgrade dependent services and clients together; public documentation alone does not indicate the new runtime is live.

Waveform pins the official IAM 5.0.0 SDK at `f1e9c4768029aacabe337ca41be52e05023d1631` and Briefcase 3.0.0 client at `ba9e5211b2c4eebd3122f15607b28408bbe22fb6`.

Application login still exchanges an IAM `oac_` SLT for `oat_` and `ort_` credentials, bound to exactly one account and organization. Login, refresh and logout accept a stable 16–255 character `Idempotency-Key`; keep it across uncertain retries. Ordinary speech login now requests `self.identity.read`. Storage permission is requested separately when the feature is used; obsolete `obo:` login scopes must be removed from the Honeycomb application configuration and deployment overrides.

## Direct storage consent

`POST /api/v1/storage-authorizations` with `{}`, the current bearer, selected `X-Org-ID`, and a stable idempotency key returns `{authorization_id,consent_url,state,status,expires_at}`. Open the trusted IAM consent URL. The user can choose a different account and organization for Briefcase. Enter the code at `POST /api/v1/storage-authorizations/{id}/complete` with `{code,state}`. `GET /api/v1/storage-authorizations/{id}` reads the status for the same account, organization and test plane. These routes return `Cache-Control: no-store`; tokens are never returned to the browser or CLI.

The web speech form preserves the pending body and logical idempotency key in memory across manual consent, then requires an explicit retry. The CLI exposes `waveform storage --org ORG start`, `status`, and `complete --code-file FILE --state STATE`. A permission error does not log the user out. The same speech key and request ID must be retained when retrying.

The broker encrypts IAM access/refresh credentials with `WAVEFORM_ENCRYPTION_KEY`, using authenticated data bound to the plane, origin account, org and endpoint. It rotates near-expiry tokens under a PostgreSQL row lock. A stable secret-derived upstream idempotency key recovers an uncertain refresh response. Removed consent produces `403 storage_authorization_required`; IAM outages remain retryable dependency errors. Ordinary Waveform logout does not delete the durable feature grant. IAM still enforces credential security revocation, membership, application state and graph changes.

## Incoming OBO and chaining

Speech accepts `X-IAM-OBO-Access-Token: oba_…` together with `X-App-ID`, or an ordinary Bearer. The retired proof header and mixed credentials are rejected. Each request is verified online through IAM `POST /api/v1/obo-access/token-verifications` as Waveform's own authenticated app, against `waveform.tts` at `/api/v1/tts` or `waveform.stt` at `/api/v1/stt`. Repeated requests may use the same token while its authority remains live.

Waveform forwards that same token to a declared Briefcase dependency, using IAM delegation only to validate the edge and resolve the recipient's selected subject, organization and testing secret. It never turns an incoming OBO request into an ordinary actor login or stores that incoming chain token. Downstream authority always uses Briefcase's selected account and organization.

## Endpoint configuration and uploads

Register the Briefcase endpoint roots `briefcase.uploads.reserve`, `briefcase.uploads.commit`, `briefcase.entries.list`, and `briefcase.files.read`. Declare TTS dependencies on reserve, commit and list; declare STT dependencies on list and read. Keep the corresponding exact `/api/v1/obo/...` paths in the catalog. App registrations and active private-provider visibility must allow these dependency graphs.

TTS checks storage authority before paid generation. Briefcase then reserves the exact filename, size and SHA-256 under the stable speech operation UUID. Only a narrow staging capability goes with the raw bytes. Briefcase rechecks the commit access token before publication; Waveform resolves the committed entry with list authority and returns its verified permanent URL. The retired raw `/obo/files` route is never used. Source reads and cached speech responses recheck live resource access through the same approved endpoint tokens.

Testing uses the selected Waveform app secret and IAM test context. An absent or mismatched downstream testing context fails closed. Clean/purge deletes broker authorization and grant rows while lifecycle fences exclude active operations. Nothing falls back to production.

## Upgrade and rollback

Apply additive PostgreSQL migration `0015_storage_consent.sql` before enabling the new runtime. It stores encrypted pending authorizations and durable grants, partitioned by plane, actor, organization and endpoint. Back up the database and preserve `WAVEFORM_ENCRYPTION_KEY`. The preceding binary can run with these unused additive tables, but old OBO calls require the old coordinated IAM and Briefcase runtimes; roll the integration set back together. Do not roll back schema or delete grants as an authentication repair.

Update consumers to `X-IAM-OBO-Access-Token` and request feature consent after login. An ATA verification is app authority and cannot be supplied to these OBO routes. Test both actors, cross-organization destination selection, revoked consent, expired tokens, uncertain refreshes and test-plane cleanup before publishing the application.
