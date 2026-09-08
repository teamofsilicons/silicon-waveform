# Waveform end-to-end test report — 2026-09-08

## Result

The required backend, Rust client, and CLI features in `UNDERSTANDING.md` are
implemented. The final validation passed **166 regression tests** and **67
distinct assertions against real paired services**. Failures found during the
work were fixed and the affected flows were rerun.

The real run used the compiled Waveform CLI and server, real isolated IAM API
and worker, real Briefcase API and worker, PostgreSQL 16, and MinIO. IAM and
Briefcase testing environments were created through their CLIs; Waveform then
created its environment using that pair. No mock IAM, mock Briefcase, or
fabricated storage URLs were used in this run. Real short-lived codes and
application tokens authenticated both a Carbon and a Silicon.

Only speech generation/transcription used the prescribed testing fixtures.
That substitution is required by `UNDERSTANDING.md`. Provider keys for paid
services were absent. Upstream SDKs were `silicon-iam-client` 1.3.0 and
`briefcase-client` 0.2.0. This is evidence for the local real-service integration,
not a claim about an internet production deployment.

## Requirement coverage

| Requirement | Evidence |
| --- | --- |
| IAM-only SLT login, Carbon and Silicon | Actual IAM `oac_` exchanges through compiled CLI; `me`, refresh, logout, invalidated bearer checks |
| Production/test separation | Test bearer without root, wrong root, production bearer in test plane and wrong organization rejected; forged actor header cannot change IAM-derived job owner |
| Synchronous TTS and STT | Both actor kinds completed real TTS → OBO upload → delegated read → STT round trips |
| Exact testing fixtures | Downloaded MP3 matches packaged bytes exactly; prescribed transcript returned; new TTS names/URLs differ |
| Briefcase storage and URLs | Real SDK upload/list/read, MinIO persistence, authenticated HTTPS content delivery with a trusted local certificate; no public bearer URL |
| Current access on replay | Stable canonical request IDs/URLs/transcripts on replay; real Briefcase deletion blocks both cached TTS and STT results |
| Provider order and fallbacks | Account and per-request preferences exercised; changing database defaults without restart produced the expected observed fallback attempt order; unit tests cover provider requests and complete exhaustion |
| Personal API keys | Set/list/delete through CLI; metadata excludes secrets; regression suite verifies encrypted actor/plane scope and real HTTP adapters receive request-local personal keys |
| Media handling | Real one-second WAV records 1000 ms; malformed uploaded WAV returns 415 and a failed job before a provider succeeds; codec bound/format tests pass |
| Jobs and polling | Actor-scoped history, completed and failed states, duration/first-line/provider/timestamps; CLI wait; database tests cover running jobs, cancellation, lease recovery, concurrency and idempotency |
| Environment creation and ownership | Real Carbon and Silicon creators; creator ID persisted; organization owner can manage Silicon-created environment; new environment initially has no jobs |
| Root key retrieval and rotation | 32 alphanumeric characters, retrieval, old-key denial, UUID cache resolution and fresh login after rotation |
| Cleanup | Jobs, preferences, keys, webhook metadata and idempotency rows removed; environment retained and usable afterward |
| Delete and recovery | Root rejected while deleted; recovery preserves history |
| Inactivity and purge | Isolated database timestamps advanced to 16 and 31 days; normal request-triggered maintenance retired/restored/purged the selected environment and preserved another |
| IAM webhooks | Real worker delivery through an HTTPS tunnel; SDK signature/root verification, durable deduplication, tamper rejection; live revocation does not depend on webhook timing |
| Rust client and CLI | Real flows used CLI built on the client; private scoped sessions, root/UUID selection, configuration and JSON output; client transport/polling regressions pass |
| Hourly updates and opt-out | Scheduling/clock rollback/configuration tests and implementation checks; no new published release was installed |
| Documentation | API/OpenAPI, Rust client, CLI, IAM, and testing guides updated |

## Bugs fixed

The feature-completion work added the missing control-plane and client/CLI
flows, real downstream SDK storage, durable job/idempotency lifecycle,
request-local provider keys, and test-environment support documented in
`IMPLEMENTATION.md`. The final real-service pass additionally found:

1. **SLT prefix mismatch:** login rejected actual IAM `oac_` codes before making
   an IAM request. Validation, OpenAPI and regression fixtures now use IAM's
   actual wire format.
2. **Default IAM scope mismatch:** default `waveform.tts`/`waveform.stt` scopes
   are not issued by IAM's standard application registration. Defaults now use
   `obo.issue`; live identity/audience/org/plane checks still apply. A real
   production-plane request reached the expected `providers_exhausted` boundary
   with no paid keys configured.
