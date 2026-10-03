# Honeycomb publication

Application: `tos>waveform` (Silicon Waveform). Registration was accepted on
2026-09-16 at configuration revision 1. Live IAM had no existing registration
for this ID, so a new identity was created through Honeycomb as the `tos` owner.
The deployed webhook URL and existing signing key were retained.

The initial catalog details and permissions were recorded in
`application-metadata.json`; its current contents are the IAM 5 candidate below.
It deliberately omits the webhook signing secret and is not a complete creation
request. Registration credentials and private request/response files are excluded
from Git. The one-time application credential is saved under the operator's
protected `~/.config/silicon/waveform/` directory for the later service rollout.

## IAM 5 candidate catalog

`application-metadata.json` now describes the prepared Waveform 0.5.0 catalog,
using canonical application ID `waveform` (`org_id` remains `tos`). It has not
been submitted by this change. The September publication record below is historical.

The external scope contains exactly the four Briefcase roots used by the current
storage broker: `briefcase.uploads.reserve`, `briefcase.uploads.commit`,
`briefcase.entries.list`, and `briefcase.files.read`, with canonical app ID
`briefcase`. Ordinary IAM scopes are unchanged. The retired raw upload endpoint
is absent. Feature permission is separate from ordinary login.

The receiving endpoints `waveform.tts` (`/api/v1/tts`) and `waveform.stt`
(`/api/v1/stt`) declare their existing downstream edges. TTS needs reserve,
commit and list; STT needs list and read. Do not replace `obo_endpoints` with an
empty array: an IAM application update would retire the receiving endpoints.

Before applying the catalog, register the exact Briefcase endpoint paths from
[the integration guide](../../docs/iam.md#endpoint-configuration-and-uploads),
resolve provider visibility/approval, and validate both production and testing
graphs against IAM 5. Keep private credentials outside this file. The metadata is
not a complete application creation request or an assertion of live registration.

Run `python3 scripts/check-iam5-catalog.py` to check this declaration against the
runtime storage roots, receiver paths, and documented downstream edges.

## September published release

Version **0.3.1** is public as of September 21, 2026. The application remains
active and public at configuration revision 1. This release preserves its
permissions and application configuration.

- Release ID: `74f98b5d-9cbc-42f3-82d0-70542eb79b38`
- Archive size: 24,578,414 bytes
- SHA-256: `04e5e4db376a16b4b6f10fdc8a61628518b7be87b0b2a16c3561dec9b3545143`
- Targets: Linux, Windows and macOS, each on x86_64 and aarch64
- [Validated builds](https://github.com/teamofsilicons/silicon-waveform/actions/runs/35581460319)
- [GitHub release](https://github.com/teamofsilicons/silicon-waveform/releases/tag/v0.3.1)

Both Linux targets are static musl binaries. They were executed on Amazon Linux
2023 with GLIBC 2.34 before publication. Use 0.3.1 instead of the GNU Linux CLI
binaries in 0.3.0, which require GLIBC 2.39. The native backend remains 0.3.0 and
was already static.

A fresh signed-out Honeycomb home installed 0.3.1 successfully. Maharaj's existing
installation was updated without a new login. Rust client 0.3.0 and CLI 0.3.1
are published on crates.io. See [production verification](../../docs/deployment-verification-2026-09-21.md).

```sh
honeycomb install 'tos>waveform'
honeycomb update 'tos>waveform'
```
