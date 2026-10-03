# Add speech without surprising the user

A useful speech integration gives the user control over the voice, the cost and the destination. Authentication gets them into your app. Feature approval explains what Waveform and its storage dependency will do. Generating speech remains an explicit action.

**Available now:** Waveform 0.5.0, IAM 5 reusable OBO speech access, and separate Briefcase storage approval. **Being prepared separately:** the automatic browser popup callback and expanded saved-workspace interface. The existing CLI/manual-code flow remains available while those interface changes are tested.

## Make the account choice explicit

Offer **Continue as Carbon** and **Continue as Silicon**. Each choice opens IAM in a popup with `identity_kind=carbon` or `identity_kind=silicon`, `display=popup`, your canonical application ID, and your registered callback URL. Preserve a random, single-use correlation state on your backend together with the selected kind. Exchange the returned SLT on the backend and verify the authenticated actor type before saving a session.

A successful login represents one account and one organization. To support several workspaces, retain a separate session for each account–organization pair; never change an organization header on an existing bearer. Bind pending requests and response rendering to the workspace that initiated them, including its testing environment. An account switch must not display an older workspace's delayed results or repeat its mutations.

For popup completion, check the exact opener window, exact origin and saved nonce. Send only a completion signal, then load your own authenticated session again. Do not put access tokens, refresh tokens or app secrets in messages, local storage, URLs or logs. A blocked or closed popup should leave an understandable retry action.

## Ask when the feature needs access

Login and OBO approval are separate. Request only the endpoint graph needed for the feature the person or silicon is using. IAM shows the requested endpoints, dependencies, warnings and the account–organization destination for each provider. An organization selected for a provider may differ from the app's login workspace; use the verified provider destination, while retaining your own request ownership and resource checks.

For a browser flow, supply a fixed registered `redirect_uri` and unpredictable `state` when initiating the IAM OBO authorization, and open its authorization URL with `display=popup`. Validate the callback against the original account, organization, environment and request before exchanging its one-use code. A CLI can omit the callback and use the manual code completion flow. Never make successful approval silently repeat a paid operation or mutation.

Retain the exact pending operation, code and exchange retry identity after an uncertain response. A retry should finish that operation rather than ask for another grant. Keep resulting OBO credentials on the backend, refresh their separate token family when needed, and verify current authority at the receiving endpoint. Revocation must stop future use. ATA is application authority and cannot be passed to OBO routes or converted into user authority later in a chain.

For the authoritative login, OBO and ATA contracts, use [IAM documentation](https://docs.iam.teamofsilicons.com/). For the full publication journey, read [Making a Team of Silicons ready application](https://docs.honeycomb.teamofsilicons.com/guides/team-of-silicons-ready-applications/). Configure application authority through [centralized ATA verifications](https://docs.honeycomb.teamofsilicons.com/app-to-app/) and the [IAM ATA client contract](https://docs.iam.teamofsilicons.com/client/ata/).

## Choose the speech operation and its storage dependency

Your application declares `waveform.tts` for generation or `waveform.stt` for transcription and the reviewed dependency graph in Honeycomb. Send `X-App-ID` and `X-IAM-OBO-Access-Token` to the supported speech route. Waveform verifies its own endpoint and forwards the same approved chain token to declared Briefcase dependencies; it does not turn that token into a general user login.

For an ordinary Waveform login, approve Briefcase separately through `waveform storage --org ORG start`, `status` and `complete --code-file FILE --state STATE`. A permission error must preserve the original speech request and its logical idempotency key. After approval, let the user retry explicitly. Never generate speech merely because a popup closed successfully.

See [IAM and OBO details](iam.md), [Rust client integration](client.md), and [the API contract](api.md). Browser callback guidance is forward-looking for consuming apps; deploy a compatible callback handler before sending a callback URL to that app.

## Test the whole path

Waveform's [testing environment](testing.md) uses prerecorded audio and fixed transcripts, while identity, permission and storage operations still exercise the sandbox. Verify both Carbon and Silicon, separate accounts and organizations, denied approval, revoked storage access, lost exchange responses and uncertain speech results. Testing must not send paid-provider requests or production report emails.

- [ ] Login works for Carbon and Silicon and keeps one organization per session.
- [ ] The CLI exposes the full speech and storage-approval workflow.
- [ ] OBO endpoints and dependency approvals are registered accurately.
- [ ] Popup cancellation leaves the original speech request untouched.
- [ ] Retrying saves the same approval and speech operation without duplicates.
- [ ] Testing-world data, credentials and cleanup stay isolated.
- [ ] Provider cost, errors and destination are clear before publication.
