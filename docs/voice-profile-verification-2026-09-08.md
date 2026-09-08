# Voice profile verification — 2026-09-08

## Scope

Implemented the voice-profile section in UNDERSTANDING.md: account default
assignment, default changes, per-generation overrides, Gemini/OpenAI/ElevenLabs
mappings, and automatic use of those mappings during fallback. Source changes
include the API, PostgreSQL migration, Rust client, CLI, and SolidJS frontend.
No AWS deployment or frontend hosting was performed for this feature.

## Automated evidence

- 153 backend unit/control tests and 8 PostgreSQL idempotency integration tests
  pass, including all normally ignored database tests. Final run used a fresh,
  disposable database on the dedicated local test cluster, then deleted it.
- 9 Rust client tests and 5 CLI tests pass. CLI help exposes `voice-profiles`,
  `tts --voice-profile`, and the account preference flag.
- 15 frontend gateway tests pass. TypeScript checking and Vite production build
  pass. Rust Clippy passes with warnings denied. OpenAPI YAML parses.
- Backend cases cover the 30-profile seed, migration consistency, invalid IDs
  and settings, first-login assignment, account/organization/environment
  isolation, test cleanup, automatic mapping revisions, account default versus
  explicit override, fallback through every provider, OBO actor resolution,
  profile-aware request digests, and metadata-preserving completed replays.
- Provider HTTP mocks verify actual adapter requests: Gemini receives Puck,
  OpenAI receives shimmer, and ElevenLabs receives the mapped path, model, and
  every voice setting, including zero values and false speaker boost.

An intermediate test run detected a migration checksum mismatch because the new,
unreleased migration had changed after being applied to the earlier local test
DB. The final tests ran all migrations from scratch in a fresh disposable DB;
no production migration or database was altered.

## Browser evidence

Using the built frontend and disposable local HTTP fixture on localhost:4341:

1. Signed in as the synthetic fixture identity and saw 30 profiles.
2. Selected Puck for one generation; the result reported `puck`.
3. Opened account settings and verified its default was still Kore.
4. Saved Sulafat, then verified that default appeared in the speech composer.
5. Selected Account default and moved ElevenLabs first; the result reported
   ElevenLabs with `sulafat`.
6. Inspected provider mapping details and the provisional mapping notice.
7. Opened job details and verified `sulafat · revision 1`.
8. Reloaded settings and verified Sulafat remained selected.
9. Checked the 390 × 844 layout visually and through DOM dimensions: document
   width 390px, viewport 390px, selector width 304px; no horizontal overflow.
   The viewport override was reset afterward.

The browser's password-manager extension logged an autofill DOM error. Its stack
was entirely in the extension; the application flows above completed.

## Limits

This run did not restart the earlier real paired IAM/Briefcase stack, contact
paid speech providers, or perform listening comparisons. PostgreSQL/control
integration tests, adapter HTTP contract tests, and the browser fixture verify
routing and persistence. They do not prove acoustic similarity or ElevenLabs
account access. All initial mappings remain `auditioned: false` until reviewed.
The previously observed ElevenLabs account billing restriction remains relevant
for a later live audio review.
