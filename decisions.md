# Silicon Waveform engineering decisions

This is the append-only decision log for the Silicon Waveform backend. Material
product interpretations, architecture choices, security controls, data-model
rules, integration assumptions, and operational defaults are recorded here
before or alongside implementation. Superseded decisions remain in the log and
are marked with their replacement.

## D-001 — Contract precedence and conservative interpretation

**Status:** Accepted

`UNDERSTANDING.md` defines product intent and provider order, `API_DOCS.md`
defines human-readable behavior, and `openapi.yaml` defines the public wire
shape. More specific requirements take precedence over general prose. When the
documents are silent or conflict, Waveform chooses the interpretation that
preserves actor authorization, organization isolation, idempotency, and the
documented response shape. Public-contract clarifications are added to the
OpenAPI document without removing a documented capability.

## D-002 — Modular monolith with ports and adapters

**Status:** Accepted

Waveform is one Rust package with a thin API binary and a reusable library. The
library is split into domain, application, HTTP API, and infrastructure modules.
IAM, Briefcase, idempotency storage, audio normalization, and every speech
provider are accessed through small traits. This gives one deployable service
while allowing incomplete upstream contracts and provider implementations to
change without contaminating request handlers or domain policy.

Microservices and an asynchronous job service are deferred. The documented
public contract is synchronous, and no job resource or cancellation contract
exists yet.

## D-003 — Rust safety and quality baseline

**Status:** Accepted

The service uses Rust 2024 with a pinned stable toolchain, `unsafe_code` denied,
strict Clippy lints, rustfmt, a committed lockfile, dependency-policy checks, and
tests at domain, application, adapter, and HTTP boundaries. Production code does
not use `unwrap`, `expect`, `todo!`, or `unimplemented!` for recoverable states.
Axum, Tokio, Tower, Reqwest with rustls, SQLx, Serde, and tracing form the core
runtime stack.

## D-004 — PostgreSQL is authoritative for idempotency

**Status:** Accepted

PostgreSQL stores shared idempotency records and minimal request/attempt
metadata. A process-local store exists only for deterministic tests. This keeps
multiple replicas from charging a provider or creating a Briefcase file twice.
No Redis dependency is required for correctness.

Waveform does not persist source text, source media, generated audio,
transcripts, bearer tokens, OBO proofs, or signed temporary URLs in its database.
Generated audio is durable only in Briefcase. Operational attempt records retain
provider name, outcome category, timing, and request ID but not payload content.

## D-005 — Idempotency scope and lifecycle

**Status:** Accepted

An idempotency key is scoped by represented actor type and ID, organization,
operation (`tts` or `stt`), and key. A SHA-256 digest of the canonical validated
request body binds the key to one request. Reusing a key with a different digest
returns `409 idempotency_key_reused`. A completed record replays the exact
normalized success response with its original request ID.

The first caller acquires a bounded lease. A concurrent duplicate with a live
lease returns `409 request_in_progress` and `Retry-After`; an expired lease may be
reclaimed. Terminal successes are retained for 24 hours by default. Failed work
releases its lease so the same key can be retried. Briefcase receives a stable,
derived idempotency key so a retry after a partial upload cannot create a second
file.

## D-006 — Online IAM authorization and ambiguous credentials

**Status:** Accepted

IAM access tokens are opaque and are introspected online for every speech
request. OBO proofs are verified online for the requested Waveform audience,
action, actor, organization, and current time. `X-Org-ID` must exactly match the
verified organization. Waveform does not use a positive authorization cache, so
logout or membership removal is enforced by IAM without depending on webhook
delivery.

Exactly one credential mode is accepted. Supplying both Bearer and OBO
credentials is rejected as an ambiguous request. OBO requires both
`X-IAM-OBO-Access-Proof` and `X-App-ID`; a partial pair is rejected. Secrets are
redacted and never included in structured logs or errors.

## D-007 — Reject the legacy deterministic proof construction

**Status:** Accepted security correction

Waveform does not implement the older `hash(auth_token + app_secret)` proof
described in the understanding documents. It relies on short-lived,
audience-bound, action-bound, replay-resistant IAM OBO proofs described by the
newer IAM API documentation. An inbound proof for Waveform is never forwarded to
Briefcase because its audience is different.