3. **STT MIME mismatch:** Briefcase download intent returns opaque bytes.
   Delegated reads now preserve the stored media type, fixing TTS-to-STT
   round trips without weakening codec validation.
4. **CLI JSON inconsistency:** status commands ignored `--json`. Login, refresh,
   logout, key mutations, cleanup and configuration now return a JSON status
   object. The compiled CLI regression parses session command output.

Test-plane media inspection also now uses the production normalizer, so STT
reports the uploaded source duration and rejects malformed media rather than
silently reporting the fixture's duration for every input.

## Regression commands and results

After provisioning PostgreSQL with `scripts/local_database.py` and sourcing its
private environment file:

```sh
cargo build --bin waveform-api
cargo build --manifest-path cli/Cargo.toml
cargo test --workspace
cargo test -- --ignored
cargo test --manifest-path client/Cargo.toml
cargo test --manifest-path cli/Cargo.toml
cargo clippy --all-targets -- -D warnings
cargo clippy --manifest-path client/Cargo.toml --all-targets -- -D warnings
cargo clippy --manifest-path cli/Cargo.toml --all-targets -- -D warnings
cargo fmt --all -- --check
cargo fmt --manifest-path client/Cargo.toml -- --check
cargo fmt --manifest-path cli/Cargo.toml -- --check
git diff --check
```

Results: 140 backend tests, 13 explicitly enabled PostgreSQL/HTTP integration
tests, 8 client tests, 5 CLI tests; all passed. The updated JSON CLI regression
was rerun after its final assertion was added. All three strict Clippy checks,
formatting and diff checks passed.

The regression suite intentionally uses mock transports for deterministic
failure/concurrency cases; it is separate from the real-service evidence above.
The real run's private provisioning state, CLI outputs, and verification journal
remain in the ignored `.local-test/real-e2e-0c0e553d08` directory. It contains
credentials and is not a shareable report. A sanitized assertion inventory is
recorded below. Temporary APIs, workers, the HTTPS tunnel, proxies and the
dedicated Docker containers were stopped after verification; their volumes
and private evidence were retained. Pre-existing services were left running.

## Scope limits

The UI and `waveform report` are explicitly future work in `UNDERSTANDING.md`.
Outbound storage OBO is implemented and verified. Incoming OBO speech is a
separate unsupported authentication mode; the released IAM interface does not
provide the onward subject-token handoff needed for it.

The run does not measure paid-provider speech quality or live availability,
install an actual newer published Waveform release, or deploy the application.
Retention tests advance database timestamps instead of waiting weeks.

## Real-service assertion inventory

- 15-day inactivity retires environment
- 30-day deleted environment purged
- Carbon jobs isolated from Silicon
- STT replay canonical transcript/job
- STT replay denied after real storage deletion
- STT runtime fallback follows database order
- Silicon TTS and STT round trip
- Silicon can create environment
- Silicon creator manages own environment
- Silicon history
- Silicon is recorded as creator
- Silicon logout clears session
- Silicon refresh
- TTS replay canonical job/file
- TTS replay denied after real storage deletion
- TTS runtime fallback follows database order
- UUID test selection
- account STT order fallback completed
- account TTS order fallback completed
- actor authority comes from IAM, forged header cannot impersonate
- actual source duration 1000ms
- capabilities exposed
- clean clears history
- clean removes all scoped state
- clean resets preferences
- clean retains environment
- cleaned environment remains usable
- completed job polling
- deleted environment rejects root
- environment detail
- environment listing
- failed job recorded
- inactive environment can recover
- invalid real uploaded audio rejected
- key removed
- logout invalidates real IAM bearer
- new TTS creates distinct file
- new environment has no jobs
- old root invalidated
- organization owner manages Silicon-created environment
- other environment survives retention
- per-request TTS order fallback completed
- permanent link is not a public bearer capability
- personal key metadata only
- preferences persist
- production CLI session preserved
- production bearer rejected in test plane
- production default IAM scope reaches provider boundary
- provider defaults loaded from DB without restart
- real Silicon identity
- real stored TTS bytes exact
- refresh keeps identity
- request preference validation
- restored history preserved
- returned HTTPS permanent link serves exact audio
- root alone supports environment metadata
- root retrieval
- root rotates to 32 alphanumerics
- rotated UUID resolves current root
- signed IAM webhook replay 0
- signed IAM webhook replay 1
- tampered webhook rejected
- test bearer rejected without root
- test clean guarded without root
- webhook durable deduplication
- wrong organization rejected
- wrong root rejected
