import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createGateway } from "../server/gateway.mjs";
import { SessionStore } from "../server/session-store.mjs";
import { fixture } from "./fixture.mjs";

const origin = "http://localhost:4325",
  backend = "http://127.0.0.1:4340",
  iam = "https://auth.iam.teamofsilicons.com",
  appId = "waveform";
function setup(t, wrap = (fetcher) => fetcher) {
  const directory = mkdtempSync(join(tmpdir(), "waveform-browser-session-")),
    fake = fixture();
  const options = {
    origin,
    backend,
    iam,
    appId,
    sessionDirectory: directory,
    fetcher: wrap(fake.fetcher),
  };
  let gateway = createGateway(options),
    cookie = "",
    context = "";
  t.after(() => {
    gateway.close();
    rmSync(directory, { recursive: true, force: true });
  });
  const restart = () => {
    gateway.close();
    gateway = createGateway(options);
  };
  const stored = (change) => {
    gateway.close();
    const store = new SessionStore(directory, { backend, origin, iam, appId }),
      rows = store.load();
    if (change) {
      change(rows[0]);
      store.save(rows[0]);
    }
    store.close();
    gateway = createGateway(options);
    return rows;
  };
  async function call(path, body, headers = {}) {
    const response = await gateway(
      new Request(origin + path, {
        method: body === undefined ? "GET" : "POST",
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
  const login = async () => {
    const response = await call("/api/session/login", {
      slt: "oac_waveform_ui_fixture",
    });
    assert.equal(response.status, 200);
    return response;
  };
  return { call, login, restart, stored, directory, fake };
}

test("encrypted browser login survives restart and days of inactivity, while logout remains removed", async (t) => {
  let now = Date.now();
  t.mock.method(Date, "now", () => now);
  const s = setup(t),
    login = await s.login();
  assert.match(login.headers.get("set-cookie"), /Max-Age=34560000/);
  const raw = readFileSync(join(s.directory, "sessions.sqlite")).toString(
    "latin1",
  );
  assert.doesNotMatch(raw, /oat_production|ort_production|waveform-tester/);
  assert.equal(statSync(join(s.directory, "session.key")).mode & 0o777, 0o600);
  now += 2 * 86400_000;
  s.restart();
  const retained = await (await s.call("/api/session")).json();
  assert.equal(retained.authenticated, true);
  assert.equal(retained.user.public_id, "waveform-tester");
  assert.doesNotMatch(JSON.stringify(retained), /oat_|ort_/);
  assert.equal((await s.call("/api/session/logout", {})).status, 200);
  s.restart();
  assert.equal(
    (await (await s.call("/api/session")).json()).authenticated,
    false,
  );
});

test("an uncertain refresh keeps its persisted retry receipt across restart and status failures", async (t) => {
  let unavailable = true;
  const calls = [];
  const s = setup(t, (fetcher) => async (url, init) => {
    if (new URL(url).pathname === "/api/v1/auth/refresh") {
      calls.push({ key: init.headers.get("idempotency-key"), body: init.body });
      if (unavailable) throw new Error("Lost upstream response");
    }
    return fetcher(url, init);
  });
  await s.login();
  assert.equal((await s.call("/api/session/refresh", {})).status, 502);
  assert.equal((await s.call("/api/session")).status, 502);
  const saved = s.stored()[0];
  assert.equal(saved.production.refresh, "ort_production_fixture");
  assert.ok(saved.production.refreshStarted);
  unavailable = false;
  s.restart();
  assert.equal((await s.call("/api/session/refresh", {})).status, 200);
  assert.equal(calls.length, 3);
  assert.deepEqual(calls[0], calls[1]);
  assert.deepEqual(calls[1], calls[2]);
  assert.equal(s.stored()[0].production.refresh, "ort_production_refreshed");
  assert.equal(s.stored()[0].production.refreshKey, undefined);
});

test("temporary, configuration and malformed refresh failures preserve credentials; definitive revocation removes them", async (t) => {
  let mode;
  const s = setup(t, (fetcher) => async (url, init) => {
    if (new URL(url).pathname === "/api/v1/auth/refresh" && mode)
      return Response.json(mode.body, { status: mode.status });
    return fetcher(url, init);
  });
  await s.login();
  for (mode of [
    { status: 503, body: { error: { code: "unavailable" } } },
    { status: 401, body: { error: { code: "invalid_client" } } },
    { status: 400, body: { error: { code: "missing_required_header" } } },
    {
      status: 200,
      body: { access_token: "", refresh_token: "ort_next", expires_in: 1800 },
    },
    {
      status: 200,
      body: { access_token: "oat_next", refresh_token: "", expires_in: 1800 },
    },
    {
      status: 200,
      body: {
        access_token: "oat_next",
        refresh_token: "ort_next",
        expires_in: -1,
      },
    },
  ]) {
    assert.ok((await s.call("/api/session/refresh", {})).status >= 400);
    assert.equal(s.stored()[0].production.refresh, "ort_production_fixture");
  }
  mode = { status: 401, body: { error: { code: "unauthenticated" } } };
  assert.equal((await s.call("/api/session/refresh", {})).status, 401);
  assert.equal(s.stored().length, 0);
});

test("test and production credentials keep their context and selector through restart", async (t) => {
  const s = setup(t);
  const production = await (await s.login()).json();
  await s.call("/api/session/environment", { key: s.fake.testKey });
  await s.login();
  s.restart();
  const testing = await (await s.call("/api/session")).json();
  assert.equal(testing.plane, "test");
  assert.equal(testing.user.public_id, "test-carbon");
  assert.equal(
    s.fake.requests.at(-1).headers.get("x-testing-environment-key"),
    s.fake.testKey,
  );
  assert.doesNotMatch(JSON.stringify(testing), /ask_/);
  assert.equal(
    (
      await s.call("/api/v1/preferences", undefined, {
        "x-waveform-context": production.context,
      })
    ).status,
    409,
  );
  await s.call("/api/session/switch", { plane: "production" });
  await s.call("/api/session");
  assert.equal(
    s.fake.requests.at(-1).headers.get("x-testing-environment-key"),
    null,
  );
  assert.equal(
    s.fake.requests.at(-1).headers.get("authorization"),
    "Bearer oat_production_fixture",
  );
});

test("logout while a resource request renews cannot resurrect login after restart", async (t) => {
  let expire = false,
    finish,
    started;
  const received = new Promise((resolve) => {
    started = resolve;
  });
  const s = setup(t, (fetcher) => async (url, init) => {
    const path = new URL(url).pathname;
    if (path === "/api/v1/preferences" && expire)
      return Response.json({ error: { code: "expired" } }, { status: 401 });
    if (path === "/api/v1/auth/refresh") {
      started();
      await new Promise((resolve) => {
        finish = resolve;
      });
    }
    return fetcher(url, init);
  });
  await s.login();
  expire = true;
  const pending = s.call("/api/v1/preferences");
  await received;
  assert.equal((await s.call("/api/session/logout", {})).status, 200);
  finish();
  await pending;
  s.restart();
  assert.equal(
    (await (await s.call("/api/session")).json()).authenticated,
    false,
  );
});

for (const legacy of [false, true])
  test(`a ${legacy ? "legacy" : "dated"} expired receipt recovers only one additional refresh generation`, async (t) => {
    const requests = [];
    const s = setup(t, (fetcher) => async (url, init) => {
      if (new URL(url).pathname === "/api/v1/auth/refresh")
        requests.push({
          token: JSON.parse(init.body).refresh_token,
          key: init.headers.get("idempotency-key"),
        });
      return fetcher(url, init);
    });
    await s.login();
    s.stored((row) => {
      row.production.refreshKey = "pending-receipt";
      row.production.refreshStarted = legacy
        ? undefined
        : Date.now() - 2 * 3600_000;
    });
    assert.equal((await s.call("/api/session")).status, 200);
    assert.equal(requests.length, 2);
    assert.deepEqual(requests[0], {
      token: "ort_production_fixture",
      key: "pending-receipt",
    });
    assert.equal(requests[1].token, "ort_production_refreshed");
    assert.notEqual(requests[1].key, requests[0].key);
  });

test("a delayed refresh response cannot extend its original access lifetime", async (t) => {
  let now = Date.now();
  t.mock.method(Date, "now", () => now);
  const s = setup(t, (fetcher) => async (url, init) => {
    if (new URL(url).pathname === "/api/v1/auth/refresh") now += 60_000;
    return fetcher(url, init);
  });
  await s.login();
  const start = now;
  assert.equal((await s.call("/api/session/refresh", {})).status, 200);
  assert.equal(s.stored()[0].production.expires, start + 3600_000);
});

test("failed rotation persistence keeps the prior token usable only with its original retry key", async (t) => {
  const s = setup(t),
    save = SessionStore.prototype.save;
  await s.login();
  let unavailable = true;
  t.mock.method(SessionStore.prototype, "save", function (session) {
    if (
      unavailable &&
      session.production.refresh === "ort_production_refreshed"
    )
      throw new Error("Disk unavailable");
    return save.call(this, session);
  });
  assert.equal((await s.call("/api/session/refresh", {})).status, 502);
  const previous = s.stored()[0].production;
  assert.equal(previous.refresh, "ort_production_fixture");
  assert.ok(previous.refreshKey);
  unavailable = false;
  assert.equal((await s.call("/api/session/refresh", {})).status, 200);
  const requests = s.fake.requests.filter(
    (request) => request.path === "/api/v1/auth/refresh",
  );
  assert.equal(
    requests.length,
    1,
    "the durably staged successor avoids replaying the consumed token",
  );
  assert.equal(s.stored()[0].production.refresh, "ort_production_refreshed");
});

test("session changes serialize and recheck context before a queued logout", async (t) => {
  let block = false,
    finish,
    started;
  const received = new Promise((resolve) => {
    started = resolve;
  });
  const s = setup(t, (fetcher) => async (url, init) => {
    if (block && new URL(url).pathname === "/api/v1/auth/login") {
      started();
      await new Promise((resolve) => {
        finish = resolve;
      });
    }
    return fetcher(url, init);
  });
  await s.login();
  block = true;
  const replacement = s.call("/api/session/login", {
    slt: "oac_waveform_ui_fixture",
  });
  await received;
  const logout = s.call("/api/session/logout", {});
  finish();
  assert.equal((await replacement).status, 200);
  assert.equal((await logout).status, 409);
  assert.equal(
    s.fake.requests.some((request) => request.path === "/api/v1/auth/logout"),
    false,
  );
  assert.equal(
    (await (await s.call("/api/session")).json()).authenticated,
    true,
  );
});

test("late rejected reads reuse an already renewed generation", async (t) => {
  const pending = new Map();
  let wait = false,
    started,
    refreshes = 0;
  const received = new Promise((resolve) => {
    started = resolve;
  });
  const s = setup(t, (fetcher) => async (url, init) => {
    const path = new URL(url).pathname;
    if (path === "/api/v1/auth/refresh") refreshes++;
    if (
      wait &&
      ["/api/v1/preferences", "/api/v1/jobs"].includes(path) &&
      init.headers.get("authorization") === "Bearer oat_production_fixture"
    )
      return new Promise((resolve) => {
        pending.set(path, resolve);
        if (pending.size === 2) started();
      });
    return fetcher(url, init);
  });
  await s.login();
  wait = true;
  const first = s.call("/api/v1/preferences"),
    second = s.call("/api/v1/jobs");
  await received;
  pending.get("/api/v1/preferences")(
    Response.json({ error: { code: "expired" } }, { status: 401 }),
  );
  assert.equal((await first).status, 200);
  pending.get("/api/v1/jobs")(
    Response.json({ error: { code: "expired" } }, { status: 401 }),
  );
  assert.equal((await second).status, 200);
  assert.equal(refreshes, 1);
});

test("stale replay recovery is bounded if the issuer keeps returning expired access", async (t) => {
  let refreshes = 0;
  const s = setup(t, (fetcher) => async (url, init) => {
    if (new URL(url).pathname === "/api/v1/auth/refresh") {
      refreshes++;
      return Response.json({
        access_token: "oat_short",
        refresh_token: `ort_next_${refreshes}`,
        expires_in: 1,
      });
    }
    return fetcher(url, init);
  });
  await s.login();
  assert.equal((await s.call("/api/session/refresh", {})).status, 503);
  assert.equal(refreshes, 2);
  assert.equal(s.stored()[0].production.refresh, "ort_next_2");
});

test("session storage rejects a second gateway", (t) => {
  const s = setup(t);
  assert.throws(
    () =>
      createGateway({
        backend,
        origin,
        iam,
        appId,
        sessionDirectory: s.directory,
      }),
    /already owned/,
  );
});

for (const delayed of [false, true])
  test(`rotation remains recoverable when identity reads fail across ${delayed ? "a long outage" : "restart"}`, async (t) => {
    let now = Date.now(),
      unavailable = false;
    t.mock.method(Date, "now", () => now);
    const requests = [];
    const s = setup(t, (fetcher) => async (url, init) => {
      const path = new URL(url).pathname;
      if (path === "/api/v1/auth/refresh") {
        requests.push(JSON.parse(init.body).refresh_token);
        return Response.json({
          access_token: `oat_generation_${requests.length}`,
          refresh_token: `ort_generation_${requests.length}`,
          expires_in: 1800,
        });
      }
      if (path === "/api/v1/auth/me" && unavailable)
        return Response.json(
          { error: { code: "unavailable" } },
          { status: 503 },
        );
      return fetcher(url, init);
    });
    await s.login();
    unavailable = true;
    assert.equal((await s.call("/api/session/refresh", {})).status, 503);
    const staged = s.stored()[0].production;
    assert.equal(staged.refresh, "ort_production_fixture");
    assert.equal(staged.pendingTokens.refresh, "ort_generation_1");
    if (delayed) now += 2 * 86400_000;
    unavailable = false;
    s.restart();
    assert.equal((await s.call("/api/session")).status, 200);
    assert.deepEqual(
      requests,
      delayed
        ? ["ort_production_fixture", "ort_generation_1"]
        : ["ort_production_fixture"],
    );
    assert.equal(s.stored()[0].production.pendingTokens, undefined);
  });

for (const repeated of [false, true])
  test(`identity access rejection ${repeated ? "stops after bounded recovery" : "renews the staged successor"}`, async (t) => {
    let refreshes = 0;
    const s = setup(t, (fetcher) => async (url, init) => {
      const path = new URL(url).pathname;
      if (path === "/api/v1/auth/refresh") {
        refreshes++;
        assert.equal(
          JSON.parse(init.body).refresh_token,
          refreshes === 1
            ? "ort_production_fixture"
            : `ort_next_${refreshes - 1}`,
        );
        return Response.json({
          access_token: `oat_next_${refreshes}`,
          refresh_token: `ort_next_${refreshes}`,
          expires_in: 1800,
        });
      }
      if (
        path === "/api/v1/auth/me" &&
        init.headers.get("authorization").startsWith("Bearer oat_next_") &&
        (repeated || init.headers.get("authorization") === "Bearer oat_next_1")
      )
        return Response.json(
          { error: { code: "unauthenticated" } },
          { status: 401 },
        );
      return fetcher(url, init);
    });
    await s.login();
    assert.equal(
      (await s.call("/api/session/refresh", {})).status,
      repeated ? 503 : 200,
    );
    assert.equal(refreshes, 2);
    const retained = s.stored()[0].production;
    assert.equal(
      repeated ? retained.pendingTokens.refresh : retained.refresh,
      "ort_next_2",
    );
  });