The initial action identifiers are `waveform.tts` and `waveform.stt`, with the
configured Waveform application ID as audience. They are isolated in
configuration so IAM can finalize the vocabulary without changing domain code.

## D-008 — Incomplete IAM and Briefcase contracts fail closed

**Status:** Accepted

The current IAM contract has no operation for Waveform to mint or exchange an
actor-bound proof for Briefcase, and its documented OBO verification operation
is absent from IAM OpenAPI. Briefcase has no operation to resolve an actor's
`apps/{app_id}` folder and no authenticated content-read operation for a
permanent URL. Waveform represents each missing ability as a port rather than
inventing a public or cross-service wire contract.

HTTP adapters support only operations established by dependency contracts.
When safe delegated authorization or content access cannot be obtained, the
request fails with `503 dependency_contract_unavailable`. Tests use explicit
port fakes to exercise the complete Waveform workflow. This decision must be
superseded when IAM and Briefcase publish the missing operations.

## D-009 — Provider ordering, fallback, and retries

**Status:** Superseded by D-020

TTS attempts Gemini, ElevenLabs, then OpenAI. STT attempts Gemini, OpenAI, then
Deepgram. The first successful normalized result wins. A provider attempt has an
explicit timeout and no hidden same-provider retry; retrying a synchronous POST
can duplicate chargeable work, while moving to the next documented provider is
bounded and observable. Any provider network error, timeout, rate limit,
upstream 5xx, authentication/configuration failure, rejected request, or invalid
response is recorded and falls through to the next provider. Client validation
occurs before provider invocation. If the complete chain fails, the caller gets
one normalized `502 providers_exhausted` response without provider-private error
bodies.

Provider adapters have independent concurrency semaphores. Saturation fails an
attempt fast and permits the fallback chain to continue rather than building an
unbounded in-memory queue.

## D-010 — Language-hint behavior

**Status:** Accepted

Language hints must parse as BCP 47 and are canonicalized before use. The TTS
`lang` value is sent only to Gemini, matching the explicit understanding; the
other TTS adapters ignore it. The STT `language` value is passed to every STT
provider using that provider's supported representation. A detected language is
returned only when the successful provider supplies one reliably; Waveform does
not guess it from the request hint.

Capabilities describe accepted best-effort hints, not a promise that every
provider supports every language. The exact stable list is kept in one domain
constant and documented in OpenAPI.

## D-011 — Synchronous request limits

**Status:** Accepted security and cost control

Until an asynchronous job API exists, TTS accepts at most 4,096 Unicode scalar
values and rejects whitespace-only text. STT accepts only configured
Briefcase-hosted HTTPS permanent URLs and at most 25 MiB of downloaded media by
default. Content length is enforced both before and while streaming. Accepted
media types are an explicit allowlist shared with `/capabilities`; declared media
type and file signature are checked where practical.

These defaults match the narrowest documented fallback constraints and are
configurable only toward a deployment's verified provider limits. Oversized
input returns `413`; unsupported media returns `415`. Long media must wait for a
future durable job contract rather than holding an unbounded connection.

## D-012 — MP3 normalization boundary

**Status:** Accepted

Every TTS provider returns a typed audio artifact. Existing valid MP3 is passed
through; raw PCM or another accepted provider format is normalized to MP3 by an
audio-normalizer port. The production adapter invokes a pinned FFmpeg binary
with fixed arguments, piped input/output, a timeout, bounded output, and no shell.
The container installs that exact runtime dependency. This avoids writing or
maintaining an audio codec in application code while keeping all orchestration
in Rust.

Duration is measured from decoded audio metadata or exact PCM sample counts,
never estimated from text length. Invalid or empty audio is treated as a
provider failure.

## D-013 — Briefcase references, storage, and filenames

**Status:** Accepted

STT accepts only HTTPS URLs whose origin exactly matches the configured
Briefcase permanent-URL origin. It never fetches a caller-provided URL directly.
The Briefcase port must re-authorize the represented actor and return bounded
media bytes; any temporary CDN URL is validated against the configured CDN
origin before download. Redirects are disabled.

