import { test } from "node:test";
import assert from "node:assert/strict";
import { createGateway, MAX_REQUEST_BODY_BYTES } from "../server/gateway.mjs";
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
test("expanded TTS controls and BYOK fit the gateway body limit", async () => {
  const s = setup();
  await s.login();
  const content = "😀".repeat(4096);
  const key = "x".repeat(16384);
  const payload = {
    text: content,
    auto_fallback: false,
    provider_options: {
      gemini: {
        scene: content,
        audio_profile: content,
        director_notes: content,
        sample_context: content,
      },
    },
    provider_keys: { gemini: key, elevenlabs: key, openai: key },
  };
  const bytes = Buffer.byteLength(JSON.stringify(payload));
  assert.ok(bytes > 128 * 1024);
  assert.ok(bytes < MAX_REQUEST_BODY_BYTES);
  const response = await s.call("/api/v1/tts", payload, "POST", {
    "idempotency-key": "expanded-tts-request",
  });
  assert.equal(response.status, 200);
  assert.deepEqual(s.fake.requests.at(-1).body, payload);
});
test("gateway rejects JSON above the expanded body limit before forwarding", async () => {
  const s = setup();
  await s.login();
  assert.equal(MAX_REQUEST_BODY_BYTES, 327680);
  const before = s.fake.requests.length;
  const response = await s.call("/api/v1/tts", {
    text: "x".repeat(MAX_REQUEST_BODY_BYTES),
  });
  assert.equal(response.status, 413);
  assert.equal((await response.json()).error.code, "request_too_large");
  assert.equal(s.fake.requests.length, before);
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
test("clean requires a signed-in test actor and rejects production", async () => {
  const s = setup();
  await s.login();
  assert.equal(
    (await s.call("/api/v1/testing-environment/clean", {})).status,
    400,
  );
  await s.call("/api/session/environment", { key: s.fake.testKey });
  assert.equal((await s.call("/api/v1/testing-environment/clean", {})).status,401);
  await s.login();
  assert.equal(
    (await s.call("/api/v1/testing-environment/clean", {})).status,
    200,
  );
  const request = s.fake.requests.at(-1);
  assert.equal(
    request.headers.get("x-testing-environment-key"),
    s.fake.testKey,
  );
  assert.match(request.headers.get("authorization"), /^Bearer /);
});
test("IAM handoff validates state and scrubs the code from the final URL", async () => {
  const s = setup();
  const start = await s.call("/auth/start");
  const destination = new URL(start.headers.get("location"));
  assert.equal(destination.origin, "https://auth.iam.teamofsilicons.com");
  assert.equal(destination.searchParams.get("app_id"), "waveform");
  assert.equal(destination.searchParams.has("org_id"), false);
  assert.equal(destination.searchParams.has("org_ids"), false);
  const callback = new URL(destination.searchParams.get("redirect_uri"));
  callback.searchParams.set("slt", "oac_waveform_ui_fixture");
  const done = await s.call(callback.pathname + callback.search);
  assert.equal(done.status, 303);
  assert.equal(done.headers.get("location"), origin + "/");
  assert.equal(s.fake.requests.at(-1).headers.get("x-org-id"), null);
  assert.equal((await (await s.call("/api/session")).json()).org, "tos");
  await s.call("/api/v1/preferences");
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
  const body = await result.text();
  assert.ok(!body.includes("private-api-key"));
  assert.match(body, /check job history before retrying/);
  assert.ok(!body.includes("retried safely"));
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

test("IAM app_secret selects a sandbox and public IDs remain test-only", async () => {
  const s=setup();
  await s.call("/api/session");
  assert.equal((await s.call("/api/session/login",{slt:"test-carbon"})).status,400);
  assert.equal((await s.call("/api/session/environment",{key:s.fake.testKey})).status,200);
  assert.equal((await s.call("/api/session/login",{slt:"test-carbon"})).status,200);
  const state=await (await s.call("/api/session")).json();
  assert.equal(state.plane,"test");assert.equal(state.user.public_id,"test-carbon");
  assert.ok(!JSON.stringify(state).includes(s.fake.testKey));
});


test("Briefcase consent preserves the login and selected plane without sending speech", async () => {
  const s = setup(); await s.login();
  const start = await s.call("/api/v1/storage-authorizations", {}, "POST", {"idempotency-key":"storage-start-fixture"});
  assert.equal(start.status,200);
  const request = await start.json();
  const upstream = s.fake.requests.at(-1);
  assert.equal(upstream.headers.get("authorization"),"Bearer oat_production_fixture");
  assert.equal(upstream.headers.get("x-org-id"),"tos");
  assert.equal(upstream.headers.get("idempotency-key"),"storage-start-fixture");
  const path = `/api/v1/storage-authorizations/${request.authorization_id}`;
  assert.equal((await s.call(path+"/complete",{code:"obc_fixture",state:"wrong"})).status,403);
  assert.equal((await s.call("/api/session")).status,200);
  const done = await s.call(path+"/complete",{code:"obc_fixture",state:request.state});
  assert.equal((await done.json()).status,"completed");
  assert.equal(s.fake.requests.some(r=>["/api/v1/tts","/api/v1/stt"].includes(r.path)),false);
  await s.call("/api/session/environment",{key:s.fake.testKey,org:"tos"});
  await s.call("/api/session/login",{slt:"test-carbon",org:"tos"});
  assert.equal((await s.call(path)).status,404);
});


test("popup login binds selected identity kind and nonce before creating a session", async () => {
  for (const kind of ["carbon", "silicon"]) {
    const s = setup();
    const nonce = "a".repeat(64);
    const start = await s.call(`/auth/start?identity_kind=${kind}&popup_nonce=${nonce}`);
    const destination = new URL(start.headers.get("location"));
    assert.equal(destination.searchParams.get("identity_kind"), kind);
    assert.equal(destination.searchParams.get("display"), "popup");
    const callback = new URL(destination.searchParams.get("redirect_uri"));
    callback.searchParams.set("slt", "oac_waveform_ui_fixture");
    const response = await s.call(callback.pathname + callback.search);
    const complete = new URL(response.headers.get("location"));
    assert.equal(complete.origin, origin);
    assert.equal(complete.searchParams.get("nonce"), nonce);
    assert.equal(complete.searchParams.get("result"), kind === "carbon" ? "ok" : "error");
    assert.equal(complete.searchParams.has("slt"), false);
    const current = await (await s.call("/api/session")).json();
    assert.equal(current.authenticated, kind === "carbon");
  }
});
test("popup login rejects malformed selection and keeps a prior session on mismatch", async () => {
  const s = setup();
  for (const query of ["identity_kind=admin", "identity_kind=carbon&popup_nonce=bad", `popup_nonce=${"b".repeat(64)}`]) {
    assert.equal((await s.call("/auth/start?" + query)).status, 400);
  }
  await s.call("/api/session");
  await s.login();
  const before = await (await s.call("/api/session")).json();
  const start = await s.call(`/auth/start?identity_kind=silicon&popup_nonce=${"b".repeat(64)}`);
  const callback = new URL(new URL(start.headers.get("location")).searchParams.get("redirect_uri"));
  callback.searchParams.set("slt", "oac_waveform_ui_fixture");
  await s.call(callback.pathname + callback.search);
  const after = await (await s.call("/api/session")).json();
  assert.deepEqual(after.user, before.user);
  assert.equal(after.context, before.context);
});


test("storage popup binds current workspace and exchanges its code only on the server", async () => {
  const s = setup(); await s.login();
  const nonce = "c".repeat(64);
  const started = await s.call("/api/session/storage/start", { popup_nonce: nonce });
  assert.equal(started.status, 200);
  const handoff = await started.json();
  assert.equal(new URL(handoff.redirect_url).searchParams.get("display"), "popup");
  const request = s.fake.requests.find(r => r.path === "/api/v1/storage-authorizations");
  assert.equal(request.body.redirect_uri, origin + "/auth/storage/callback");
  assert.equal((await s.call("/auth/storage/callback?state=wrong&code=obc_fixture")).status, 403);
  assert.equal(s.fake.requests.filter(r => r.path.endsWith("/complete")).length, 0);
  const callback = await s.call("/auth/storage/callback?state=fixture-state&code=obc_fixture");
  const target = new URL(callback.headers.get("location"));
  assert.equal(target.searchParams.get("nonce"), nonce);
  assert.equal(target.searchParams.get("result"), "ok");
  assert.equal(target.searchParams.has("code"), false);
  assert.equal(s.fake.requests.filter(r => r.path.endsWith("/complete")).length, 1);
  assert.equal((await (await s.call("/api/session")).json()).authenticated, true);
  assert.equal(s.fake.requests.some(r => r.path === "/api/v1/tts"), false);
});
test("storage popup retries an uncertain code exchange and refuses a switched workspace", async () => {
  const fake = fixture(); let fail = true;
  const s = setup({fetcher: async (url, options) => {
    if (new URL(url).pathname.endsWith("/complete") && fail) return Response.json({error:{code:"temporary"}}, {status:503});
    return fake.fetcher(url, options);
  }});
  await s.login();
  await s.call("/api/session/storage/start", {popup_nonce:"d".repeat(64)});
  const failed = await s.call("/auth/storage/callback?state=fixture-state&code=obc_fixture");
  assert.equal(new URL(failed.headers.get("location")).searchParams.get("result"), "error");
  fail = false;
  const retry = await (await s.call("/api/session/storage/start", {popup_nonce:"e".repeat(64)})).json();
  assert.equal(new URL(retry.redirect_url).searchParams.get("result"), "ok");
  assert.equal(fake.requests.filter(r => r.path === "/api/v1/storage-authorizations").length, 1);
  await s.call("/api/session/storage/start", {popup_nonce:"f".repeat(64)});
  await s.call("/api/session/environment", {key:fake.testKey,org:"tos"});
  assert.equal((await s.call("/auth/storage/callback?state=fixture-state&code=obc_fixture")).status, 403);
});
