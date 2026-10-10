# Public identifier cutover

Waveform uses bare application identities (`waveform`, `briefcase`), `c:handle` / `si:handle` public actors and unchanged organization IDs. OBO endpoint catalogs bind the requested bare app ID and return its authoritative organization; app ownership is no longer parsed from the audience string.

Stop writers and back up every data plane. Complete IAM's global collision preflight/cutover, apply migration 0014, update the configured application/storage audiences and `obo:briefcase:...` scopes, and deploy the IAM SDK 4.0.0 and matching Briefcase SDK consumer together.

Migration 0014 maps public identity bindings and lifecycle app IDs while retaining every `storage_actor_id` UUID. Colliding public actor names in one plane fail the transaction. Jobs, audio history, private preferences, provider credentials, profile relationships, idempotency receipts and provider-key encryption AAD continue using the original private UUID. Exact historical response bodies and signed lifecycle receipts are not rewritten.

Refresh session metadata and verify old audio history, BYOK decryption, existing preferences, generation/replay, Briefcase reads/uploads, cross-org and cross-plane rejection. Validate production and each sandbox. Rollback requires coordinated database/configuration backups and old binaries; old code must not write against the migrated bindings.

Run `python3 scripts/verify-identifier-migration.py` to verify the migration chain, retained private UUIDs and collision rollback against an isolated Docker PostgreSQL 16 instance.

The matching IAM SDK 4.0.0 source is vendored under `vendor/silicon-iam-client`, with normalized Cargo metadata and local snapshot provenance. Standalone and Docker builds use this source. This change does not publish a new SDK release.
The matching Briefcase SDK snapshot is vendored under `vendor/briefcase-client` for the same reason.