TTS files are placed in the represented actor's Waveform application folder.
The filename is UTC
`tts_YYYYMMDD_HHMMSS_<request-id>.mp3`: it preserves the documented
`tts_{date}_{time}` prefix and adds the request UUID to prevent collisions.
The permanent URL is the durable response reference; the temporary URL is
returned for immediate playback and is never logged or stored in Waveform.

Failure to create the temporary URL fails the synchronous TTS request, because
the public success schema requires it. A retry uses the same Briefcase
idempotency key and request-derived filename so it can recover the already
created entry without duplication.

## D-014 — Error and request-ID contract

**Status:** Accepted

Every request receives a UUIDv7 request ID. It is returned in
`X-Request-ID`, propagated to internal dependency calls when supported, included
in every success, and included in every error envelope. Client-supplied request
IDs are accepted only if they are valid UUIDs; otherwise Waveform creates one.

Errors are stable Waveform codes mapped to explicit HTTP statuses. Provider
details, response bodies, credentials, input text, transcripts, and signed URLs
are not exposed. Retryable dependency failures use `502`, `503`, or `504` as
appropriate; validation, authentication, authorization, payload, media, and
idempotency failures use explicit 4xx statuses.

## D-015 — Bounded HTTP and dependency behavior

**Status:** Accepted

The public server applies JSON/body limits, a total synchronous deadline,
load-shedding, graceful shutdown, sensitive-header redaction, and bounded
concurrency. Every outbound client uses rustls, explicit connect and operation
timeouts, redirects disabled by default, bounded bodies, and response status
classification. Provider and dependency base URLs are parsed and validated at
startup.

The total endpoint deadline is larger than the sum of configured attempt
budgets but remains finite. Cancellation drops in-flight futures; operations
that may already have charged a provider or created a file remain recoverable
through the idempotency key.

## D-016 — Privacy-minimal telemetry

**Status:** Accepted

Structured logs and metrics contain request ID, operation, actor type, a keyed
non-reversible actor label, organization label, provider, attempt outcome,
latency, response class, and byte/character counts. They never contain source
text, transcript text, media, bearer tokens, OBO proofs, provider keys,
application secrets, permanent URLs, temporary URLs, or raw provider error
bodies. Panic bodies are converted to a generic internal error.

## D-017 — Public capabilities are deterministic

**Status:** Accepted

`GET /api/v1/capabilities` is unauthenticated, side-effect free, and generated
from compile-time normalized capabilities rather than live provider probes. All
nested response fields are always present. Results are cacheable for five
minutes because provider health does not change the stable public contract.

## D-018 — Operational endpoints are separate from the public API

**Status:** Accepted

Waveform adds `/health/live` and `/health/ready` outside `/api/v1`. Liveness
reports only process health. Readiness checks required configuration,
PostgreSQL, the audio normalizer, and that at least one configured provider
exists for each operation; it does not call billable provider APIs. These routes
do not accept actor credentials and are documented as operational rather than
product endpoints.

## D-019 — Dependency injection and deterministic tests

**Status:** Accepted

Application services depend on object-safe asynchronous ports held behind
`Arc`. Production composition creates concrete HTTP/PostgreSQL/FFmpeg adapters;
tests use narrow fakes. Time and request-ID generation are injected where their
behavior affects names, leases, or replay. Provider adapter tests use local mock
HTTP servers and never require real credentials or make billable requests.

## D-020 — One bounded transient retry before provider fallback

**Status:** Accepted; supersedes D-009 only where retry count is concerned

The provider order and failure normalization from D-009 remain unchanged. An
adapter may retry one time for a connection failure, `408`, `429`, or `5xx` when
the operation still has sufficient deadline budget. It uses capped exponential
backoff with jitter and honors a bounded `Retry-After` value. Authentication,
permission, credit, validation, unsupported-media, and other deterministic 4xx
responses are not retried within the adapter, but the orchestrator still moves
to the next documented provider. This exception is required because the Gemini
3.1 TTS documentation explicitly calls for an automated retry for a known rare
HTTP 500 mode. More than one same-provider retry is deferred until production
evidence justifies its added latency and duplicate-charge risk.

## D-021 — Exact provider models and wire protocols

**Status:** Accepted

The exact product-specified models are current official models as of
2026-08-31 and are the configuration defaults:

