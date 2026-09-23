# Public identifier cutover — 2026-09-23

Waveform 0.4.0 is live at `c90ca5365c922c93693d7a90eb8511f4453af487`. Public readiness passes.

Migration 14 preserved all 15 retained fingerprints/private keys after fresh frozen backup and rollback rehearsal. Cached sessions were decrypted and rebound to the bare app ID with unchanged credentials. Native API and frontend are healthy. Startup config and Secrets Manager audience/OBO scope fields were mapped exactly while preserving credentials. Fresh c:saket login and status succeeded; this read-only acceptance does not claim paid STT/TTS provider execution.

Frozen backup, mapping, migration, session conversion and activation receipts are retained in the protected operator directory `/tmp/consumer-cutover-20260923/`, including final-artifact-verification.json, fresh-cli-auth.json and service-specific SSM receipts. Public health was independently rechecked after all activations. Client/CLI crates are published. The six-platform GitHub v0.4.0 release is public after remote asset SHA256 verification. Honeycomb release `d03026c0-8f9a-4ff0-8f7c-d58c0d2d6aed` is accepted with archive SHA256 `798904e930d0c1e36e7bc6e9eb734662bec10dc4016c52e9a94aba99c874ddb6`. Hosted documentation was rebuilt, published and checked.
