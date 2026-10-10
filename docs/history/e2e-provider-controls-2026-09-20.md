# Provider controls and BYOK E2E verification — September 20, 2026

The local end-to-end run passed **32 automated HTTP/CLI checks** and **13 browser
workflow checks**. The production frontend build and all 23 frontend tests also
passed after the result-label correction described below.

## Tested boundary

The browser used the built SolidJS site and real Node gateway, connected to the
actual `waveform-api` executable, a fresh native PostgreSQL database and FFmpeg.
The actual IAM/Briefcase clients and speech-provider HTTP adapters ran against
loopback mock upstreams. IAM proofs were bound to exact upload bytes; generated
audio was normalized, uploaded and recorded in the database. This exercised the
ordinary provider path, rather than the sandbox's prerecorded-provider shortcut.

All credentials were synthetic. No production services, paid synthesis or real
provider credentials were used. These tests prove local application behavior,
not acceptance by live providers or a deployed release. The reserved Briefcase
fixture URL is not publicly hosted; its stored audio bytes were verified locally.

## Browser results

| Flow | Result |
| --- | --- |
| IAM redirect login | Authenticated through gateway and actual API |
| Fresh TTS form | Automatic fallback off, including after reload |
| Gemini controls | Voice, scene, audio profile, director notes and sample context arrived at the provider adapter |
| Rejected Gemini request key | Clear provider/401/reason; one call, no fallback or shared-key retry |
| Explicit TTS fallback | Gemini failure followed by ElevenLabs success; provider-specific controls omitted |
| Provider switch | Request key cleared; other-provider controls excluded |
| ElevenLabs controls | Voice/model, zero stability/style/seed, false speaker boost, speed, context and normalization preserved |
| OpenAI controls | Explicit model, coral voice, speed and delivery instructions preserved |
| Request key lifecycle | Fields cleared immediately after submission |
| STT fallback | Read the generated Briefcase file; Gemini failed and OpenAI completed transcription |
| Saved BYOK | Settings saved the encrypted key; subsequent generation selected it automatically |
| Job history | Completed and failed requests appeared with persisted status |
| Result metadata | Rebuilt UI correctly says “Voice profile” for the base profile |

The only product issue found was the result label “Voice”, which could be
mistaken for the effective provider voice after an override. It now says
“Voice profile”; the rebuilt browser flow passed.

## Automated results

The repeatable HTTP/CLI suite verifies early control validation without provider
calls or job creation, request-size rejection, request → saved → shared key
precedence, encrypted saved keys, no plaintext credentials in job/key-list/database
output, exact control forwarding, authorized replay without a second generation,
409 conflicts for changed controls or keys, safe provider failures, opt-in TTS
fallback and unchanged STT fallback. The real CLI uses an isolated `SILICON_HOME`
and private key/options files; its success, replay and failure paths passed.

Evidence from this run is retained in
`.local-test/tts-e2e-k6jchkjr/http-e2e-report.json`,
`browser-e2e-report.json`, `browser-provider-events.json` and `events.json`.
These contain synthetic test data only and are excluded from Git.

## Repeat locally

Prerequisites: Python 3, Node 22.12+, PostgreSQL 16 at the Homebrew path used by
the harness, and the repository's normal Rust/FFmpeg build dependencies. Use
`DEVELOPER_DIR=/Library/Developer/CommandLineTools` where the system Xcode
selection has an unaccepted license.

Build from the repository root:

```sh
cargo build --locked --bin waveform-api
cargo build --locked --manifest-path cli/Cargo.toml
npm --prefix frontend run build
```

Start the backend and mock upstreams in one terminal:

```sh
python3 scripts/e2e-local-stack.py
```

Start the actual frontend in another terminal:

```sh
HOST=127.0.0.1 PORT=4381 \
WAVEFORM_FRONTEND_ORIGIN=http://localhost:4381 \
WAVEFORM_BACKEND_URL=http://127.0.0.1:4382 \
WAVEFORM_IAM_AUTH_ORIGIN=http://127.0.0.1:4383 \
npm --prefix frontend start
```

Use the state directory printed by the backend harness:

```sh
python3 scripts/e2e-provider-controls-http.py \
  --state-dir .local-test/tts-e2e-REPLACE_WITH_PRINTED_ID \
  --report .local-test/provider-controls-e2e-report.json \
  --cli cli/target/debug/waveform
```

For browser verification, open `http://localhost:4381` and choose Continue with
IAM. The mock redirects through the real callback. Dummy provider keys start
with `e2e-`; suffix `-success` forces success and `-fail-invalid_api_key` or
`-fail-rate_limited` forces the corresponding provider failure. Ctrl-C stops
the harness API and its task-owned PostgreSQL instance; stop the frontend too.
