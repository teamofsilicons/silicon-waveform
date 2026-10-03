// Actual Solid UI regression with synthetic loopback HTTP only. No live IAM or speech.
// PLAYWRIGHT_MODULE=/absolute/path/to/@playwright/test/index.mjs node tests/popup-ui-browser.mjs
import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";
import { createServer } from "vite";
import { fixture } from "./fixture.mjs";

if (!process.env.PLAYWRIGHT_MODULE) throw new Error("Set PLAYWRIGHT_MODULE to an existing local @playwright/test/index.mjs; no installation is performed.");
const { chromium, expect } = await import(pathToFileURL(process.env.PLAYWRIGHT_MODULE).href);
const server = await createServer({ root: new URL("..", import.meta.url).pathname, server: { host: "127.0.0.1", port: 0, strictPort: false }, logLevel: "error" });
await server.listen();
const origin = `http://127.0.0.1:${server.httpServer.address().port}`;
const browser = await chromium.launch({ headless: true });
const results = [];
const actor = { public_id: "c:browser-fixture", actor_type: "carbon", org_id: "tos", org_role: "owner", scopes: ["self.identity.read"] };
const signed = { context: "fixture-context", contextId: "fixture-slot", contexts: [{ id: "fixture-slot", plane: "production", user: actor, org: "tos", environment: null }], authenticated: true, user: actor, org: "tos", plane: "production", environment: null, productionAvailable: true, testAvailable: false };
async function scenario(name, authenticated, run) {
  const context = await browser.newContext();
  const page = await context.newPage(), backend = fixture(), errors = [];
  const state = { session: authenticated ? signed : { ...signed, authenticated: false, user: null, contexts: [] }, cancels: [], starts: [], completions: [], statuses: [], speech: [], authCancelFailures: 0, storageCancelFailures: 0, storageCompletionFailures: 0, approved: false, pending: undefined };
  page.on("pageerror", e => errors.push(e.message));
  await page.addInitScript(() => {
    window.__popups = [];
    window.__popupMode = "pending";
    window.open = () => {
      if (window.__popupMode === "blocked") return null;
      if (window.__popupMode === "throws") throw new Error("fixture popup blocked");
      const popup = { closed: false, location: { href: "about:blank" }, focus() {}, close() { this.closed = true; } };
      window.__popups.push(popup);
      return popup;
    };
  });
  await context.route("**/*", async route => {
    const req = route.request(), url = new URL(req.url());
    if (url.origin !== origin) return route.abort("blockedbyclient");
    if (!/^\/(api|auth|health)\//.test(url.pathname)) return route.continue();
    const body = req.postData() ? req.postDataJSON() : {}, path = url.pathname;
    const json = (data, status = 200) => route.fulfill({ status, contentType: "application/json", body: JSON.stringify(data) });
    const fail = () => json({ error: { code: "temporary_failure", message: "Fixture retry needed" } }, 503);
    if (path === "/api/session") return json(state.session);
    if (path === "/auth/cancel") {
      state.cancels.push({ kind: "login", body });
      if (state.authCancelFailures-- > 0) return fail();
      return json(state.session);
    }
    if (path === "/api/session/storage/start") {
      state.starts.push(body);
      state.pending ??= { authorization_id: "11111111-1111-4111-8111-111111111111", state: "fixture-bound-state", consent_url: "https://iam.example/fixture-review", redirect_url: `${origin}/fixture-review`, manual: !body.popup_nonce, code_saved: false, status: "pending", expires_at: new Date(Date.now() + 600000).toISOString() };
      return json(state.pending);
    }
    if (path === "/api/session/storage/cancel") {
      state.cancels.push({ kind: "storage", body });
      if (state.storageCancelFailures-- > 0) return fail();
      return json({ cancelled: true });
    }
    if (path === "/api/session/storage/status") { state.statuses.push(body); return json(state.pending); }
    if (path === "/api/session/storage/complete") {
      state.completions.push(body);
      if (state.storageCompletionFailures-- > 0) return route.abort("failed");
      assert.equal(body.authorization_id, state.pending.authorization_id);
      assert.equal(body.state, state.pending.state);
      state.approved = true; state.pending.status = "completed";
      return json(state.pending);
    }
    if (path === "/api/v1/tts") {
      state.speech.push({ body, key: req.headers()["idempotency-key"], id: req.headers()["x-request-id"] });
      if (!state.approved) return json({ error: { code: "storage_authorization_required", message: "Review Briefcase access" } }, 403);
    }
    const response = await backend.fetcher(url, { method: req.method(), body: req.postData() || undefined, headers: { ...req.headers(), authorization: "Bearer fixture-only" } });
    await route.fulfill({ status: response.status, headers: Object.fromEntries(response.headers), body: Buffer.from(await response.arrayBuffer()) });
  });
  try {
    await page.goto(origin);
    await expect(page.getByLabel("Text to speak")).toBeVisible();
    await run({ page, state });
    assert.deepEqual(errors, []);
    results.push({ name, passed: true });
    console.log(`PASS ${name}`);
  } finally { await context.close(); }
}
async function needStorage(page) {
  await page.getByLabel("Text to speak").fill("Preserve this synthetic speech draft.");
  await page.getByRole("button", { name: "Generate speech", exact: true }).click();
  const panel = page.getByRole("region", { name: "Briefcase approval" });
  await expect(panel).toBeVisible();
  return panel;
}
try {
  await scenario("ordinary popup close/reopen and cancellation 503 retry", false, async ({ page, state }) => {
    state.authCancelFailures = 1;
    await page.getByRole("button", { name: "Sign in", exact: true }).first().click();
    let modal = page.getByRole("dialog", { name: "Welcome to Waveform" });
    await modal.getByRole("button", { name: "Continue as Carbon" }).click();
    await expect.poll(() => page.evaluate(() => window.__popups[0]?.location.href)).toContain("/auth/start");
    await expect(modal.getByRole("link", { name: "Carbon", exact: true })).toBeVisible();
    await expect(modal.getByRole("button", { name: "Cancel sign-in" })).toBeEnabled();
    await modal.getByRole("button", { name: "Close dialog" }).click();
    await expect(modal).not.toBeVisible();
    await expect.poll(() => state.cancels.length).toBe(1);
    await page.getByRole("button", { name: "Sign in", exact: true }).first().click();
    modal = page.getByRole("dialog", { name: "Welcome to Waveform" });
    await expect(modal.getByRole("button", { name: "Continue as Carbon" })).toBeEnabled();
    await modal.getByRole("button", { name: "Continue as Carbon" }).click();
    await expect.poll(() => state.cancels.length).toBe(2);
    assert.equal(state.cancels[0].body.nonce, state.cancels[1].body.nonce);
    await expect.poll(() => page.evaluate(() => window.__popups[1]?.location.href)).toContain("/auth/start");
    assert.equal(await page.evaluate(() => window.__popups[0].closed), true);
    await page.keyboard.press("Escape");
    await expect(modal).not.toBeVisible();
  });
  // Feature-specific scenarios are below; actual source is mounted inside Speech's form.
  await scenario("feature cancel 503 can retry, preserves speech draft and permits resume", true, async ({ page, state }) => {
    const panel = await needStorage(page);
    await panel.getByRole("button", { name: "Review in popup", exact: true }).click();
    await expect.poll(() => state.starts.length).toBe(1);
    await expect(panel.getByRole("button", { name: "Cancel review", exact: true })).toBeEnabled();
    state.storageCancelFailures = 1;
    await panel.getByRole("button", { name: "Cancel review", exact: true }).click();
    await expect(panel).toContainText("Fixture retry needed");
    await expect(panel.getByRole("button", { name: "Cancel review", exact: true })).toBeEnabled();
    await panel.getByRole("button", { name: "Cancel review", exact: true }).click();
    await expect(page.getByRole("button", { name: "Resume approval" })).toBeVisible();
    await expect(page.getByLabel("Text to speak")).toHaveValue("Preserve this synthetic speech draft.");
    assert.equal(state.speech.length, 1);
    assert.equal(state.cancels[0].body.review_nonce, state.cancels[1].body.review_nonce);
    await page.getByRole("button", { name: "Resume approval" }).click();
    await expect(page.getByRole("region", { name: "Briefcase approval" })).toBeVisible();
    assert.equal(await page.evaluate(() => window.__popups[0].closed), true);
  });
  await scenario("blocked popup has manual code recovery and original speech is retried only explicitly", true, async ({ page, state }) => {
    const panel = await needStorage(page);
    await page.evaluate(() => { window.__popupMode = "blocked"; });
    await panel.getByRole("button", { name: "Review in popup", exact: true }).click();
    await expect(panel.getByRole("button", { name: "Review manually" })).toBeEnabled();
    await panel.getByRole("button", { name: "Review manually" }).click();
    await expect(panel.getByLabel("Approval code")).toBeVisible();
    assert.equal(state.starts.length, 1);
    assert.equal(state.starts[0].popup_nonce, undefined);
    assert.match(state.starts[0].review_nonce, /^[a-f0-9]{64}$/);
    assert.equal(state.pending.manual, true);
    assert.equal(await page.locator("form form").count(), 0);
    assert.equal(await panel.getByRole("button", { name: "Complete approval" }).getAttribute("type"), "button");
    state.storageCompletionFailures = 1;
    await panel.getByLabel("Approval code").fill("obc_browser_fixture");
    await panel.getByRole("button", { name: "Complete approval" }).click();
    await expect(panel.getByRole("button", { name: "Retry completion" })).toBeEnabled();
    await expect(panel.getByLabel("Approval code")).toHaveValue("");
    await panel.getByRole("button", { name: "Retry completion" }).click();
    await expect(page.getByRole("button", { name: "Retry original request" })).toBeVisible();
    assert.deepEqual(state.completions[1], state.completions[0]);
    assert.equal(state.speech.length, 1, "permission completion must not generate speech");
    await expect(page.getByLabel("Text to speak")).toHaveValue("Preserve this synthetic speech draft.");
    await page.getByRole("button", { name: "Retry original request" }).click();
    await expect.poll(() => state.speech.length).toBe(2);
    assert.deepEqual(state.speech[1], state.speech[0], "original body, request ID and idempotency key must survive approval");
  });
  await scenario("manual fallback reuses popup-origin approval and checks callback status without code", true, async ({ page, state }) => {
    const panel = await needStorage(page);
    await panel.getByRole("button", { name: "Review in popup", exact: true }).click();
    await expect.poll(() => state.starts.length).toBe(1);
    await expect(panel.getByRole("button", { name: "Review manually" })).toBeEnabled();
    await panel.getByRole("button", { name: "Review manually" }).click();
    await expect.poll(() => state.starts.length).toBe(2);
    assert.equal(state.pending.manual, false, "original request mode remains immutable");
    assert.equal(state.starts[1].popup_nonce, undefined);
    assert.notEqual(state.starts[0].review_nonce, state.starts[1].review_nonce);
    assert.equal(state.cancels[0].body.review_nonce, state.starts[0].review_nonce);
    assert.equal(await page.evaluate(() => window.__popups[0].closed), true);
    await expect(panel.getByRole("button", { name: "Check approval" })).toBeEnabled();
    state.pending.status = "completed"; state.approved = true;
    await panel.getByRole("button", { name: "Check approval" }).click();
    await expect(page.getByRole("button", { name: "Retry original request" })).toBeVisible();
    assert.equal(state.completions.length, 0);
    assert.equal(state.statuses.length, 1);
    assert.equal(state.statuses[0].authorization_id, state.pending.authorization_id);
    assert.equal(state.speech.length, 1);
  });
  console.log(JSON.stringify({ local_only: true, results }, null, 2));
} finally {
  await browser.close();
  await server.close();
}
