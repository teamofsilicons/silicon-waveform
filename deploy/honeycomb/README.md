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

## Uploaded release

- Version: `0.1.2`
- Release ID: `bf76ad0d-be7a-4eda-aba0-8fa20a4a92b8`
- Archive size: 23,733,190 bytes
- SHA-256: `29c5c3fad9ac747ef6f1e2a1e0ace0a42c535a87f1cf95d722c1d4a516ec906d`
- Targets: Linux, Windows and macOS, each on x86_64 and aarch64
- [Validated build artifacts](https://github.com/teamofsilicons/silicon-waveform/actions/runs/35083867470)

## Publication status at submission

Request `66efb189-0838-45d4-87b3-322518392f46`, revision 1, is
`awaiting_validator`. The sole remaining gate is Honeycomb validation. The current
operator is a `tos` organization owner, but does not have Honeycomb validator
authority. Upload and submission do not mean the application is public.

An authorized validator can review the request in the
[Honeycomb console](https://console.honeycomb.teamofsilicons.com/). Honeycomb's
coordinator activates approved requests and reconciles archive visibility using
the authorizing manager's session; expired authorization may require the manager
to sign in again and open Sent requests.

Read current state before any retry:

```sh
honeycomb publication get 'tos>waveform' --json
honeycomb releases list 'tos>waveform' --json
```

Production service rollout remains on hold. The newly registered application
credential must be configured during that rollout; hosted runtime readiness has
not been established by package publication.
