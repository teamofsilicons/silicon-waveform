# API compatibility and version policy

Read `GET /api/contracts`, `waveform contracts --json`, or Rust `client.contracts()`
before upgrading a consumer. Discovery remains reachable when a version is sunset.
The SDK uses the unversioned endpoint so compatibility discovery survives retirement.
`GET /api/v1/contracts` is also available while v1 is supported.

| Consumer | API | Wire protocol | Contract |
| --- | --- | --- | --- |
| Waveform Rust client 0.1.x | v1 | 1 (HTTP JSON) | 1.0.0 |
| Waveform CLI 0.1.x | v1 | 1 (HTTP JSON) | 1.0.0 |
| Existing website / HTTP consumers | v1 | 1 (default) | 1.0.0 |

## Negotiation

Keep `/api/v1/` in request paths. The client and CLI send
`X-Waveform-API-Version: v1` and `X-Waveform-Protocol-Version: 1`. Older consumers
may omit both headers. Responses include the selected API, protocol and contract
versions. Mismatched path/header versions, duplicate negotiation headers and
unsupported protocols fail explicitly; they never silently select another version.

Additive optional fields and new routes stay within v1. Preserve existing field
names, types, permission semantics, status codes and defaults. A breaking change
requires a new major API path, matching handlers, a new migration registering its
contract, and an updated compatibility matrix and consumer tests. A database entry
alone does not implement a new protocol. The app release version is independent
from the API contract version.

## Deprecation and sunset

Operators authenticate with the dedicated service credential and call
`POST /internal/contracts/{version}/deprecate` with `{"successor":"v2"}`. The
successor must already be registered and active; v1 cannot currently be deprecated
because it is the only implemented version. Deprecation is retry-safe and does
not reset its starting timestamp. Deprecated responses include a `Deprecation`
timestamp and a link to compatibility discovery.

A deprecated version sunsets after seven consecutive days with zero production
requests, counted from the later of deprecation or its last production request.
An active successor is required. Admitted versioned requests count even if their
business operation fails. Sandbox requests keep their own usage record and do not
extend production retention. This essential contract accounting is independent
of optional telemetry.

Request admission and periodic maintenance persist retirement in PostgreSQL.
At the quiet-period boundary, requests return `410 api_version_sunset`; incoming
traffic cannot revive that version. Discovery remains unversioned and available.

## Consumer contract checks

Database integration tests exercise an existing header-free IAM discovery consumer,
explicit negotiation failures, deprecation without a successor, recent traffic,
isolated sandbox accounting, seven-day retirement and post-retirement discovery.
The real Rust/CLI and sandbox speech tests cover existing response decoding,
actor permissions, request-bound Briefcase uploads, replay and session isolation.
Run the development guide’s ordinary and database test commands for changes to
any public handler or client.
