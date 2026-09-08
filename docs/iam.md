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

The backend, Rust client and CLI use `silicon-iam-client` 1.3.0. Production
introspection, test-plane authorization, login, webhooks and storage proof
exchanges use the official SDK. Storage uses `briefcase-client` 0.2.0.
For bearer speech, Waveform mints a separate proof for every delegated listing
page, file read and upload. Proofs bind the SDK's exact request bytes and are
never persisted in jobs or idempotency responses. See `testing.md` for the
required Briefcase endpoint registration.

IAM's short-lived login token uses the `oac_` authorization-code prefix; the
login request field is still named `slt`. Access and refresh tokens use `oat_`
and `ort_`. The default speech authorization scope is `obo.issue`, which IAM
issues to applications for downstream storage delegation. Organization,
application audience, actor identity, expiry, and test-plane checks still apply.
Deployments may explicitly configure a stricter issued scope through
`WAVEFORM_IAM_TTS_ACTION` and `WAVEFORM_IAM_STT_ACTION`.
