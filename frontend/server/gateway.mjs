import { randomBytes } from "node:crypto";

const json = (value, status = 200) => Response.json(value, { status });
const failure = (code, message, status = 400) =>
  json({ error: { code, message } }, status);
const routes = [
  ["GET", /^\/health\/(live|ready)$/],
  ["GET", /^\/api\/v1\/capabilities$/],
  ["GET", /^\/api\/v1\/auth\/me$/],
  ["POST", /^\/api\/v1\/(tts|stt)$/],
  ["GET", /^\/api\/v1\/jobs(?:\/[a-f0-9-]{36})?$/],
  ["GET", /^\/api\/v1\/voice-profiles$/],
  ["GET", /^\/api\/v1\/preferences$/],
  ["PATCH", /^\/api\/v1\/preferences$/],
  ["GET", /^\/api\/v1\/provider-keys$/],
  ["PUT", /^\/api\/v1\/provider-keys\/(gemini|elevenlabs|openai|deepgram)$/],
  ["DELETE", /^\/api\/v1\/provider-keys\/(gemini|elevenlabs|openai|deepgram)$/],
  ["GET", /^\/api\/v1\/testing-environments(?:\/[a-f0-9-]{36}(?:\/key)?)?$/],
  [
    "POST",
    /^\/api\/v1\/testing-environments(?:\/[a-f0-9-]{36}\/(rotate-key|delete|restore))?$/,
  ],
  ["GET", /^\/api\/v1\/testing-environment$/],
  ["POST", /^\/api\/v1\/testing-environment\/clean$/],
];
const id = () => randomBytes(32).toString("hex");
export function createGateway({
  backend = "https://backend.waveform.teamofsilicons.com",
  origin = "http://localhost:4325",
  iam = "https://auth.iam.teamofsilicons.com",
  appId = "tos>waveform",
  fetcher = fetch,
} = {}) {
  const sessions = new Map();
  const secure = new URL(origin).protocol === "https:";
  const cookieName = secure ? "__Host-waveform" : "waveform-local";
  for (const value of [backend, origin, iam]) {
    const url = new URL(value);
    if (
      url.username ||
      url.password ||
      (url.protocol !== "https:" &&
        !(
          url.protocol === "http:" &&
          ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname)
        ))
    )
      throw new Error("Use HTTPS or a loopback URL.");
  }
  async function upstream(path, slot, method = "GET", body, extra = {}) {
    const headers = new Headers({ accept: "application/json", ...extra });
    if (body !== undefined) headers.set("content-type", "application/json");
    if (slot?.org) headers.set("x-org-id", slot.org);
    if (slot?.key) headers.set("x-testing-environment-key", slot.key);
    if (slot?.access) headers.set("authorization", `Bearer ${slot.access}`);
    return fetcher(new URL(path, backend), {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      redirect: "error",
      signal: AbortSignal.timeout(240_000),
    });
  }
  async function tokens(slot, payload) {
    if (
      typeof payload.access_token !== "string" ||
      typeof payload.refresh_token !== "string"
    )
      throw new Error("Invalid token response");
    slot.access = payload.access_token;
    slot.refresh = payload.refresh_token;
    slot.expires = Date.now() + (Number(payload.expires_in) || 3600) * 1000;
  }
  async function refresh(slot) {
    if (!slot.refresh) return false;
    if (!slot.refreshing)
      slot.refreshing = (async () => {
        const response = await upstream("/api/v1/auth/refresh", slot, "POST", {
          refresh_token: slot.refresh,
        });
        if (!response.ok) {
          if (response.status === 401) {
            delete slot.access;
            delete slot.refresh;
            delete slot.user;
          }
          return false;
        }
        await tokens(slot, await response.json());
        return true;
      })().finally(() => {
        delete slot.refreshing;
      });
    return slot.refreshing;
  }
  async function authorized(path, slot, method, body, headers) {
    if (slot?.access && slot.expires < Date.now() + 30_000) await refresh(slot);
    let response = await upstream(path, slot, method, body, headers);
    if (response.status === 401 && slot?.refresh && (await refresh(slot)))
      response = await upstream(path, slot, method, body, headers);
    return response;
  }
  async function login(slot, slt) {
    if (typeof slt !== "string" || !/^oac_[!-~]{1,16380}$/.test(slt))
      return failure("invalid_token", "Enter a valid short-lived IAM code.");
    const response = await upstream(
      "/api/v1/auth/login",
      { ...slot, org: undefined },
      "POST",
      {
        slt,
      },
    );
    if (!response.ok) return response;
    await tokens(slot, await response.json());
    const me = await upstream("/api/v1/auth/me", slot);
    if (!me.ok) return me;
    slot.user = await me.json();
    slot.org = slot.user.org_id;
    return null;
  }
  const snapshot = (s) => ({
    authenticated: !!s[s.active]?.access,
    user: s[s.active]?.user ?? null,
    org: s[s.active]?.org ?? "tos",
    plane: s.active,
    environment: s.test?.environment ?? null,
    productionAvailable: !!s.production?.access,
    testAvailable: !!s.test?.key,
    context: s.context,
  });
  return async function handle(request) {
    let sessionId, session;
    const finish = (response) => {
      const headers = new Headers(response.headers);
      // fetch has already decoded the body. Never forward transport framing or
      // compressed lengths/encodings into the browser-facing HTTP response.
      for (const name of [
        "connection",
        "content-length",
        "content-encoding",
        "transfer-encoding",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "upgrade",
      ])
        headers.delete(name);
      headers.set("cache-control", "no-store");
      headers.set("referrer-policy", "no-referrer");
      headers.set("x-content-type-options", "nosniff");
      if (sessionId)
        headers.set(
          "set-cookie",
          `${cookieName}=${sessionId}; Path=/; HttpOnly; SameSite=Lax; Max-Age=86400${secure ? "; Secure" : ""}`,
        );
      return new Response(response.body, { status: response.status, headers });
    };
    try {
      const url = new URL(request.url),
        path = url.pathname;
      if (
        !["GET", "HEAD"].includes(request.method) &&
        request.headers.get("origin") !== new URL(origin).origin
      )
        return finish(
          failure(
            "origin_rejected",
            "Reload Waveform before trying again.",
            403,
          ),
        );
      for (const [key, s] of sessions)
        if (s.until < Date.now()) sessions.delete(key);
      sessionId = request.headers
        .get("cookie")
        ?.split(";")
        .map((x) => x.trim())
        .find((x) => x.startsWith(cookieName + "="))
        ?.slice(cookieName.length + 1);
      session = sessions.get(sessionId);
      const existingSession = !!session;
      if (
        request.method === "GET" &&
        (path.startsWith("/health/") || path === "/api/v1/capabilities")
      ) {
        sessionId = undefined;
        if (
          !routes.some(
            ([method, pattern]) => method === "GET" && pattern.test(path),
          )
        )
          return finish(failure("not_found", "Unknown route.", 404));
        return finish(await upstream(path));
      }
      if (!session) {
        if (sessions.size >= 10_000)
          return finish(failure("busy", "Please try again shortly.", 503));
        sessionId = id();
        session = {
          context: id(),
          active: "production",
          production: { org: "tos" },
          until: Date.now() + 86400_000,
        };
        sessions.set(sessionId, session);
      }
      session.until = Date.now() + 86400_000;
      const guarded =
        path.startsWith("/api/v1/") ||
        (path.startsWith("/api/session/") && request.method !== "GET");
      if (
        guarded &&
        existingSession &&
        request.headers.get("x-waveform-context") !== session.context
      )
        return finish(
          failure(
            "workspace_changed",
            "Your workspace changed in another tab. Reload Waveform to continue.",
            409,
          ),
        );
      let body;
      if (!["GET", "HEAD"].includes(request.method)) {
        if (
          !request.headers.get("content-type")?.startsWith("application/json")
        )
          return finish(failure("json_required", "Use a JSON request.", 415));
        const text = await request.text();
        if (Buffer.byteLength(text) > 128 * 1024)
          return finish(
            failure("request_too_large", "This request is too large.", 413),
          );
        try {
          body = text ? JSON.parse(text) : {};
        } catch {
          return finish(failure("invalid_json", "Invalid request."));
        }
      }
      if (path === "/api/session" && request.method === "GET") {
        const slot = session[session.active];
        if (slot?.access) {
          const me = await authorized("/api/v1/auth/me", slot);
          if (me.status === 401) {
            delete slot.access;
            delete slot.refresh;
            delete slot.user;
          } else if (!me.ok) return finish(me);
          else slot.user = await me.json();
        }
        return finish(json(snapshot(session)));
      }
      if (path === "/auth/start" && request.method === "GET") {
        const state = id();
        session.pending = { state, until: Date.now() + 600_000 };
        const callback = new URL("/auth/callback", origin);
        callback.searchParams.set("state", state);
        const destination = new URL(
          url.searchParams.get("intent") === "signup" ? "/signup" : "/login",
          iam,
        );
        destination.searchParams.set("app_id", appId);
        // Login is unscoped; discover the workspace from verified IAM authority.
        destination.searchParams.set("redirect_uri", callback.href);
        return finish(Response.redirect(destination, 303));
      }
      if (path === "/auth/callback" && request.method === "GET") {
        const pending = session.pending;
        delete session.pending;
        let error;
        if (
          !pending ||
          pending.until < Date.now() ||
          pending.state !== url.searchParams.get("state")
        )
          error = "Sign-in expired. Please start again.";
        else {
          const slot = {};
          const failed = await login(slot, url.searchParams.get("slt"));
          if (failed)
            error = "IAM could not finish sign-in. Please try a fresh code.";
          else {
            session.production = slot;
            session.active = "production";
            session.context = id();
          }
        }
        const destination = new URL("/", origin);
        if (error) destination.searchParams.set("auth_error", error);
        return finish(Response.redirect(destination, 303));
      }
      if (path === "/api/session/login" && request.method === "POST") {
        const org = body.org || session[session.active]?.org || "tos";
        if (!/^[a-zA-Z0-9_-]{1,128}$/.test(org))
          return finish(
            failure("invalid_organization", "Enter an organization handle."),
          );
        const slot = { ...session[session.active], org };
        delete slot.access;
        delete slot.refresh;
        delete slot.user;
        const failed = await login(slot, body.slt);
        if (failed) return finish(failed);
        session[session.active] = slot;
        session.context = id();
        return finish(json(snapshot(session)));
      }
      if (path === "/api/session/environment" && request.method === "POST") {
        if (!/^[A-Za-z0-9]{32}$/.test(body.key || ""))
          return finish(
            failure(
              "invalid_environment_key",
              "Enter the 32-character Waveform environment key.",
            ),
          );
        const slot = { key: body.key, org: body.org || "tos" };
        const response = await upstream("/api/v1/testing-environment", slot);
        if (!response.ok) return finish(response);
        slot.environment = await response.json();
        session.test = slot;
        session.active = "test";
        session.context = id();
        return finish(json(snapshot(session)));
      }
      if (path === "/api/session/switch" && request.method === "POST") {
        if (
          !["production", "test"].includes(body.plane) ||
          !session[body.plane]
        )
          return finish(
            failure(
              "environment_required",
              "Connect a test environment first.",
            ),
          );
        session.active = body.plane;
        session.context = id();
        return finish(json(snapshot(session)));
      }
      if (path === "/api/session/refresh" && request.method === "POST") {
        if (!(await refresh(session[session.active])))
          return finish(
            failure(
              "sign_in_required",
              "Sign in again to refresh this session.",
              401,
            ),
          );
        return finish(json(snapshot(session)));
      }
      if (path === "/api/session/logout" && request.method === "POST") {
        const slot = session[session.active];
        if (slot?.refresh) {
          const result = await upstream("/api/v1/auth/logout", slot, "POST", {
            token: slot.refresh,
          });
          if (!result.ok && result.status !== 401) return finish(result);
        }
        if (session.active === "test") {
          delete session.test;
          session.active = "production";
        } else session.production = { org: slot?.org || "tos" };
        session.context = id();
        return finish(json(snapshot(session)));
      }
      if (
        !routes.some(
          ([method, pattern]) =>
            method === request.method && pattern.test(path),
        )
      )
        return finish(
          failure("not_found", "This operation is not available.", 404),
        );
      const slot = session[session.active];
      const isPublic =
        path.startsWith("/health/") || path === "/api/v1/capabilities";
      const rootOnly =
        path === "/api/v1/testing-environment" ||
        path === "/api/v1/testing-environment/clean";
      if (!isPublic && !rootOnly && !slot?.access)
        return finish(
          failure(
            "sign_in_required",
            "Sign in with Silicon IAM to continue.",
            401,
          ),
        );
      if (rootOnly && !slot?.key)
        return finish(
          failure(
            "test_environment_required",
            "Connect a test environment first.",
          ),
        );
      const extra = {};
      for (const name of ["idempotency-key", "x-request-id"])
        if (request.headers.has(name)) extra[name] = request.headers.get(name);
      const response = await authorized(
        path + url.search,
        isPublic ? undefined : slot,
        request.method,
        body,
        extra,
      );
      // Upstream cookies and redirects must never become browser credentials.
      const headers = new Headers();
      for (const name of [
        "content-type",
        "retry-after",
        "x-request-id",
        "x-idempotent-replay",
      ])
        if (response.headers.has(name))
          headers.set(name, response.headers.get(name));
      return finish(
        new Response(response.body, { status: response.status, headers }),
      );
    } catch {
      return finish(
        failure(
          "upstream_unavailable",
          "Waveform could not reach the service. Try again; your request can be retried safely.",
          502,
        ),
      );
    }
  };
}
