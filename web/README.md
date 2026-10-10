# Silicon Waveform website

Next.js 16, React 19 and Arc UI, using the Accounts/Apps theme and workspace shell. The website calls the actual Waveform API through a same-origin server proxy. Tokens stay in sealed HTTP-only cookies; the browser uses hosted Silicon Accounts sign-in with PKCE.

## Product pages

- Speech: text-to-speech and Briefcase speech-to-text, voice profiles, provider order, opt-in TTS fallback, request keys, provider controls, language hints, retry-safe requests, playable/downloadable audio and transcripts.
- History: account selection, operation filtering, paginated requests, running-state refresh and details (the first 512 characters retained by the service).
- Settings: own or custodial Silicon preferences and provider keys, Briefcase authorization/revocation, telemetry and bug reporting. Speaking and approving Briefcase access always use the signed-in account.

## Local setup

Use Node 24+ and the pinned pnpm version in package.json. From the repository root, configure and start `scripts/dev-accounts.sh` using `docs/migration/progress.md`. The backend listens on `4161`. Then:

```sh
cd web
corepack enable
pnpm install --frozen-lockfile
cp .env.example .env.local
# Fill APP_SECRET and SESSION_SECRET for the registered waveform app.
pnpm dev
```

Open `http://127.0.0.1:4160`. Register this exact origin plus `/auth/callback` in Silicon Accounts. Local email codes are delivered to the shared test messaging service. The API must already run; the website does not start a product stub.

| Variable | Purpose |
| --- | --- |
| `APP_ID` | Defaults to `waveform`. |
| `APP_SECRET` | Silicon Accounts app credential, server only. |
| `ACCOUNTS_URL` | Hosted sign-in origin and token issuer. |
| `ACCOUNTS_API_URL` | Optional private Accounts API origin. |
| `APP_API_URL` | Product API origin (`http://127.0.0.1:4161` locally). |
| `SESSION_SECRET` | At least 32 random bytes for sealed cookies; rotation signs browsers out. |
| `PUBLIC_URL` | Website origin; its `/auth/callback` must be registered. |
| `EXTRA_IMG_ORIGINS` | Optional image/media origins for local fixtures or account photos. |
| `EXTRA_ORIGINS` | Optional additional origins of this website for write requests. |

Values are validated at server startup and read at request time. External production origins require HTTPS. Cookies use Secure and the __Host prefix on HTTPS. The proxy forwards only allowlisted headers and refreshes access tokens on the server; state-changing requests require the site's Origin. Browser paths `/api/api/v1/...` forward to the backend's `/api/v1/...`.

## Verification

```sh
pnpm typecheck
pnpm lint
pnpm test
# Real backend plus shared Accounts stack must be running:
TEST_STACK_JSON=/path/to/test-stack.json pnpm test:e2e
NEXT_OUTPUT=standalone pnpm build
```

Playwright starts Next on 4160, signs two fresh Carbons in using the hosted email flow and uses actual API requests. It checks product journeys, public/session flows, refresh concurrency, CSRF, desktop/mobile layout and WCAG 2.2 AA in both themes. Product screenshots go to the ignored `../.mig/screens/`. Provider fixtures prove application behavior, not compatibility with paid external providers. See the migration progress record for exact runs and limits.

## Deployment

Vercel: project root `web`, use the checked-in Next configuration and the environment above. Register production/preview callback URLs before rollout. Self-hosting: `NEXT_OUTPUT=standalone pnpm build`, copy `public` and `.next/static` into `.next/standalone`, then run `server.js` behind the site's TLS proxy.

The release workflow builds `web/Dockerfile` into the existing waveform-frontend image. `deploy/native/install-frontend.py` retains the established port 4325 and installs its runtime secrets. Audio held by Waveform streams through the same-origin proxy; permanent Briefcase audio is loaded directly from its configured media origin.

## Design and provenance

Shared styles and Arc components are under `styles/`, `components/arc/` and `components/foundation/`. Product components are under `components/waveform/`. See `DESIGN.md`, `vendor/uiarc/PROVENANCE.md`, `vendor/uiarc/LICENSE` and the BDO Grotesk font licenses.
