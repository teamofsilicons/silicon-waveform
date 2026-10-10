# Transcription source staging verification

The source-target addition retains the existing application namespace fence.
Interface must read an original as its owner, copy the bounded bytes through
ordinary Briefcase uploads into the verified target, and transcribe that copy.
The new target endpoint performs no provider request and returns no credential.

The final PR preserves release ancestry `b8de32b` (including the deployed
`b255236` runtime), rather than dropping imported-app discovery, current
Briefcase test-secret discovery, reports/telemetry, or published client/CLI
changes when merging back to main. Reconciliation keeps current test-secret
semantics: a testing delegation must return its verified Briefcase selector;
a production delegation must not unexpectedly select a test plane.

Local checks: 156 ordinary Rust backend/contract tests, strict all-target
Clippy, formatting, dependency policy, and OpenAPI parsing passed. The new
HTTP/PostgreSQL matrix uses disposable PostgreSQL17.6 and actual IAM/Briefcase
SDK transports with loopback fixtures. It covers production, legacy-root and
imported-app test selection for Carbon and Silicon; same data organization
distinct from the application owner; fresh exact single-use signed list proofs;
current scope/identity/org/audience/world denial; private folder owner/type/app/
path/access/deletion verification; and absent/unexpected test-selector rejection.
Four malformed-header/body cases reject before upstream access. CI includes
the new ignored database matrix explicitly.

Interface commit `e4fef41` contains the matching staging gateway and ten focused
backend tests (retry identity, fresh reads, forged receipts, stale worlds and
stream cleanup), plus TypeScript verification. No paid provider, production
record, or deployment was invoked for these checks. Hosted transcription after
rollout remains a separate manual acceptance step.
