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

test("full-page fallback keeps the chosen identity kind and returns without popup metadata", async () => {
  for (const kind of ["carbon", "silicon"]) {
    const fake = fixture();
    const s = setup({ fetcher: async (input, options) => {
      const response = await fake.fetcher(input, options);
      if (new URL(input).pathname !== "/api/v1/auth/me") return response;
      return Response.json({ ...await response.json(), actor_type: kind, public_id: kind === "carbon" ? "c:person" : "si:agent" });
    } });
    const start = await s.call(`/auth/start?identity_kind=${kind}`);
    assert.equal(start.status, 303);
    const destination = new URL(start.headers.get("location"));
    assert.equal(destination.searchParams.get("identity_kind"), kind);
    assert.equal(destination.searchParams.has("display"), false);
    assert.equal(destination.searchParams.has("popup_nonce"), false);
    const callback = new URL(destination.searchParams.get("redirect_uri"));
    assert.ok(callback.searchParams.get("state"));
    callback.searchParams.set("slt", "oac_waveform_ui_fixture");
    const completed = await s.call(callback.pathname + callback.search);
    assert.equal(completed.headers.get("location"), origin + "/");
    const current = await (await s.call("/api/session")).json();
    assert.equal(current.authenticated, true);
    assert.equal(current.user.actor_type, kind);
  }
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

test("each saved workspace retains its own approval and requires a fresh popup after selection", async () => {
  const s = setup(); await s.login();
  const start = async nonce => (await s.call("/api/session/storage/start", { popup_nonce: nonce.repeat(64) })).json();
  const production = await start("a");
  await s.call("/api/session/environment", { key: s.fake.testKey, org: "tos" });
  await s.call("/api/session/login", { slt: "test-carbon", org: "tos" });
  const testing = await start("b");
  assert.notEqual(testing.redirect_url, production.redirect_url);
  await s.call("/api/session/switch", { plane: "production" });
  assert.equal((await s.call("/auth/storage/callback?state=fixture-state&code=obc_fixture")).status, 403);
  assert.deepEqual(await start("c"), production);
  const callback = await s.call("/auth/storage/callback?state=fixture-state&code=obc_fixture");
  assert.equal(new URL(callback.headers.get("location")).searchParams.get("nonce"), "c".repeat(64));
  const completion = s.fake.requests.find(r => r.path.endsWith("/complete"));
  assert.equal(completion.headers.get("x-testing-environment-key"), null);
  await s.call("/api/session/switch", { plane: "test" });
  assert.deepEqual(await start("d"), testing);
  assert.equal(s.fake.requests.filter(r => r.path === "/api/v1/storage-authorizations").length, 2);
});

test("an old ordinary login popup cannot redeem a code after switching away and back", async () => {
  const s = setup(); await s.login();
  const started = await s.call(`/auth/start?identity_kind=carbon&popup_nonce=${"e".repeat(64)}`);
  const callback = new URL(new URL(started.headers.get("location")).searchParams.get("redirect_uri"));
  callback.searchParams.set("slt", "oac_waveform_ui_fixture");
  await s.call("/api/session/environment", { key: s.fake.testKey, org: "tos" });
  await s.call("/api/session/switch", { plane: "production" });
  const before = await (await s.call("/api/session")).json();
  const loginCount = s.fake.requests.filter(r => r.path === "/api/v1/auth/login").length;
  const response = await s.call(callback.pathname + callback.search);
  assert.equal(new URL(response.headers.get("location")).searchParams.get("result"), "error");
  assert.equal(s.fake.requests.filter(r => r.path === "/api/v1/auth/login").length, loginCount);
  assert.deepEqual(await (await s.call("/api/session")).json(), before);
});

test("approval completion checks its exact receipt and changed graphs require a new review", async () => {
  const fake = fixture(); let mode = "mismatch";
  const starts = [];
  const s = setup({fetcher: async (url, options) => {
    const path = new URL(url).pathname;
    if (path === "/api/v1/storage-authorizations") starts.push(options.headers.get("idempotency-key"));
    if (path.endsWith("/complete")) {
      if (mode === "mismatch") return Response.json({authorization_id:"00000000-0000-4000-8000-000000000000",state:"fixture-state",status:"completed"});
      if (mode === "changed") return Response.json({error:{code:"graph_changed"}}, {status:412});
    }
    return fake.fetcher(url, options);
  }});
  await s.login();
  await s.call("/api/session/storage/start", {popup_nonce:"f".repeat(64)});
  const callback = await s.call("/auth/storage/callback?state=fixture-state&code=obc_fixture");
  assert.equal(new URL(callback.headers.get("location")).searchParams.get("result"), "error");
  assert.equal((await s.call("/api/session/storage/start", {popup_nonce:"a".repeat(64)})).status, 409);
  assert.equal(starts.length, 1);
  mode = "changed";
  assert.equal((await s.call("/api/session/storage/start", {popup_nonce:"b".repeat(64)})).status, 412);
  mode = "ok";
  assert.equal((await s.call("/api/session/storage/start", {popup_nonce:"c".repeat(64)})).status, 200);
  assert.equal(starts.length, 2);
  assert.notEqual(starts[1], starts[0]);
  assert.equal((await (await s.call("/api/session")).json()).authenticated, true);
});

test("nonce cancellation rejects a delayed login start and prevents a pending callback exchange", async () => {
  const s = setup(); await s.login();
  const before = await (await s.call("/api/session")).json(), nonce = "1".repeat(64);
  const started = await s.call(`/auth/start?identity_kind=carbon&popup_nonce=${nonce}`);
  const callback = new URL(new URL(started.headers.get("location")).searchParams.get("redirect_uri")); callback.searchParams.set("slt","oac_waveform_ui_fixture");
  await s.call("/auth/cancel", {nonce});
  const count = s.fake.requests.length;
  await s.call(callback.pathname + callback.search);
  assert.equal(s.fake.requests.length,count);
  assert.equal((await s.call(`/auth/start?identity_kind=carbon&popup_nonce=${nonce}`)).status,409);
  assert.equal((await (await s.call("/api/session")).json()).context,before.context);
});

test("cancel queued behind a callback restores the previous account and never resurrects after logout", async () => {
  const fake = fixture(); let account = "c:first", block = false, entered, release;
  const enteredPromise = new Promise(done => { entered = done; });
  const s = setup({fetcher: async (url, options) => {
    const path = new URL(url).pathname;
    if (path.endsWith("/login") && block) { entered(); await new Promise(done => { release = done; }); }
    const response = await fake.fetcher(url,options);
    if (path.endsWith("/me")) return Response.json({...await response.json(), public_id:account});
    return response;
  }});
  const before = await (await s.login()).json();
  const nonce = "2".repeat(64), started = await s.call(`/auth/start?identity_kind=carbon&popup_nonce=${nonce}`);
  const callback = new URL(new URL(started.headers.get("location")).searchParams.get("redirect_uri")); callback.searchParams.set("slt","oac_waveform_ui_fixture");
  account="c:second"; block=true;
  const finishing = s.call(callback.pathname+callback.search); await enteredPromise;
  const cancelling = s.call("/auth/cancel",{nonce}); release(); await finishing;
  const restored = await (await cancelling).json();
  assert.equal(restored.user.public_id,before.user.public_id);
  assert.ok(restored.contexts.some(value=>value.user.public_id==="c:second"));
  await s.call("/api/session/logout",{});
  assert.equal((await (await s.call("/auth/cancel",{nonce})).json()).authenticated,false);
});

test("manual storage review needs no popup and cancellation retains the request while fencing late callbacks", async () => {
  const s=setup(); await s.login();
  const nonce="3".repeat(64), body={review_nonce:nonce};
  const review=await (await s.call("/api/session/storage/start",body)).json();
  assert.equal(review.manual,true);
  assert.deepEqual(s.fake.requests.find(r=>r.path==="/api/v1/storage-authorizations").body,{});
  await s.call("/api/session/storage/cancel",body);
  assert.equal((await s.call("/api/session/storage/start",body)).status,409);
  assert.equal((await s.call("/auth/storage/callback?state=fixture-state&code=obc_fixture")).status,403);
  const resumed=await (await s.call("/api/session/storage/start",{review_nonce:"4".repeat(64)})).json();
  assert.equal(resumed.authorization_id,review.authorization_id);
  assert.equal(s.fake.requests.filter(r=>r.path==="/api/v1/storage-authorizations").length,1);
  await s.call("/api/session/storage/cancel",body); // A delayed old cancellation cannot cancel the resumed review.
  const complete={authorization_id:review.authorization_id,state:review.state,code:"obc_fixture"};
  assert.equal((await s.call("/api/session/storage/complete",{...complete,state:"wrong"})).status,403);
  const done=await(await s.call("/api/session/storage/complete",complete)).json(); assert.equal(done.status,"completed");
  await s.call("/api/session/storage/cancel",{review_nonce:"4".repeat(64)});
  assert.equal((await(await s.call("/api/session/storage/status",complete)).json()).status,"completed");
  assert.equal(s.fake.requests.some(r=>r.path==="/api/v1/tts"||r.path.endsWith("/revoke")),false);
});

test("a late cancelled callback cannot consume the newer pending sign-in", async () => {
  const s=setup(); await s.login();
  const old=await s.call(`/auth/start?identity_kind=carbon&popup_nonce=${"5".repeat(64)}`);
  const oldCallback=new URL(new URL(old.headers.get("location")).searchParams.get("redirect_uri")); oldCallback.searchParams.set("slt","oac_waveform_ui_fixture");
  await s.call("/auth/cancel",{nonce:"5".repeat(64)});
  const fresh=await s.call(`/auth/start?identity_kind=carbon&popup_nonce=${"6".repeat(64)}`);
  const nextCallback=new URL(new URL(fresh.headers.get("location")).searchParams.get("redirect_uri")); nextCallback.searchParams.set("slt","oac_waveform_ui_fixture");
  const count=s.fake.requests.length;
  await s.call(oldCallback.pathname+oldCallback.search);
  assert.equal(s.fake.requests.length,count);
  const result=await s.call(nextCallback.pathname+nextCallback.search);
  const target=new URL(result.headers.get("location"));
  assert.equal(target.searchParams.get("nonce"),"6".repeat(64));
  assert.equal(target.searchParams.get("result"),"ok");
});

test("declined manual authorization allows an explicit fresh review without changing ordinary login", async () => {
  const fake=fixture(); let declined=true;
  const s=setup({fetcher:async(url,options)=>new URL(url).pathname.endsWith("/complete")&&declined ? Response.json({error:{code:"invalid_storage_authorization"}},{status:400}) : fake.fetcher(url,options)});
  const before=await(await s.login()).json();
  const first=await(await s.call("/api/session/storage/start",{review_nonce:"7".repeat(64)})).json();
  assert.equal((await s.call("/api/session/storage/complete",{authorization_id:first.authorization_id,state:first.state,code:"obc_fixture"})).status,400);
  declined=false;
  const next=await(await s.call("/api/session/storage/start",{review_nonce:"8".repeat(64)})).json();
  assert.notEqual(first.authorization_id,next.authorization_id);
  const current=await(await s.call("/api/session")).json();assert.equal(current.context,before.context);assert.deepEqual(current.user,before.user);
  assert.equal(fake.requests.some(r=>r.path==="/api/v1/tts"||r.path.endsWith("/revoke")),false);
});