- Gemini TTS: `gemini-3.1-flash-tts-preview` through the Gemini Interactions API.
- ElevenLabs TTS: `eleven_multilingual_v2` through synchronous
  `/v1/text-to-speech/{voice_id}` with `mp3_44100_128` output.
- OpenAI TTS: `tts-1` through `/v1/audio/speech` with explicit MP3 output.
- Gemini STT: `gemini-3.5-transcribe` through Files plus Interactions.
- OpenAI STT: `gpt-transcribe` through multipart `/v1/audio/transcriptions`.
- Deepgram STT: `nova-3` with `language=multi` through synchronous
  `/v1/listen`.

Model IDs, base URLs, voices, timeouts, and concurrency limits remain typed
configuration so preview replacements and regional endpoints do not require a
domain change. Startup validation prevents an empty model or voice. Waveform
does not silently replace an unavailable requested model with a different model
family.

Gemini 3.1 TTS has no dedicated language-code field. To honor the Waveform
contract, the optional BCP 47 hint is included in the explicit speech
instruction while preserving the original text byte-for-byte. ElevenLabs
Multilingual v2 and OpenAI `tts-1` receive no language field. Gemini and OpenAI
STT receive supported language hints; Deepgram uses a compatible base language
when supplied and `multi` when it is absent.

## D-022 — Provider media handling and temporary uploads

**Status:** Accepted

Provider API success bodies are bounded before allocation. Binary TTS content
must be non-empty and match an allowed audio format before normalization. STT
source media is held only for the duration of the request and is not written to
the Waveform database.

Gemini STT uploads media to the Gemini Files API because its transcription
contract references an uploaded file URI. Waveform waits only within the
provider attempt deadline, calls Interactions after the file becomes usable,
and makes a best-effort deletion of the provider file after success, failure, or
cancellation. Cleanup failure is logged without changing an otherwise valid
transcription result; lifecycle and provider-side retention remain governed by
the configured Gemini account policy.

## D-023 — Provider-specific deadlines reflect workflow shape

**Status:** Accepted

TTS and STT do not share a provider timeout. Gemini defaults to 45 seconds for
TTS and 180 seconds for its Files-plus-Interactions STT flow. OpenAI defaults to
45 seconds for TTS and 120 seconds for multipart STT. ElevenLabs TTS defaults to
45 seconds and Deepgram STT to 120 seconds. The complete public workflow
defaults to 180 seconds for TTS and 480 seconds for STT, leaving at least 30
seconds beyond the sum of provider attempt budgets for IAM, Briefcase, audio
normalization, persistence, and response serialization. All values are bounded
typed configuration and the process refuses internally inconsistent totals.

## D-024 — Exact idempotency replay requires a short-lived response cache

**Status:** Accepted; supersedes D-004 and D-013 only for completed-response storage

Exact cross-replica replay cannot be implemented from content-free metadata: an
STT success contains the transcript, while a TTS success contains the two URLs
that were returned to the caller. PostgreSQL therefore retains the normalized
successful response inside the actor-, organization-, operation-, and
key-scoped idempotency record for the configured 24-hour window. It still never
stores request text, source media, generated audio, IAM credentials, provider
credentials, raw provider responses, or provider error bodies. Expired records
are deleted in bounded batches, response fields are never logged, and database
encryption, access controls, backups, and retention must follow the same
classification as Briefcase speech data.

The original request ID is part of recoverable operation identity. Reclaiming
an expired in-progress lease reuses that ID instead of the retrying caller's ID,
so the derived Briefcase upload key and filename remain stable after a crash.
Client-supplied request IDs are correlation values rather than globally unique
idempotency keys, so the database indexes them but does not impose a uniqueness
constraint across independently scoped operations.

## D-025 — Failed-work release preserves recoverable operation identity

**Status:** Accepted

Releasing failed work expires its ownership lease instead of deleting the
pending row. The next same-body caller atomically reclaims that row and its
original request ID until the normal retention boundary. This preserves the
Briefcase filename and downstream idempotency key even when an upload succeeded
but its response was lost. A different-body caller still receives a key-reuse
conflict, and bounded cleanup eventually removes abandoned rows.

## D-026 — Identifier bounds follow their authoritative contracts

**Status:** Accepted

