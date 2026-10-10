# Waveform documentation site

Run `npm ci && npm run build && npm run check`. Canonical Markdown lives in `../docs`; `docs/history/` and
`docs/migration/` are not published. The build also publishes `openapi.yaml`, `llms.txt`, `llms-full.txt` and the
sitemap. Deploy `dist` to the dedicated Vercel project `silicon-waveform-docs`, with `docs.waveform.teamofsilicons.com`
attached ([development guide](../docs/development.md#publishing-docs)).
