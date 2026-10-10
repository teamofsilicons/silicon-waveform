# Waveform history

Dated records of Waveform releases before version 0.6.0, kept as they were written. They describe the sign-in,
organization, test-environment and distribution systems Waveform used then, which 0.6.0 replaced with Silicon
Accounts and Silicon Apps. None of their instructions apply to the current Waveform: for that, start at
[the documentation](../README.md). The docs site does not publish this directory, and the CLI does not bundle it.

The move itself is recorded in [`docs/migration/`](../migration/): decisions, progress and the cutover runbook.

| Record | What it is |
| --- | --- |
| [implementation-2026-10-03.md](implementation-2026-10-03.md) | Implementation and verification status as of the 0.5 line |
| [releases-0.2-0.5.md](releases-0.2-0.5.md) | The release procedure and release notes of 0.2.0 to 0.5.x |
| [iam.md](iam.md) | The integration guide for the earlier sign-in system |
| [honeycomb-lifecycle.md](honeycomb-lifecycle.md) | The test-environment lifecycle participant protocol |
| [frontend-iam5-contexts.md](frontend-iam5-contexts.md) | The earlier website's account and context handling |
| [public-identifier-migration.md](public-identifier-migration.md) | The 0.4.0 move to `c:`/`si:` public identifiers |
| [test-report-2026-09-08.md](test-report-2026-09-08.md), [frontend-verification-2026-09-08.md](frontend-verification-2026-09-08.md), [demo-audio-verification-2026-09-08.md](demo-audio-verification-2026-09-08.md), [voice-profile-verification-2026-09-08.md](voice-profile-verification-2026-09-08.md) | First release verification |
| [deployment-verification-2026-09-08.md](deployment-verification-2026-09-08.md) to [deployment-verification-2026-09-21.md](deployment-verification-2026-09-21.md), [session-retention-deployment-2026-09-22.md](session-retention-deployment-2026-09-22.md) | Production deployments of September 2026 |
| [verification-2026-09-13.md](verification-2026-09-13.md) to [verification-2026-09-16.md](verification-2026-09-16.md), [briefcase-compatibility-2026-09-11.md](briefcase-compatibility-2026-09-11.md), [e2e-provider-controls-2026-09-20.md](e2e-provider-controls-2026-09-20.md) | Feature verification runs |
| [deploy/](deploy/) | Host records: the first AWS activation, the 2026-09-15 release, the 2026-09-23 identity cutover |