Organization handles remain limited to 64 ASCII identifier characters.
Application IDs and IAM audiences permit up to 80, matching IAM's published
AppId bound; configuration, domain validation, request headers, and OpenAPI use
the same limit. Values are normalized to lowercase and are never truncated.

## D-027 — IAM OBO verification is now published; delegation remains blocked

**Status:** Accepted; supersedes the OBO-verification clause of D-008

IAM's current OpenAPI now includes `/api/v1/obo-access/verify`. Waveform consumes
that proof online and validates issuer application, audience, action,
organization, represented actor, expiry, and consumption state. Bearer tokens
continue to use online introspection. The remaining IAM blocker is a distinct
actor-context exchange that can mint a new Briefcase-audience proof after the
inbound credential has been verified; Waveform still refuses to forward the
Waveform-audience proof or retain/reuse a bearer token for that purpose.

## D-028 — Gemini TTS uses the documented language field

**Status:** Accepted; supersedes the Gemini TTS language transport in D-021

The current Gemini Interactions API documents optional
`generation_config.speech_config.language`. Waveform sends the validated BCP 47
hint through that field only to Gemini and keeps the user's input text
byte-for-byte unchanged. ElevenLabs Multilingual v2 and OpenAI `tts-1` still
receive no language field.

## D-029 — Provider retry bounds are uniform and deadline-aware

**Status:** Accepted; refines D-020

The single transient retry starts with 200 milliseconds of backoff plus 0–100
milliseconds of full additive jitter. A valid delta-seconds or HTTP-date
`Retry-After` is honored, capped at two seconds. The retry is skipped unless at
least 50 milliseconds remains for the second exchange inside the original
provider deadline. These constants are shared across adapters, and the logical
attempt holds only one concurrency permit across both exchanges.

## D-030 — Downstream authority precedes billable work

**Status:** Accepted

After online caller authorization and idempotency ownership, both workflows
obtain a new purpose-bound Briefcase delegation before reading media or calling
a speech provider. TTS therefore fails at the known IAM proof-exchange gap
before incurring synthesis cost. The delegated proof remains request-local,
short-lived, and is never persisted or logged.

## D-031 — TTS capabilities follow the provider that receives the hint

**Status:** Accepted

Because the public `lang` value is sent only to Gemini, the deterministic TTS
capability set is Gemini 3.1 Flash TTS Preview's current documented 78-language
table rather than a fallback provider's language list. Waveform advertises the
documented Mandarin code `cmn`, not an undocumented `zh` alias. Regional and
script variants remain accepted when their canonical primary language appears
in the table.

## D-032 — Public deadlines cover the complete worst-case workflow

**Status:** Accepted; supersedes the public deadline defaults in D-023

TTS now defaults to 300 seconds and STT to 600 seconds. Startup validation
requires the total deadline to cover every provider attempt plus worst-case
codec work, both IAM calls, Briefcase API/download work, two authoritative-store
statements, and 30 seconds of orchestration headroom. This preserves the
documented fallback chain under configured maximums instead of publishing a
deadline that can make the final fallback unreachable.

## D-033 — Operational attempts are telemetry, not transactional data

**Status:** Accepted; supersedes the provider-attempt persistence in D-004 and
the exact field list in D-016

PostgreSQL is authoritative only for idempotency state and the short-lived
normalized response needed for exact replay. Provider attempts are emitted as
content-free structured events containing request ID, operation, provider,
stable outcome category, and elapsed milliseconds. A failed telemetry sink can
therefore never fail or delay a speech operation, and high-volume diagnostic
data cannot expand the correctness database.

Waveform does not log actor IDs, organization IDs, text or transcript sizes, or
URLs. Request middleware supplies method, route, status class, and latency.
Deployment-specific metrics may aggregate the low-cardinality operation,
provider, outcome, and status fields, but no metrics registry is embedded until
the platform defines its scrape or export contract.

## D-034 — Secure Briefcase public origins are the development defaults

**Status:** Accepted

The Briefcase API base URL may use loopback HTTP in development, but permanent
file references and temporary-media origins default independently to
`https://127.0.0.1:8082`. They cannot inherit an HTTP API base URL. This keeps a
minimal valid development configuration compatible with the public HTTPS URL
invariant while production continues to require HTTPS for every dependency.

