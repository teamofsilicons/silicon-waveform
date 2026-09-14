# IAM integration

Waveform uses the official `silicon-iam-client` package. The server's IAm app
ID and secret are deployment secrets. Browser or agent login supplies only a
short-lived token to `/auth/login`; Waveform exchanges it and returns the IAM
access and refresh tokens with `Cache-Control: no-store`. Every protected
request rechecks current IAm authorization, so logout and membership changes
apply immediately even if a webhook is delayed. IAM events are authenticated
with the configured versioned signing key and retained only as content-free
metadata for deduplication. Bearer introspection uses IAM's published
`/api/v1/oauth/introspect` route. OBO speech remains fail-closed until the
downstream actor-context exchange is defined by IAM. An inbound OBO proof is
not an `oat_` token issued to Waveform and cannot be reused as an exchange subject.

For login, refresh and logout, callers should send a stable `Idempotency-Key`
(16–255 visible ASCII characters) and retain it across retries of that same
operation. Waveform validates and forwards it unchanged to IAM in production and
testing. This lets IAM replay its saved result after a response is lost, including
when a login code or refresh token has already been consumed. Duplicate or invalid
keys are rejected before contacting IAM. Omitting the header remains supported,
but generates a new key per request and cannot recover a prior request's receipt.

The backend, Rust client and CLI use `silicon-iam-client` 1.8.0. Production
introspection, test-plane authorization, login, webhooks and storage proof
exchanges use the official SDK. Storage uses `briefcase-client` 1.0.3.
For bearer speech, Waveform mints a separate proof for every delegated listing
page, file read and upload. Proofs bind the SDK's exact request bytes and are
never persisted in jobs or idempotency responses. See `testing.md` for the
required Briefcase endpoint registration.

IAM's short-lived login token uses the `oac_` authorization-code prefix; the
login request field is still named `slt`. Access and refresh tokens use `oat_`
and `ort_`. The default TTS permission is `obo:tos>briefcase:briefcase.files.create`; STT
requires `obo:tos>briefcase:briefcase.files.read`. Waveform checks these before
provider work. Current IAM uses explicit
`app_scope.external` endpoint permissions for storage delegation; it no longer issues
the old `obo.issue` scope. IAM checks the user’s consent and selected organization
again for every endpoint-bound OBO exchange. Organization,
application audience, actor identity, expiry, and test-plane checks still apply.
Deployments may explicitly configure a stricter issued scope through
`WAVEFORM_IAM_TTS_ACTION` and `WAVEFORM_IAM_STT_ACTION`.

## Storage permission

Declare only the Briefcase private-file creation permission for generated audio. Do not request global file-write or administration scopes. Declare `briefcase.files.create`, plus `briefcase.entries.list` and `briefcase.files.read` for source reads and replay access checks. Include `self.identity.read`, `self.membership.read`, and `self.tags.read` for Briefcase’s resource authorization, alongside the existing `self.profile.read` permission. The generated-audio OBO endpoint is `briefcase.files.create`; the private application folder and actor are resolved by Briefcase. Source transcription still requires delegated read permission for the supplied file. See [Briefcase OBO](https://docs.briefcase.teamofsilicons.com/obo/) and [testing](testing.md) for request-bound downstream test credentials.

## Current hosted integration limit

The September 13 deployment verifies app-secret discovery, public-ID login and endpoint-bound IAM exchange against live services. However, IAM proof verification currently returns only the delegated endpoint scope, with `org_role` and `tags` undisclosed. Briefcase requires those fields and rejects the upload. Adding the declared permissions alone does not resolve that upstream contract mismatch. See [deployment verification](verification-2026-09-13.md).
