# Waveform testing environments

Waveform testing environments are isolated planes backed by the IAM and
Briefcase testing environments supplied when the plane is created. A request
selects a plane with `X-Testing-Environment-Key`; the header is mandatory for
test traffic and omission always means production. Waveform never accepts a
production IAM or Briefcase root key as a substitute for a test key.

## Create a plane

Authenticate against the production Waveform plane and call
`POST /api/v1/testing-environments`:

```json
{
  "name": "agent-e2e",
  "description": "local end-to-end run",
  "iam_environment_id": "<IAM test-environment UUID>",
  "iam_environment_key": "<32-character IAM root key>",
  "app_secret": "<test-only Waveform application secret>",
  "briefcase_environment_key": "<32-character Briefcase root key>"
}
```

The create response contains a Waveform root key. Store it in a secret
manager or the CLI's private store. Waveform stores only encrypted upstream
keys and an HMAC digest of its root key. The application secret must belong to
the test IAM plane; using the production application secret is rejected by IAM.

Organization members can list environments and inspect metadata. The creator,
organization administrators, organization heads, and owners can retrieve the
current root key, rotate it, soft-delete the environment, and restore it during
the 30-day recovery window.
The root key is not returned in list or detail responses. A rotated key
immediately invalidates the old key.

## Use and clean a plane

Pass the root key on every test login, speech, and control request:

```text
X-Testing-Environment-Key: <Waveform root key>
```

The selected IAM client uses the stored IAM test key and the selected
Briefcase client uses the stored Briefcase test key. `GET
/api/v1/testing-environment` returns the selected metadata. `POST
/api/v1/testing-environment/clean` removes jobs, webhook deduplication records,
provider preferences, and personal provider keys while preserving the plane
and its root key. Cleaning is allowed with the root key so an automated test
runner can reset its sandbox without a second administrative login.

Every test-plane request updates activity. Waveform marks a plane deleted after
15 days without activity. A deleted plane can be restored for 30 days; after
that retention window its row and all scoped data are permanently purged.

## Deterministic speech fixtures

At startup Waveform seeds 30 packaged spoken MP3 clips, one for each Gemini
voice profile, into `waveform_speech_fixtures`. Every clip speaks the same
prescribed test message from `UNDERSTANDING.md`. Test TTS selects the clip using
the resolved profile’s Gemini voice, so account defaults and request overrides
are audible in the testing environment. No profile defaults to another voice
when its clip is missing. Test TTS loads the selected audio from PostgreSQL.
Waveform hashes the final bytes, exchanges the caller's
test-plane access token for a Briefcase upload proof using the paired IAM SDK
client, and uploads those bytes through the official Briefcase SDK with the
paired Briefcase root key. Each new request has a distinct generated filename.
The response contains a permanent URL and `temporary_url: null`. An upstream
failure returns an error; the runtime does not substitute an in-memory upload
or fabricate a successful storage URL. Unit-test-only storage fixtures remain
in the test suite.

Briefcase 0.2.0 supports delegated listings and file reads. Waveform resolves
an organization-qualified permanent URL by listing its parent with a fresh
IAM proof for each page, then obtains a separate proof for the exact file read.
STT returns the prescribed fixture transcript after the authorized read and
local audio validation. Its duration comes from the uploaded audio, using the
same inspector as production. Only the billable speech providers are replaced. TTS
and STT retries recheck current file access with a one-byte range request before
returning cached results. A revoked file cannot expose a cached transcript.

Register these endpoints in the paired IAM Briefcase application:

| Endpoint | Full path | Metadata schema |
| --- | --- | --- |
| `briefcase.files.create` | `/api/v1/obo/files` | `path`, `name`, `content_type` strings |
| `briefcase.entries.list` | `/api/v1/obo/entries/list` | empty object |
| `briefcase.files.read` | `/api/v1/obo/files/read` | empty object |

The test access token must be issued to Waveform's application and allow OBO
issuance. Every downstream exchange binds the exact bytes sent by the official
Briefcase client. Never reuse a proof for pagination, a read, or a retry.

A paired-environment run requires the paired environment keys, the test
Waveform application secret, and a test SLT (`oac_…`). IAM and Briefcase can run
as real isolated local services; internet deployment is not required. On
2026-09-08, the compiled CLI passed real paired-service checks for both actor
kinds, storage, sessions, preferences, webhooks, and lifecycle operations. See
[test-report-2026-09-08.md](test-report-2026-09-08.md).

The assets and generation/transcription QA manifest live in
`src/infrastructure/test-audio/`. Regenerate explicitly with
`python3 scripts/generate_test_audio.py --voice all --aws-secret --verify`; this
one-time authoring command contacts providers, while test requests never do.
Existing verified assets are reused. The legacy default fixture remains Kore.

Gemini is the successful fixture provider. The other fixture providers return
bounded unavailable errors, allowing account and per-request ordering to exercise
fallback without paid requests. Default order is read from PostgreSQL on each
request. A successful response names the provider that completed the operation.

IAM webhooks are different from client requests: IAM signs the exact outer
body and embeds its IAM test root in `test.testing_key`. Waveform verifies that
signature through the SDK and routes the event to the matching paired plane.
Do not attach a Waveform root header to an IAM delivery. Use a distinct IAM and
Briefcase pair per independently isolated Waveform environment.

The returned TTS `file_url` is Briefcase's permanent entry URL. Use the Briefcase
SDK to read/download it. For direct HTTP, Briefcase's `?disposition=inline`
content route requires its organization, IAM bearer and paired Briefcase test
headers; the URL itself is not an access capability.

## Local regression checks

Run `python3 scripts/local_database.py`, source the generated environment
file shown by the script, then run:

```sh
cargo build --manifest-path cli/Cargo.toml
cargo test --all-targets --all-features
cargo test --all-targets --all-features -- --ignored
```

The ignored tests use isolated PostgreSQL schemas. They include actual CLI
processes talking to a local HTTP server and full TTS requests passing through
the HTTP router, database, IAM SDK exchange, and Briefcase SDK upload. Tests also
exercise Carbon and Silicon STT, paginated resolution, exact manifest hashes,
single-use proofs, TTS/STT replay and denial after file access is revoked. Upstream
HTTP services in these tests are mocks, so they validate protocol wiring and
isolation rather than deployed IAM/Briefcase behavior. No paid speech provider
is contacted.

## Isolation checks

An end-to-end run should verify that:

1. a production token cannot select a test IAM authorization without the test
   key;
2. two test root keys cannot read each other's jobs, preferences, files, or
   webhook deduplication state;
3. rotating a key invalidates the old key and preserves the environment ID;
4. cleaning removes scoped data but leaves the environment usable; and
5. deleting, restoring, inactivity expiry, and 30-day purge do not leak keys or
   data into another plane.