## D-035 — Ambient HTTP proxies are not trusted for credential-bearing egress

**Status:** Accepted

Provider and IAM clients ignore ambient proxy environment variables. API keys,
application secrets, access tokens, and OBO proofs must not be routed through a
host-level proxy that Waveform did not explicitly configure. Redirects remain
disabled. If a deployment requires an egress proxy, Waveform will add a typed,
validated, explicitly trusted proxy setting rather than inheriting process
environment behavior implicitly; the same rule applies to the future
Briefcase HTTP adapter.

## D-036 — FFmpeg is fixed at the invocation boundary, not the package version

**Status:** Accepted; supersedes the exact-binary packaging clause of D-012

Waveform invokes the configured FFmpeg executable without a shell and with a
fixed argument set, bounded pipes, byte limits, and a deadline. The reference
container installs the security-maintained FFmpeg package from its Debian base
instead of freezing one package revision indefinitely. Deployments identify the
promoted final image by immutable digest; the application validates FFmpeg
availability through readiness rather than depending on a specific major
version string.

## D-037 — Speech-provider retention and model-improvement controls fail closed

**Status:** Accepted

ElevenLabs requests disable `enable_logging` by default so enterprise accounts
use zero-retention mode; deployments may explicitly enable provider history
only through typed configuration. Deepgram requests set `mip_opt_out=true` by
default so source speech is excluded from its Model Improvement Program;
deployments may explicitly change that policy through typed configuration.
Provider rejection of a requested privacy mode is a redacted deterministic
failure and never causes Waveform to resend the same content with weaker
privacy settings.

## D-038 — One retry token spans a complete provider attempt

**Status:** Accepted; clarifies D-020 and D-029

The one transient retry is a logical-attempt budget, not a per-HTTP-exchange
allowance. Multi-exchange Gemini STT shares one token across retry-safe polling
and interaction calls. Resumable upload creation and finalization are not
automatically replayed because they are not safely idempotent without explicit
session reconciliation. Gemini file deletion is best effort, bounded, and
idempotent; failures emit content-free telemetry containing only provider,
request ID, and stable failure category.

## D-039 — Audio byte bounds and duration sources are independent

**Status:** Accepted; clarifies D-012

Provider-artifact input bytes and normalized MP3 output bytes have separate
typed limits. MP3 pass-through enforces both. Headerless PCM must contain a
whole number of interleaved sample frames, and its public duration is computed
from that exact input sample count rather than MP3 encoder frames, which may
include codec delay or padding. Binary MP3 provider responses must have an MP3
content type and valid MPEG audio framing before becoming typed artifacts.

## D-040 — JSON admission covers legal escaped Unicode

**Status:** Accepted

The default JSON body limit is 65,536 bytes. Configuration validation requires
at least twelve bytes per permitted text Unicode scalar plus 1,024 bytes of
envelope headroom, because one non-BMP scalar may be represented as two escaped
UTF-16 surrogate code units. A syntactically legal 4,096-character TTS request
must not be rejected by the transport before domain validation.

## D-041 — Idempotency ownership spans the public request deadline

**Status:** Accepted

The idempotency lease must be at least the larger of the configured TTS and STT
public deadlines. This prevents another replica from reclaiming live ownership
while the original request is still permitted to perform provider, codec,
storage, or cleanup work.

## D-042 — The public idempotency retention minimum is one day

**Status:** Accepted

Completed idempotency records are retained for at least 86,400 seconds and at
most seven days. The default remains exactly one day. Configuration cannot
weaken the public replay guarantee by selecting a shorter terminal lifetime.

## D-043 — Readiness proves the configured MP3 path, not only the executable

**Status:** Accepted; refines D-036

Audio readiness pipes a bounded, generated silence sample through the same
fixed raw-PCM and `libmp3lame` invocation used for normalization, discarding its
output. Merely executing `ffmpeg -version` is insufficient because an installed
FFmpeg build may omit the configured MP3 encoder. The check contains no user or
provider media and remains deadline bounded.

## D-044 — The configured TTS text limit is application policy

**Status:** Accepted

