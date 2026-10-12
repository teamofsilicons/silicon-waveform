/** The public files built from lib/app.config.ts: robots.txt, the sitemap and the llms.txt stub. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { llmsTxt, robotsTxt, sitemapXml } from "../lib/agent/files";
import { markSvg } from "../lib/brand-svg";

const ORIGIN = "https://waveform.example";

test("robots.txt opens the public pages and keeps the workspace, sign-in and the proxy out", () => {
  const text = robotsTxt(ORIGIN);
  assert.match(text, /^# https:\/\/waveform\.example: Waveform\./);
  for (const line of ["User-agent: ClaudeBot", "User-agent: *", "Allow: /", "Allow: /llms.txt", "Disallow: /api/", "Disallow: /auth/", "Disallow: /sign-in", "Disallow: /speech", "Disallow: /settings", `Sitemap: ${ORIGIN}/sitemap.xml`]) {
    assert.ok(text.split("\n").includes(line), line);
  }
});

test("the sitemap lists the public pages with their date", () => {
  const xml = sitemapXml(ORIGIN, "2026-10-10");
  assert.match(xml, /^<\?xml version="1\.0" encoding="UTF-8"\?>/);
  assert.ok(xml.includes(`<url><loc>${ORIGIN}/</loc><lastmod>2026-10-10</lastmod></url>`));
  assert.ok(xml.includes(`<loc>${ORIGIN}/llms.txt</loc>`));
});

test("llms.txt says what the app is, how a Silicon installs it and signs in, and where the docs are", () => {
  const text = llmsTxt(ORIGIN);
  assert.match(text, /^# Waveform\n\n> Give words a voice. Give audio a transcript\.\n/);
  assert.ok(text.includes("`silicon-apps install waveform`"));
  assert.ok(text.includes("`silicon-accounts login --app waveform -q | waveform login --slt-stdin`"));
  assert.ok(text.includes("- [Docs](https://docs.waveform.teamofsilicons.com)"));
  for (const word of ["AI agent", "human", "user account", "organization", " org ", "Honeycomb", "IAM"]) assert.ok(!text.includes(word), `never "${word}"`);
});

test("the mark is the app's logo, in ink on a cream square, rounded for the browser tab", () => {
  const svg = markSvg();
  assert.ok(svg.startsWith("<svg "));
  assert.ok(svg.includes('<rect width="200" height="200" fill="#EDE8E0"/>'));
  assert.ok(svg.includes("#181818"));
  assert.ok(markSvg({ rounded: true }).includes('<rect width="200" height="200" rx="44" fill="#EDE8E0"/>'));
});
