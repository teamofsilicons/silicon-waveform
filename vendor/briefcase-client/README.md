# briefcase-client

The official Rust client for [Silicon Briefcase][service], the
organization-scoped file service used by Carbons, Silicons, and IAM-authorized
applications.

Everything the service exposes to a client, and nothing it does internally.
Application behavior remains stateless: it holds no login session or API
cache — a `Config` goes in and a `Client` comes out. API calls never query a
package registry, run Cargo, or change the consuming project's lockfile.
Update the dependency explicitly and rebuild. `with_auto_update` and
`with_update_manifest` remain compatibility no-ops; `update_status()` always
reports `Disabled`. Honeycomb manages CLI installation and updates.

```toml
[dependencies]
briefcase-client = "2.0.0"
```

```rust
use briefcase_client::{Client, Config, Destination, ListEntries, Upload};

let client = Client::connect(
    Config::new("https://backend.briefcase.teamofsilicons.com/api/v1/", "tos")?
        .with_token(token),
)
.await?;

for entry in client.list_entries(&ListEntries::default()).await?.items {
    println!("{}", entry.path);
}

let stored = client
    .upload(&Upload::file(Destination::path("private/cos:tos/notes"), "./report.pdf")?)
    .await?;
```

`connect` verifies the service identity, selected/supported API major, and the
exact ID/version/method/path of every operation this client calls before the
first real call. Unknown operation IDs are additive; duplicate IDs are
refused. An incompatible pairing therefore fails at startup rather than
mid-request.

`Config::new` accepts exactly the compiled `/api/v1/` base and requires HTTPS,
with clear-text HTTP limited to `localhost` and loopback IPs for local tests.
The version response header and body must select the same API major.

IAM login uses `Client::login_with_slt`. For the default all-organizations flow,
create the anonymous client with `Config::for_sign_in(base_url)`. It accepts an
unscoped session, and `SessionTokens::organizations` contains the IAM handles
currently reachable by that token. After the caller chooses one, build a file
client with `Config::for_sign_in(base_url)?.with_organization(handle)?` and
attach the same access token. This switches request context without minting a
new login. A `Config::new(base_url, org)` login remains available when the
caller deliberately wants IAM to bind the token to one organization. The IAM
Application secret stays on the Briefcase backend. Testing environments use a
typed IAM app-secret `EnvironmentKey` in `Config::with_environment`,
independently of the bearer credential. Production-only management methods
create, inspect, re-pair, rotate, clean, retire, and restore those planes. Every
environment mutation has a caller-key `_with_key` variant for safely replaying
an unchanged request after an uncertain result; upload, entry update, and
version restore accept caller-owned keys too. Persist that `IdempotencyKey`
before the first attempt.

## Delegated operations

The SDK exposes `create_folder_on_behalf_of`, `list_entries_on_behalf_of`,
`read_file_on_behalf_of`, and `trash_entry_on_behalf_of`. Each requires the
calling application's canonical `ApplicationId`, a fresh `OboProof` supplied
by that caller, and an immutable `DelegatedManifest` of the matching request
type. The SDK never mints a proof, retries one, sends its configured bearer
alongside it, or starts package maintenance during these calls.

Prepare the manifest before asking IAM for a proof:

```rust
use briefcase_client::{ApplicationId, DelegatedCreateFolder, OboProof};

let manifest = DelegatedCreateFolder {
    operation_id, // a caller-generated non-nil UUID, retained for logical retries
    parent_path: String::new(), // the represented member's private app folder
    name: "reports".into(),
}.prepare()?;

// Ask IAM using the caller application's own credentials and delegated authority.
// Bind manifest.endpoint_id(), manifest.method(), manifest.path(), and
// manifest.body_sha256(); the endpoint metadata must be the empty object {}.
// manifest.body_bytes() is exactly what this SDK will send, not a re-serialization.
let proof = OboProof::new(fresh_proof_from_iam)?;
let folder = client.create_folder_on_behalf_of(
    &ApplicationId::new("notes")?, proof, &manifest,
).await?;
```

The other strict JSON DTOs are `DelegatedListEntries` (parent, filter, cursor,
limit), `DelegatedReadFile` (file UUID, optional range, download disposition),
and `DelegatedTrashEntry` (stable operation UUID, entry UUID). Each has
`prepare()` and the same binding accessors. Read results use `ContentStream`;
range and disposition are bound inside the manifest, not unbound headers or
query parameters. Listing each new page requires a new manifest and proof.
After an uncertain mutation result, retain the exact manifest and operation
UUID but mint a fresh proof before retrying. `OboProof` is consumed per call,
redacted in debug output, and cannot be cloned or serialized. Fixed binding
path and IAM endpoint constants are also available in the `delegated` module.
The existing raw-byte `create_file_on_behalf_of` API is unchanged.

For a long or recoverable upload, use `DelegatedReserveUpload::file` to hash the
file before minting a proof, then call `reserve_delegated_upload`. Transfer with
the returned narrow `UploadCapability` using `transfer_delegated_upload`.
Prepare `DelegatedCommitUpload` and obtain a new proof for
`commit_delegated_upload`; the private transfer alone never publishes a file.
`DelegatedUploadQuery` and `DelegatedCancelUpload` support reconciliation and
cancellation. Retain the logical operation UUID and manifest, not an IAM proof
or parent credential, in your outbox. See the [staged-upload guide][staging].

Full guide: [Rust client guide][guide]. The `briefcase` command-line client is
built on this package and lives in the same repository.

[service]: https://briefcase.teamofsilicons.com
[guide]: https://github.com/teamofsilicons/silicon-briefcase/blob/main/docs/client/README.md
[staging]: https://github.com/teamofsilicons/silicon-briefcase/blob/main/docs/api/delegated-uploads.md

In a paired test environment, the SLT can be an IAM-issued test login code or an existing Carbon ID (e.g. `alice`)
or Silicon ID (e.g. `worker:tos`). Configure the test app secret and pass that
ID to `login_with_slt`, or use `briefcase --test <environment-id> login <actor-id>`.
IAM issues the test session and determines its current access. Production
continues to require a one-time IAM login code.