`WAVEFORM_MAX_TEXT_CHARS` may lower, but never raise, the 4,096-scalar domain
ceiling. The application service enforces the configured limit before online
authorization or any idempotency claim, returns the stable payload-too-large
failure, and performs no dependency or billable work. Runtime composition
passes the validated limit into immutable service policy.

## D-045 — Every HTTP response is non-cacheable

**Status:** Accepted; supersedes the five-minute cache policy in D-017

Waveform sets `Cache-Control: no-store` on every success and failure, including
capabilities and operational probes. Speech responses contain transcripts or
signed delivery URLs, while even deterministic capabilities carry a
request-associated `X-Request-ID`; allowing an intermediary to replay any of
those responses would
mix private data or correlation identity across requests. Capabilities remain
compile-time deterministic, but clients must not rely on shared HTTP caching.

## D-046 — Request binding uses a deployment-secret keyed digest

**Status:** Accepted; supersedes the unkeyed SHA-256 clause in D-005

Canonical request fields are length-prefixed and bound with HMAC-SHA-256 under
the dedicated `WAVEFORM_IDEMPOTENCY_DIGEST_KEY`. This preserves constant-size,
constant-time digest comparison without allowing a database reader to test
likely TTS text or permanent URLs offline. The key is required, redacted in all
debug output, copied into zeroizing service policy, and must contain 32 to 1,024
bytes. It is independent from IAM, database, and provider credentials.

The initial schema stores no key identifier, so a deployment must drain the
longest configured idempotency retention window before replacing this key.
Changing it while old records remain fails closed as key-reuse conflicts rather
than replaying data under a mismatched digest. Seamless future rotation requires
an explicit versioned multi-key migration.

## D-047 — Transport time, admission, and telemetry have separate boundaries

**Status:** Accepted; refines D-015, D-018, and D-033

An outer whole-request deadline includes body extraction and response creation,
so a slow body cannot hold an admission permit forever. TTS and STT keep their
full application budgets plus five seconds of transport allowance. Capabilities,
unmatched routes, and readiness are bounded at ten seconds. Public traffic and
readiness use independent non-queueing semaphore budgets; liveness bypasses
both admission and timeout so an overloaded process remains observable.

HTTP telemetry uses custom spans containing only the validated request ID,
method, matched route template, status, and elapsed milliseconds. Raw URIs are
never logged because their query component is caller-controlled and may contain
private references. Request ID and `no-store` finalization wrap timeout,
overload, panic, extraction, routing, and handler responses alike.

## D-048 — IAM application failures and proof time are validated distinctly

**Status:** Accepted

Both IAM authorization endpoints authenticate Waveform with its own HTTP Basic
credential. An upstream `401` therefore represents Waveform dependency
configuration or availability and maps to the dependency-unavailable response,
not to a caller `401`. Bearer invalidity remains the successful introspection
shape with `active=false`; the documented OBO validation statuses remain caller
credential failures.

OBO proof lifetime is checked from IAM's `consumed_at` to `expires_at` interval,
which must be positive and at most 60 seconds. Current validity and a future
consumption timestamp allow at most five seconds of bounded clock skew. This
avoids rejecting a valid short proof merely because IAM's clock is slightly
ahead while still rejecting future-issued, expired, or overlong proof windows.

## D-049 — Database migrations are a startup prerequisite

**Status:** Accepted

Forward-only SQL migrations are embedded in the production binary and applied
after the PostgreSQL pool connects but before the HTTP listener is created.
Migration failure prevents startup and the pool is closed within the configured
shutdown bound. This keeps every serving replica on a schema understood by its
code and uses the migration runner's database coordination rather than an
application-specific schema bootstrap path.

## D-050 — PostgreSQL time and lease tokens fence idempotency ownership

**Status:** Accepted; refines D-004, D-005, D-019, D-025, and D-041

PostgreSQL `clock_timestamp()` is authoritative for record creation, lease
expiry, reclaim, completion, release, retention extension, and cleanup. The
application never submits process-wall-clock values for these transitions. A
database timestamp is sampled after any row-lock or unique-conflict wait so a
new owner receives the full configured lease; `created_at` from the first
successful claim remains the canonical TTS filename time across release and
reclaim. `Retry-After` rounds the database-observed remaining lease upward.

