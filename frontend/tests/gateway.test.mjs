import { test } from "node:test";
import assert from "node:assert/strict";
import { createGateway } from "../server/gateway.mjs";
import { fixture } from "./fixture.mjs";
const origin = "http://localhost:4325";
function setup(extra = {}) {
  const fake = fixture(),
    handler = createGateway({
      backend: "http://127.0.0.1:4340",
      origin,
      fetcher: fake.fetcher,
      ...extra,
    });
  let cookie = "",
    context = "";
  async function call(
    path,
    body,
    method = body === undefined ? "GET" : "POST",
    headers = {},
  ) {
    const response = await handler(
      new Request(origin + path, {
        method,
        headers: {
          cookie,
          origin,
          "x-waveform-context": context,
          ...(body === undefined ? {} : { "content-type": "application/json" }),
          ...headers,
        },
        body: body === undefined ? undefined : JSON.stringify(body),
      }),
    );
    cookie = response.headers.get("set-cookie")?.split(";")[0] || cookie;
    const value = await response
      .clone()
      .json()
      .catch(() => null);
    if (value?.context) context = value.context;
    return response;
  }
  async function login() {
    const result = await call("/api/session/login", {
      slt: "oac_waveform_ui_fixture",
      org: "tos",
    });
    assert.equal(result.status, 200);
    return result;
  }
  return { fake, call, login };
}
test("IAM login stores credentials only on the server, validates identity, and logs out", async () => {
  const s = setup();
  const response = await s.login();
  const body = await response.text();
  assert.ok(!body.includes("oat_") && !body.includes("ort_"));
  assert.match(response.headers.get("set-cookie"), /HttpOnly; SameSite=Lax/);
  assert.equal((await s.call("/api/session")).status, 200);
  assert.equal((await s.call("/api/session/logout", {})).status, 200);
  assert.equal((await s.call("/api/v1/jobs")).status, 401);
  assert.ok(
    s.fake.requests.some(
      (r) =>
        r.path.endsWith("/logout") && r.body.token === "ort_production_fixture",
    ),
  );
});
test("cross-origin mutations and non-public proxy routes are rejected", async () => {
  const s = setup();
  const denied = await s.call(
    "/api/session/login",
    { slt: "oac_waveform_ui_fixture" },
    "POST",
    { origin: "https://evil.example" },
  );
  assert.equal(denied.status, 403);
  assert.equal(s.fake.requests.length, 0);
  await s.login();
  for (const path of [
    "/api/v1/auth/refresh",
    "/api/v1/auth/login",
    "/api/v1/internal",
    "/api/v1/oauth/introspect",
    "/api/v1/tts/../../secrets",
  ])
    assert.equal((await s.call(path, {})).status, 404);
});
test("test roots never inherit production bearer tokens; switching restores the right identity", async () => {
  const s = setup();
  await s.login();
  assert.equal(
    (
      await s.call("/api/session/environment", {
        key: s.fake.testKey,
        org: "tos",
      })
    ).status,
    200,
  );
  assert.equal(
    (await (await s.call("/api/session")).json()).authenticated,
    false,
  );
  assert.equal((await s.call("/api/v1/tts", { text: "test" })).status, 401);
  await s.login();
  await s.call("/api/v1/tts", { text: "test" }, "POST", {
    "idempotency-key": "speech-test-1",
  });
  const request = s.fake.requests.at(-1);
  assert.equal(
    request.headers.get("x-testing-environment-key"),
    s.fake.testKey,
  );
  assert.equal(request.headers.get("authorization"), "Bearer oat_test_fixture");
  const switched = await (
    await s.call("/api/session/switch", { plane: "production" })
  ).json();
  assert.equal(switched.user.public_id, "waveform-tester");
  await s.call("/api/v1/preferences");
  assert.equal(
    s.fake.requests.at(-1).headers.get("x-testing-environment-key"),
    null,
  );
});
test("failed test connection preserves production context", async () => {
  const s = setup();
  await s.login();
  assert.equal(
    (await s.call("/api/session/environment", { key: "x".repeat(32) })).status,
    401,
  );
  assert.equal(
    (await (await s.call("/api/session")).json()).plane,
    "production",
  );
});
test("speech retry preserves canonical request and idempotency keys, pagination is forwarded", async () => {
  const s = setup();
  await s.login();
  const headers = {
    "idempotency-key": "repeat-this-request",
    "x-request-id": "33333333-3333-4333-8333-333333333333",
  };
  const payload = {
    text: "Hello",
    lang: "en",
    provider_order: ["openai", "gemini", "elevenlabs"],
  };
  const first = await (
    await s.call("/api/v1/tts", payload, "POST", headers)
  ).json();
  const again = await (
    await s.call("/api/v1/tts", payload, "POST", headers)
  ).json();
  assert.deepEqual(again, first);
  assert.equal(first.provider, "openai");
  assert.equal(s.fake.plane("").jobs.length, 1);
  await s.call("/api/v1/jobs?operation=tts&limit=20&cursor=opaque%7Ccursor");
  assert.match(s.fake.requests.at(-1).query, /cursor=opaque%7Ccursor/);
});
test("account preferences and write-only keys support save and remove", async () => {
  const s = setup();
  await s.login();
  await s.call(
    "/api/v1/preferences",
    { tts_order: ["openai", "elevenlabs", "gemini"] },
    "PATCH",
  );
  assert.equal(
    (await (await s.call("/api/v1/preferences")).json()).tts_order[0],
    "openai",
  );
  await s.call(
    "/api/v1/provider-keys/gemini",
    { api_key: "fake-private-key" },
    "PUT",
  );
  const keys = await (await s.call("/api/v1/provider-keys")).text();
  assert.ok(keys.includes("gemini") && !keys.includes("fake-private-key"));
  await s.call("/api/v1/provider-keys/gemini", {}, "DELETE");
  assert.equal(
    (await (await s.call("/api/v1/provider-keys")).json()).items.length,
    0,
  );
});
test("environment create, list, inspect, retrieve key, rotate, delete and restore use the complete contract", async () => {
  const s = setup();
  await s.login();
  const body = {
    name: "Verification",
    description: "Fixture only",
    iam_environment_id: "11111111-1111-4111-8111-111111111111",
    iam_environment_key: "a".repeat(32),
    briefcase_environment_key: "b".repeat(32),
    app_secret: "test-app-secret",
  };
  const created = await (
    await s.call("/api/v1/testing-environments", body)
  ).json();
  assert.deepEqual(s.fake.requests.at(-1).body, body);
  const path = `/api/v1/testing-environments/${created.id}`;
  assert.equal((await s.call(path)).status, 200);
  assert.equal((await (await s.call(path + "/key")).json()).key, created.key);
  const rotated = await (await s.call(path + "/rotate-key", {})).json();
  assert.notEqual(rotated.key, created.key);
  await s.call(path + "/delete", {});
  assert.ok(
    (await (await s.call("/api/v1/testing-environments")).json()).items[0]
      .deleted_at,
  );
  await s.call(path + "/restore", {});
  assert.equal((await (await s.call(path)).json()).deleted_at, null);
});
test("clean uses only the selected root and rejects production", async () => {
  const s = setup();
  await s.login();
  assert.equal(
    (await s.call("/api/v1/testing-environment/clean", {})).status,
    400,
  );
  await s.call("/api/session/environment", { key: s.fake.testKey });
  assert.equal(
    (await s.call("/api/v1/testing-environment/clean", {})).status,
    200,
  );
  const request = s.fake.requests.at(-1);
  assert.equal(
    request.headers.get("x-testing-environment-key"),
    s.fake.testKey,
  );
  assert.equal(request.headers.get("authorization"), null);
});
test("IAM handoff validates state and scrubs the code from the final URL", async () => {
  const s = setup();
  const start = await s.call("/auth/start?org=tos");
  const destination = new URL(start.headers.get("location"));
  assert.equal(destination.origin, "https://auth.iam.teamofsilicons.com");
  assert.equal(destination.searchParams.get("app_id"), "tos>waveform");
  assert.equal(destination.searchParams.has("org_id"), false);
  assert.equal(destination.searchParams.has("org_ids"), false);
  const callback = new URL(destination.searchParams.get("redirect_uri"));
  callback.searchParams.set("slt", "oac_waveform_ui_fixture");
  const done = await s.call(callback.pathname + callback.search);
  assert.equal(done.status, 303);
  assert.equal(done.headers.get("location"), origin + "/");
  assert.equal(s.fake.requests.at(-1).headers.get("x-org-id"), "tos");
  const replay = await s.call(callback.pathname + callback.search);
  assert.match(replay.headers.get("location"), /auth_error=/);
});
test("401 refresh is single-flight and never exposes renewed tokens", async () => {
  const fake = fixture();
  let expired = true,
    refreshCount = 0;
  const s = setup({
    fetcher: async (url, options) => {
      if (new URL(url).pathname === "/api/v1/auth/refresh") {
        refreshCount++;
        await new Promise((r) => setTimeout(r, 20));
        expired = false;
      }
      if (new URL(url).pathname === "/api/v1/preferences" && expired)
        return Response.json({ error: { code: "expired" } }, { status: 401 });
      return fake.fetcher(url, options);
    },
  });
  await s.login();
  const responses = await Promise.all([
    s.call("/api/v1/preferences"),
    s.call("/api/v1/preferences"),
  ]);
  assert.ok(responses.every((r) => r.status === 200));
  assert.equal(refreshCount, 1);
});
test("upstream failures return safe errors without credential leakage", async () => {
  const s = setup({
    fetcher: async () => {
      throw new Error("private-api-key-must-not-leak");
    },
  });
  const result = await s.call("/health/ready");
  assert.equal(result.status, 502);
  assert.ok(!(await result.text()).includes("private-api-key"));
});

