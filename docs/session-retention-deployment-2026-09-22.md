# Session retention deployment — September 22, 2026

Waveform backend 0.3.1, frontend and CLI 0.3.2 are released from
`0435ac68cd87b31cf4ad99a687de589acc94675e`. Rust client 0.3.0 is unchanged.
The release includes `00836c9` CLI recovery, `bb2dd78` backend error classification
and `153f266` durable browser sessions.

## Published artifacts

[GitHub v0.3.2](https://github.com/teamofsilicons/silicon-waveform/releases/tag/v0.3.2)
and Honeycomb expose the same validated six-platform archive, 24,594,198 bytes:
`d5ed140c75f1243f3c70b11ae05d99c6dc501031d9c01e53c300669663b6324e`.
Honeycomb release `08d5d4f6-8a6a-4668-a720-b1df0357369c` is accepted on the
production channel. The `waveform-cli 0.3.2` crate is published and not yanked.

All six native builds/tests and package validation passed in
[release CI](https://github.com/teamofsilicons/silicon-waveform/actions/runs/35662508218).
The published archive was locally packed using the fixed official Honeycomb 0.3.1
operator. The CI job's older pinned packager produces different archive metadata
(SHA-256 `fe2949aef596931707dd530b7deab43d30881c2fa1bce6e3bc241bc3feb0f45a`);
all seven file payload hashes match exactly between those archives. Both Linux
artifacts also passed live discovery on Amazon Linux 2023 in the
[baseline smoke](https://github.com/teamofsilicons/silicon-waveform/actions/runs/35664258947).

A fresh anonymous installation downloaded the public release. Its native macOS
ARM executable reports `waveform 0.3.2`, exactly matches the CI executable hash
`9f0c8c499104082fceff0058d732708abda257d572490c5ce0239e4639157378`, and passes
live IAM discovery. Running that binary against Maharaj's existing private home
refreshed the saved `chef:bricks` login without a new sign-in. Its two history jobs,
identity, preferences and provider-key listing matched the pre-release snapshot.
The managed Maharaj installation was left for the coordinated ecosystem update.

## Runtime deployment

The existing shared host is `i-0a20224cc619a1bc9` in `us-east-1`.
The native API bundle SHA-256 is
`54ee3a8a580cea9e887991ba6a57941f68e85ad4e2ee2d6d9d0b90b4294ac20e`;
the exact bundle and checksum are attached to the GitHub release. SSM
`e7f08269-8148-4fa3-800a-3b9fc58a50b1` installed it using the normal native
upgrade script with verified paused-writer backup and rollback support.
The active native path is
`/opt/waveform/releases/0435ac68cd87b31cf4ad99a687de589acc94675e`.
The previous release is retained. The database backup is
`s3://silicon-waveform-standalone-backups-c9ap0zqahvyr/backups/upgrade-20260921T223328Z-0435ac68cd87.dump`.

Frontend SSM `7597e464-ece5-490a-a9e3-50efb27dc120` succeeded using the dedicated
native-host frontend installer. Its image is
`234951665042.dkr.ecr.us-east-1.amazonaws.com/silicon-waveform-production@sha256:9ed21c3d4e8e75038fc091963d0545f936884679bdf873a260a04de6cc6d94bf`.
The exported image archive hash is
`014cb775ef3e11d085094f012ffbbc960e62340e93e00a50a09e436806260d23`.
Its architecture and embedded source revision were verified before ECR upload.

`/var/lib/waveform/frontend-sessions` is owned by `1000:1000`, mode `0700`, and
mounted at `/var/lib/waveform/sessions`. Preserve its encryption key and session
store together. Both the Caddy configuration and native environment retained
their exact pre-release hashes. PostgreSQL and Caddy remained running; no new
infrastructure was created. Temporary transfer objects, downloaded host archive
and the isolated install's temporary update worker were removed.

## Live verification

Readiness, liveness and capabilities returned 200; anonymous identity and properly
scoped history requests returned 401. The frontend and its public manifest are
available and identify CLI package 0.3.2.

A normal fresh `chef:bricks` browser login selected `bricks` and the existing
configured permissions. Identity and both history jobs were available. Explicit
browser refresh succeeded, then frontend restart
`e673b24a-0c0a-48fa-b1fb-72aadd706fbe` preserved the same private browser cookie,
identity and exact job responses. The durable mount and encryption key survived.
The independent verification browser family was logged out normally; the next
session response was unauthenticated. No speech generation or account-settings
mutation was used for this verification.

The old frontend kept sessions only in memory, so the first upgrade requires one
fresh sign-in. Subsequent restarts retain the encrypted regular sessions. Local
regressions and CI additionally cover temporary failures, concurrent refresh,
request-start accounting, save failures, isolated test contexts and logout races.
[Source CI](https://github.com/teamofsilicons/silicon-waveform/actions/runs/35662505681)
and [native/frontend CI](https://github.com/teamofsilicons/silicon-waveform/actions/runs/35662508877)
passed.

## Documentation

Documentation source `bea96db` was built from an exact Git archive. All 28 pages,
local links, navigation anchors and canonical URLs passed checks. Vercel deployment
`silicon-waveform-docs-czqfhdhn7-saketdev12-5675s-projects.vercel.app` is aliased to
`https://docs.waveform.teamofsilicons.com`. Public release notes and the 0.3.2
manifest were verified. The downloadable source archive matches SHA-256
`3fa81cb6f73768a047e9f1d9762c454d412e5fb952e34b993d93c6db00e4f9b6`.

## Final CLI follow-up: 0.3.3

Source `c475f0e771fe7cf1de69f4df727313e5f71fd35d` adds saved-access validation and
locked recovery on early authentication rejection. Backend 0.3.1, frontend 0.3.2
and client 0.3.0 remain unchanged. All 23 CLI tests and Clippy passed, including
future-expiry 401 recovery and credential retention after a 503. Source CI and
[all six native targets plus package validation](https://github.com/teamofsilicons/silicon-waveform/actions/runs/35666095608)
passed. Both Linux payloads are static musl binaries without a dynamic loader or
GLIBC symbol dependency.

[GitHub v0.3.3](https://github.com/teamofsilicons/silicon-waveform/releases/tag/v0.3.3)
contains four assets, all anonymously downloaded and matched against the release
artifacts. The official Honeycomb 0.3.2 operator packed the exact six CI binaries.
The 24,653,631-byte catalog archive has SHA-256
`28efc10aa8c36b5db5c27ee6452f2a4f419eb81f36995c5e39e34e3ece696a47`.
Honeycomb release `ba361498-2384-4c29-a3ff-71695613fb3e` is accepted and public.
The `waveform-cli 0.3.3` crate is published; downloaded bytes match SHA-256
`9eef7bc2dd5cfb8f4d31bee26168f498d03e4498ce4f1855e8db29967c0b6d24`.

A fresh anonymous install executed 0.3.3 and correctly reported signed out. Its
native macOS ARM binary hash is
`e7ea7d586df20d196a413ac65b468df151c0578f10cc09770e54caceb6bf2df3`.
Maharaj's supported package update from 0.3.2 to 0.3.3 completed. The earlier saved
login and two-job retention proof is recorded above; the final 0.3.3 retained-login
recheck timed out because macOS file opens under Documents block before state can
be read. No saved credentials were deleted or replaced to work around this.

The 29-page documentation build and link checks passed. Vercel deployment
`silicon-waveform-docs-c5batuyhg-saketdev12-5675s-projects.vercel.app` is aliased to
`https://docs.waveform.teamofsilicons.com`. Public release notes and manifest both
returned 200 and identified 0.3.3. This appendix was reconstructed in a clean
checkout while the original local documentation commit was inaccessible; reconcile
the original checkout after macOS file access recovers.