The random lease token is the fencing credential. Lease expiry makes a pending
record reclaimable, but does not by itself invalidate a completion from the
same token: the row lock linearizes late completion against reclaim, and only a
successful reclaim replaces the token. Completion or release from a displaced
owner fails with `LeaseLost`. Cleanup may remove a completed record after its
retention boundary or an abandoned pending record after both record and lease
expiry, but never a still-live lease. SQL statement cancellation and pool
acquisition timeouts map to the safe timeout category; other driver diagnostics
are collapsed to unavailable or invalid-record categories and are not exposed.

## D-051 — Completed replay reauthorizes durable resources

**Status:** Accepted; supersedes the exact-response wording in D-005, D-024,
and D-033, and refines D-013 and D-030

A completed record is durable operation state, not a bearer capability. TTS
records retain the canonical request ID, permanent Briefcase reference,
provider, and duration, but never the expiring temporary URL. After online IAM
authorization on every retry, Waveform obtains a new read-purpose delegation,
atomically checks current access to the exact generated file, and issues a
fresh temporary URL. STT records retain the normalized transcript result, but
Waveform verifies current access to the exact request source before releasing
that cached result. A forward migration removes legacy `temporary_url` fields;
decoders tolerate and discard the old field during data restoration, and a
database constraint prevents a legacy writer from reintroducing it. Deployments
must drain old writers before the startup migration because those writers
cannot replay the durable-only TTS shape.

Replay dependency calls use the current attempt request ID for audit
correlation, while a successful public replay preserves the original operation
request ID and stable result fields. Denial, deletion, timeout, or an unavailable
dependency contract returns an error without deleting the completed record, so
a later authorized retry may still succeed. Replay never invokes a speech
provider, downloads source content, normalizes audio, uploads a file, or mutates
the idempotency record.

Idempotency remains scoped to represented actor, organization, operation, and
key rather than originating application. The product explicitly grants
authenticated applications the actor's non-delete authority; current resource
authorization therefore prevents cross-application replay from granting a new
capability, while preserving deduplication across Bearer and OBO retry paths.
Per-application billing or quota isolation would require a future versioned
caller namespace. A Briefcase `NotFound` result for the exact source or
generated replay file maps to the distinct `404 source_not_found` contract;
actor or resource denial remains `403`. Briefcase rejection of Waveform's own
delegated credential is dependency unavailability, not caller authentication
failure.

## D-052 — Transport request IDs and idempotency lease IDs are separate

**Status:** Accepted; refines D-014 and supersedes the request-ID injection
clause of D-019

The HTTP boundary validates a caller UUID or generates UUIDv7 before extraction,
tracing, admission, and error handling. Application code receives that attempt
ID rather than owning another request-ID generator. On first acquisition it
becomes the canonical operation ID; release and reclaim preserve the stored
canonical ID, and a completed successful replay returns it in both the response
header and body. Replay-only IAM and Briefcase calls use the current attempt ID
as established in D-051; an error from those checks also returns the attempt ID.

Only the unpredictable idempotency lease-token generator is injected into the
application service. Keeping lease identity independent from public correlation
identity prevents a client-controlled request ID from becoming an ownership
credential while retaining deterministic unit tests.

## D-053 — Persisted providers remain operation compatible

**Status:** Accepted

Provider identity in a completed record is validated against the documented
operation chain both before encoding and after decoding. Gemini and OpenAI may
appear in either operation, ElevenLabs only in TTS, and Deepgram only in STT. A
corrupt or manually altered row containing an incompatible provider is an
invalid authoritative record and fails closed; it is never replayed into a
public response that violates the operation-specific OpenAPI enum.

## D-054 — Ephemeral-URL exclusion belongs to the initial schema

**Status:** Accepted; supersedes the legacy-data migration mechanism in D-051

No Waveform application version or database schema has shipped yet. The rule
that a TTS idempotency record cannot contain `temporary_url` is therefore a
validated check constraint in the initial migration, rather than a later
startup migration that scans and rewrites a potentially large retention table.
This enforces the privacy invariant for every first-release deployment without
making startup time depend on row count. The decoder remains defensive against
an extra legacy field in restored or manually supplied JSON, but the production
schema never permits that field to be written. After the first schema release,
all migration history is forward-only and immutable as established in D-049.
