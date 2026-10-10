# API compatibility and version policy

Read `GET /api/contracts`, `waveform contracts --json`, or Rust `client.contracts()` before upgrading a consumer.
Discovery stays reachable when a version is sunset: the client uses the unversioned endpoint for that reason.
`GET /api/v1/contracts` answers too while v1 is supported.

| Consumer | API | Wire protocol | Contract | Credentials |
| --- | --- | --- | --- | --- |
| Waveform Rust client 0.6.x | v1 | 1 (HTTP JSON) | 2.0.0 | Silicon Accounts bearer token or proof |
| Waveform CLI 0.6.x | v1 | 1 (HTTP JSON) | 2.0.0 | Silicon Accounts bearer token or proof |
| The website and other HTTP consumers | v1 | 1 (default) | 2.0.0 | Silicon Accounts bearer token or proof |

Contract 2.0.0 is the move to Silicon Accounts: every route takes `Authorization: Bearer <access token>` (a Silicon
Accounts access token issued to `waveform`) or, on the speech routes, `Authorization: Proof sap_…`; the service has no
sign-in routes; everything belongs to the account that made it, with `?account=` for a Carbon managing a Silicon it
looks after; TTS answers carry `audio_url` and `storage`. Clients before 0.6 cannot sign in against it: the routes they
signed in through answer `410 route_retired` with the replacement in the hint.

## Negotiation

Keep `/api/v1/` in request paths. The client and CLI send `X-Waveform-API-Version: v1` and
`X-Waveform-Protocol-Version: 1`; other consumers may omit both. Responses include the selected API, protocol and
contract versions (`X-Waveform-Contract-Version`). Mismatched path and header versions, duplicate negotiation headers
and unsupported protocols fail explicitly; they never silently select another version.

Additive optional fields and new routes stay within the contract. Preserve existing field names, types, permission
semantics, status codes and defaults. A breaking change raises the contract version and the compatibility matrix in
the same release (as 2.0.0 did); a change that cannot be expressed on the same path needs a new major API path,
matching handlers, a migration registering its contract, and consumer tests. A database entry alone does not
implement a new protocol. The app release version is independent of the contract version.

The September 2026 TTS control update changed the v1 TTS fallback default at the product owner's request: omitted
`auto_fallback` means false. Consumers that need the provider chain send `auto_fallback: true`; STT keeps its
fallback. See the [API guide](api.md) for the optional fields.

## Deprecation and sunset

An operator with access to the host and the database deprecates a version with the `waveform-api` operator command,
which reads the service's own environment:

```sh
waveform-api contracts deprecate v1 --successor v2
```

The successor must already be registered and active, so v1 cannot be deprecated while it is the only implemented
version. Deprecation is retry-safe and does not reset its starting timestamp. Deprecated responses carry a
`Deprecation` timestamp and a link to compatibility discovery.

A deprecated version sunsets after seven consecutive days with zero requests, counted from the later of its
deprecation or its last request, and only while its successor is active. Every admitted versioned request counts,
even when its operation fails. This contract accounting is independent of optional telemetry.

Request admission and the periodic maintenance task persist retirement in PostgreSQL. At the quiet-period boundary,
requests answer `410 api_version_sunset`; incoming traffic cannot revive the version. Discovery stays unversioned and
available.

## Consumer contract checks

The database tests cover header-free consumers, explicit negotiation failures, deprecation without a successor,
recent traffic, seven-day retirement, post-retirement discovery and the operator command. The client and CLI tests
cover response decoding, credentials (bearer and proof), the account selector and the retired routes. Run the
[development guide](development.md)'s ordinary and database test commands for changes to any public handler or
client.
