import test from "node:test";
import assert from "node:assert/strict";
import { build } from "vite";

let built;
async function client() {
  built ||= build({
    configFile: false,
    logLevel: "silent",
    build: {
      write: false,
      minify: false,
      lib: {
        entry: new URL("../src/api.ts", import.meta.url).pathname,
        formats: ["es"],
        fileName: "api",
      },
    },
  });
  const result = await built;
  const output = Array.isArray(result) ? result[0].output : result.output;
  const code = output.find((item) => item.type === "chunk").code;
  return import(
    "data:text/javascript;base64," +
      Buffer.from(code + `\n// ${crypto.randomUUID()}`).toString("base64")
  );
}
const account = (context, actor = "carbon") => ({
  context,
  contextId: context,
  authenticated: true,
  plane: "production",
  org: "same-org",
  user: { public_id: context, actor_type: actor, org_id: "same-org" },
  environment: null,
  productionAvailable: true,
  testAvailable: false,
});

test("mounted speech and approval requests cannot adopt another account in the same organization", async (t) => {
  const api = await client();
  api.acceptSession(account("first"));
  const request = api.bindApi();
  const calls = [];
  let finish;
  t.mock.method(globalThis, "fetch", async (path, options) => {
    calls.push({ path, ...options });
    return new Promise((resolve) => {
      finish = resolve;
    });
  });
  const pending = request("/api/v1/tts", {
    method: "POST",
    body: { text: "Original words" },
    headers: { "idempotency-key": "original-operation" },
  });
  assert.equal(calls[0].headers["x-waveform-context"], "first");
  api.acceptSession(account("second", "silicon"));
  finish(Response.json({ file_url: "https://briefcase.example/first" }));
  await assert.rejects(pending, (error) => error.code === "workspace_changed");
  await assert.rejects(
    request(
      "/api/v1/storage-authorizations/11111111-1111-4111-8111-111111111111/complete",
      { method: "POST", body: { code: "obc_original" } },
    ),
    (error) => error.code === "workspace_changed",
  );
  assert.equal(calls.length, 1);
  api.recordEvent("speech_failed", 1, "first");
  assert.equal(calls.length, 1);
});

test("a delayed session action cannot overwrite a new workspace and ordinary refresh keeps drafts mounted", async (t) => {
  const api = await client();
  api.acceptSession(account("first"));
  const version = api.contextVersion();
  api.acceptSession(account("first"));
  assert.equal(api.contextVersion(), version);
  let finish;
  t.mock.method(
    globalThis,
    "fetch",
    async () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const pending = api.sessionAction("refresh");
  api.acceptSession(account("second"));
  finish(Response.json(account("first")));
  await assert.rejects(pending, (error) => error.code === "workspace_changed");
  assert.equal(api.session().context, "second");
});

test("a delayed bootstrap session response cannot replace a newer accepted workspace", async (t) => {
  const api = await client();
  let finish;
  t.mock.method(
    globalThis,
    "fetch",
    async () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const pending = api.api("/api/session");
  api.acceptSession(account("new-workspace"));
  finish(Response.json(account("old-workspace")));
  await assert.rejects(pending, (error) => error.code === "workspace_changed");
  assert.equal(api.session().context, "new-workspace");
});
