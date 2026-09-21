# IAM 3 consumer cutover

Waveform uses the published IAM 3 Rust client for server, client and CLI requests. Both current IAM responses with old extra UUID fields and new canonical responses are accepted. Authentication uses the immutable `public_id`; a private Waveform UUID continues to key history, preferences, idempotency and provider-key encryption contexts.

Publish Rust client 0.3.0, then CLI 0.3.0 and all six native Honeycomb targets before changing the backend. Upgrade installed clients first: published 0.2.0 clients use an IAM SDK that requires the removed UUID identity fields. The 0.3.0 client accepts both old and new backend responses, but the new backend does not recreate removed IAM fields.

Before starting this backend, pause API/worker requests, take a database backup, run migration 0013 and import the refreshed private pre-cutover IAM identity export:

```sh
python3 scripts/import-iam-identities.py --database-url "$WAVEFORM_DATABASE_URL" --identity-map /private/identity-mapping.json
```

The export contains `production` and `testing` arrays with `legacy_id`, `public_id` and `testing_environment_id`. It must remain outside source control. The importer binds only retained actors, using each Waveform plane's actual IAM environment. Missing or contradictory bindings abort atomically. Existing data and encrypted provider-key bytes remain unchanged. The new backend must not serve requests before this import, since it would otherwise allocate fresh private keys for existing users.

Canonical actor bindings are deleted with their test environment's data on cleanup. Production and two test environments may use the same canonical handle while retaining different private row keys. New accounts receive private UUIDs. Webhook aggregate metadata now accepts canonical identity strings and existing resource UUID strings.

Deploy the compatible backend before IAM 3. Keep it if IAM rolls back. Rolling back to the old backend is safe only before IAM begins returning canonical-only responses, while the old IAM UUIDs still match the retained storage keys. Do not change or re-encrypt provider-key ciphertext as part of this rollout.

Validation includes the real PostgreSQL import test (production plus two isolated test environments, incomplete-export rollback, exact-repeat imports and preserved ciphertext), control API login/preferences/provider-key decryption, online revocation, sandbox cleanup, speech/storage boundaries and session retry receipts. Server, client and CLI tests also cover automatic refresh and saved-session isolation.
