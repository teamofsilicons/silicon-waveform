# Waveform PR 4 deployment and republication

Completed 2026-09-14T18:53:16.695340+00:00.

- PR [#4](https://github.com/teamofsilicons/silicon-waveform/pull/4) merged as `8bd07049565503313264d191bce98cd1acbb4f6f`.
- Production image: `234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-waveform-production@sha256:a538a3417e139b7c384189ef916ffc3983fc985ce03b2004aa1177b02cefda13`; runtime source `b255236` preserves the previously deployed app discovery, telemetry and testing features.
- Runtime rollout SSM: `7733f8b0-c8da-48d8-9a26-581aaa4c28e1`. Backup completed before replacement. Readiness passed and final container restart count is zero.
- Rust client [0.1.1](https://crates.io/crates/silicon-waveform-client/0.1.1) and CLI [0.1.2](https://crates.io/crates/waveform-cli/0.1.2) published and confirmed available from the registry. Cargo verified both packaged crates before upload.
- [Documentation](https://docs.waveform.teamofsilicons.com) and downloadable installer source republished. All 19 pages pass local link/navigation/canonical checks. Public archive checksum matches the exact local source bundle; it contains CLI 0.1.2 and client 0.1.1.
- Release source pushed to `codex/waveform-release-20260915`; package source commit `7c56dfb`. Original working checkout preserved except for its deployment record.

Validation: 153 backend tests, two fixed Briefcase catalog checks, seven real PostgreSQL control tests, three speech/CLI/PostgreSQL scenarios, eight transaction tests, ten Rust client tests and fourteen CLI tests passed. Strict Clippy passed for backend, client and CLI. Older fixtures were corrected to provide valid session receipt keys and the now-required CLI test-context discovery response; authorization assertions remain intact.

The speech suite exercises Carbon/Silicon delegated transcription, replay reauthorization and discovered TTS using deterministic providers and local HTTP fixtures. Public production readiness/discovery and docs were verified. No fresh authenticated hosted speech operation was performed in this rollout.

PR #3 remains open; it is a separate test-harness change.
