# Honeycomb lifecycle participant

Create and manage shared environments in Honeycomb. Waveform’s website, CLI and
API select an environment using its IAM application secret. Ordinary users never
need the lifecycle service credential or manually paired IAM/Briefcase root keys.

## Service configuration

Provision `WAVEFORM_HONEYCOMB_SERVICE_TOKEN` in the deployment secret store (32–512
characters) and register Waveform’s HTTPS origin and that credential reference in
Honeycomb’s `HONEYCOMB_LIFECYCLE_PARTICIPANTS`. The existing AWS installer loads
`WAVEFORM_*` secrets into the service. Keep this service credential separate from
IAM application secrets, test keys and user tokens. An unconfigured participant
returns `503 honeycomb_lifecycle_not_configured`.

Honeycomb calls:

```text
PUT /internal/honeycomb/organizations/{org}/testing-environments/{environment}/operations/{operation}
Authorization: Bearer <dedicated service credential>
```

```json
{
  "operation_id": "11111111-1111-4111-8111-111111111111",
  "environment_id": "22222222-2222-4222-8222-222222222222",
  "org_id": "tos",
  "app_id": "waveform",
  "environment_revision": 1,
  "generation": 1,
  "key_version": 1,
  "action": "prepare",
  "snapshot": {}
}
```

Actions: `prepare`, `rotate-key`, `clean`, `disable`, `restore`, `purge`. The body
also accepts the coordinator’s `testing_key`, `reason` and `retired_apps` fields;
unknown actions fail explicitly. Preparation records the participant binding;
IAM-validated discovery initializes the empty runtime plane and voice defaults.
Key rotation supplies `testing_key` so webhook matching uses the new key digest.
The root key itself is not retained by this participant endpoint.

Receipts echo the operation, environment, application, revision, generation and
key version with `pending`, `completed` or `failed`. `GET` at the same URL retrieves
the durable receipt, including after a runtime-data purge. Retry the exact ID and
payload after failures or timeouts. Reusing an ID with changed content returns
409. Revision must advance; only clean advances the generation and only rotation
advances the key version. Purged environments cannot be restored or recreated.

## Cleanup and access fencing

Before draining active work, Waveform commits a pending barrier that rejects new
test access. Every test operation holds a PostgreSQL shared advisory lock through
its side effects; lifecycle cleanup takes the exclusive lock. This works across
API processes and releases locks when a request is cancelled. Requests that were
already running finish or fail before cleanup completes. A completed cleanup
cannot be followed by an old upload or recreation of cleared records.

Cleanup removes jobs, history, idempotency, preferences, provider credentials,
reports, webhook metadata, test contract usage and test voice configuration.
The shared environment binding and operation receipts survive. Built-in voice
defaults are initialized again on next use; Briefcase removes its own files.
Old webhook events predating the clean are ignored. Temporary speech files follow
the existing request-scoped cleanup path before the request fence is released.

IAM discovery is revalidated inside the fence, checks the application, shared ID,
organization and key version, and uses IAM’s current runtime availability checks.
Disabling blocks new access; restore permits it only after IAM confirms readiness.
Restoring does not undo cleanup. Legacy root-key sessions cannot access a plane
adopted by Honeycomb. The historical public lifecycle URLs return
`409 manage_environment_in_honeycomb`.

## Retention activity

`GET /internal/honeycomb/organizations/{org}/testing-environments/{environment}/activity`
uses the same service credential and reports last accepted discovery activity plus
current lifecycle versions. Reading activity does not extend retention. Waveform
never independently expires or purges an environment. IAM remains responsible
for test identities, authentication and signed webhook delivery.