test("stale tabs cannot send a request into a newly selected environment", async () => {
  const s = setup();
  const old = await (await s.login()).json();
  await s.call("/api/session/environment", { key: s.fake.testKey });
  const count = s.fake.requests.length;
  const response = await s.call(
    "/api/v1/tts",
    { text: "must not run" },
    "POST",
    { "x-waveform-context": old.context },
  );
  assert.equal(response.status, 409);
  assert.equal(s.fake.requests.length, count);
});
test("public probes never create or replace an authenticated session cookie", async () => {
  const s = setup();
  assert.equal((await s.call("/health/ready")).headers.get("set-cookie"), null);
  await s.login();
  assert.equal(
    (await s.call("/api/v1/capabilities")).headers.get("set-cookie"),
    null,
  );
  assert.equal(
    (await (await s.call("/api/session")).json()).authenticated,
    true,
  );
});

test("decoded upstream bodies cannot carry stale compression or HTTP framing", async () => {
  const s = setup({
    fetcher: async () =>
      new Response('{"status":"ready"}', {
        headers: {
          "content-type": "application/json",
          "content-encoding": "gzip",
          "content-length": "4",
          connection: "close",
          "transfer-encoding": "chunked",
        },
      }),
  });
  const response = await s.call("/health/ready");
  for (const name of [
    "content-length",
    "content-encoding",
    "connection",
    "transfer-encoding",
  ])
    assert.equal(response.headers.get(name), null);
  assert.deepEqual(await response.json(), { status: "ready" });
});

