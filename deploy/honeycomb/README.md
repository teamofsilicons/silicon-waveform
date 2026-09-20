# Honeycomb publication

Application: `tos>waveform` (Silicon Waveform). Registration was accepted on
2026-09-16 at configuration revision 1. Live IAM had no existing registration
for this ID, so a new identity was created through Honeycomb as the `tos` owner.
The deployed webhook URL and existing signing key were retained.

`application-metadata.json` records the submitted catalog details and permissions.
It deliberately omits the webhook signing secret and is not a complete creation
request. Registration credentials and private request/response files are excluded
from Git. The one-time application credential is saved under the operator's
protected `~/.config/silicon/waveform/` directory for the later service rollout.

## Current published release

Version **0.2.0** is public as of September 20, 2026. The application remains
active at configuration revision 1; its earlier publication request is approved
and published. The new release did not change permissions or app configuration.

- Release ID: `d47400ab-4ab3-4e59-afef-e569c3946b68`
- Archive size: 24,357,252 bytes
- SHA-256: `7604e2e73b512e526846fdb1c89c9236b1517bd9017fbb3b4f121f26c3f5790a`
- Targets: Linux, Windows and macOS, each on x86_64 and aarch64
- [Validated build artifacts](https://github.com/teamofsilicons/silicon-waveform/actions/runs/35522041395)
- [GitHub release](https://github.com/teamofsilicons/silicon-waveform/releases/tag/v0.2.0)

A fresh signed-out Honeycomb home installed the latest public release with an
alias. Its native macOS ARM64 executable reported `waveform 0.2.0`, exposed the
new controls/BYOK flags, and read the deployed API's compatible contract.

The backend and website are deployed. Both `silicon-waveform-client` and
`waveform-cli` 0.2.0 are published on crates.io. See
[production verification](../../docs/deployment-verification-2026-09-20.md).

```sh
honeycomb install 'tos>waveform'
honeycomb update 'tos>waveform'
```
