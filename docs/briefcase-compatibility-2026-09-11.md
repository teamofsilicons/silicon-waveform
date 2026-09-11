# Briefcase upload compatibility

Waveform used briefcase-client 0.2.0, whose connection check required the removed request-access operations. Briefcase returned HTTP 200 for discovery, but the client rejected the operation catalog before sending the delegated upload. Waveform surfaced this as dependency_unavailable (503).

Upgrade to the published briefcase-client 0.3.0, which matches the current Briefcase contract. IAM authorization, same-organization delegation, and exact-byte proof binding remain in place. Local validation passed all 149 enabled library tests (seven database-dependent tests ignored), the official upload contract test, and Clippy with warnings denied.