test("voice catalog, account defaults, per-request overrides and test isolation", async () => {
  const s = setup();
  assert.equal((await s.call("/api/v1/voice-profiles")).status, 401);
  await s.call("/api/session");
  await s.login();
  const catalog = await (await s.call("/api/v1/voice-profiles")).json();
  assert.equal(catalog.items.length, 30);
  const profile = catalog.items.find((p) => p.id === "puck");
  assert.equal(profile.elevenlabs.model_id, "eleven_multilingual_v2");
  assert.equal(
    (await s.call("/api/v1/voice-profiles", {}, "PATCH")).status,
    404,
  );
  await s.call("/api/v1/preferences", { voice_profile: "puck" }, "PATCH");
  assert.deepEqual(s.fake.requests.at(-1).body, { voice_profile: "puck" });
  const first = await (
    await s.call("/api/v1/tts", { text: "Use default" }, "POST", {
      "idempotency-key": "voice-default",
    })
  ).json();
  assert.equal(first.voice_profile.id, "puck");
  const override = await (
    await s.call(
      "/api/v1/tts",
      {
        text: "Use override",
        voice_profile: "sulafat",
        provider_order: ["elevenlabs"],
      },
      "POST",
      { "idempotency-key": "voice-override" },
    )
  ).json();
  assert.equal(override.voice_profile.id, "sulafat");
  assert.equal(override.provider, "elevenlabs");
  assert.equal(
    (await (await s.call("/api/v1/preferences")).json()).voice_profile,
    "puck",
  );
  await s.call("/api/session/environment", { key: s.fake.testKey, org: "tos" });
  await s.login();
  assert.equal(
    (await (await s.call("/api/v1/preferences")).json()).voice_profile,
    "kore",
  );
  await s.call("/api/v1/preferences", { voice_profile: "sulafat" }, "PATCH");
  assert.equal(
    (await s.call("/api/session/switch", { plane: "production" })).status,
    200,
  );
  assert.equal(
    (await (await s.call("/api/v1/preferences")).json()).voice_profile,
    "puck",
  );
});
