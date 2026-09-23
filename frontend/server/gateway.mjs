import { createHash, randomBytes } from "node:crypto";
import {
  SessionStore,
  SESSION_TTL_MS,
  COOKIE_TTL_MS,
} from "./session-store.mjs";

// Match the backend's expanded JSON contract for provider controls and BYOK.
export const MAX_REQUEST_BODY_BYTES = 327_680;

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
  ["POST", /^\/api\/v1\/telemetry$/],
  ["GET", /^\/api\/v1\/testing-environment$/],
  ["POST", /^\/api\/v1\/testing-environment\/clean$/],
];
const id = () => randomBytes(32).toString("hex");
class UpstreamFailure extends Error {
  constructor(response) {
    super("Upstream request failed");
    this.response = response;
  }
}
export function createGateway({
  backend = "https://backend.waveform.teamofsilicons.com",
  origin = "http://localhost:4325",
  iam = "https://auth.iam.teamofsilicons.com",
  appId = "waveform",
  fetcher = fetch,
  sessionDirectory,
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
  const store = sessionDirectory
    ? new SessionStore(sessionDirectory, { backend, origin, iam, appId })
    : undefined;
  try {
    for (const session of store?.load() || []) {
      if (session.until <= Date.now()) store.delete(session.id);
      else sessions.set(session.id, session);
    }
  } catch (error) {
    store?.close();
    throw error;
  }
  const persist = (session) => {
    if (sessions.get(session.id) === session) store?.save(session);
  };
  const updateSession = (session, changes) => {
    const previous = { ...session };
    Object.assign(session, changes);
    try {
      persist(session);
    } catch (error) {
      for (const key of Object.keys(session))
        if (!Object.hasOwn(previous, key)) delete session[key];
      Object.assign(session, previous);
      throw error;
    }
  };
  const currentSlot = (session, slot) =>
    !slot.revoked && [session.production, session.test].includes(slot);
  const clearSlot = (slot) => {
    for (const key of [
      "access",
      "refresh",
      "user",
      "refreshKey",
      "refreshStarted",
      "pendingTokens",
    ])
      delete slot[key];
  };
  const identity = (value) =>
    typeof value?.public_id === "string" &&
    ["carbon", "silicon"].includes(value.actor_type) &&
    typeof value.org_id === "string";
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
  function tokens(slot, payload, started = Date.now()) {
    if (
      typeof payload.access_token !== "string" ||
      !payload.access_token ||
      typeof payload.refresh_token !== "string" ||
      !payload.refresh_token ||
      !Number.isSafeInteger(payload.expires_in) ||
      payload.expires_in <= 0 ||
      !Number.isFinite(started + payload.expires_in * 1000)
    )
      throw new Error("Invalid token response");
    slot.access = payload.access_token;
    slot.refresh = payload.refresh_token;
    slot.expires = started + payload.expires_in * 1000;
  }
  async function refresh(session, slot, recover = true) {
    if (!slot?.refresh || !currentSlot(session, slot)) return false;
    if (!slot.refreshing)
      slot.refreshing = (async () => {
        if (!slot.refreshKey) {
          slot.refreshKey = id();
          slot.refreshStarted = Date.now();
        }
        persist(session);
        if (!slot.pendingTokens) {
          const response = await upstream(
            "/api/v1/auth/refresh",
            { ...slot, access: undefined },
            "POST",
            { refresh_token: slot.refresh },
            { "idempotency-key": slot.refreshKey },
          );
          if (!response.ok) {
            const error = await response
              .clone()
              .json()
              .catch(() => ({}));
            const clientFailure = [
              "invalid_client",
              "invalid_app_secret",
              "invalid_application_secret",
              "application_disabled",
              "configuration_required",
            ].includes(error.error?.code);
            if (
              (response.status === 401 && !clientFailure) ||
              (response.status === 400 &&
                [
                  "invalid_grant",
                  "invalid_refresh_token",
                  "session_expired",
                ].includes(error.error?.code))
            ) {
              clearSlot(slot);
              persist(session);
              return false;
            }
            throw new UpstreamFailure(response);
          }
          const pending = {};
          // Persist the successor before dependent identity reads. Recovery must
          // not depend on the issuer retaining its idempotent response forever.
          tokens(pending, await response.json(), slot.refreshStarted ?? 0);
          if (!currentSlot(session, slot)) return false;
          slot.pendingTokens = pending;
          try {
            persist(session);
          } catch (error) {
            delete slot.pendingTokens;
            throw error;
          }
        }
        const next = { ...slot, ...slot.pendingTokens };
        if (next.expires > Date.now() + 30_000) {
          const me = await upstream("/api/v1/auth/me", next);
          if (!me.ok) {
            if (me.status !== 401) throw new UpstreamFailure(me);
            if (!recover)
              throw new UpstreamFailure(
                failure(
                  "verification_unavailable",
                  "The renewed account could not be verified. Retry shortly.",
                  503,
                ),
              );
            // Access rejection does not revoke the refresh family. Keep the
            // staged successor and renew it once before trying identity again.
            next.expires = 0;
          } else {
            next.user = await me.json();
            if (!identity(next.user))
              throw new Error("Invalid account response");
            if (
              slot.user &&
              (next.user.public_id !== slot.user.public_id ||
                next.user.actor_type !== slot.user.actor_type ||
                next.user.org_id !== slot.org)
            ) {
              clearSlot(slot);
              persist(session);
              throw new UpstreamFailure(
                failure(
                  "identity_mismatch",
                  "The renewed login belongs to a different account. Sign in again.",
                  401,
                ),
              );
            }
          }
        }
        if (!currentSlot(session, slot)) return false;
        const previous = { ...slot };
        Object.assign(slot, next, {
          refreshKey: undefined,
          refreshStarted: undefined,
          pendingTokens: undefined,
        });
        try {
          persist(session);
        } catch (error) {
          Object.assign(slot, previous);
          throw error;
        }
        return true;
      })().finally(() => {
        delete slot.refreshing;
      });
    const result = await slot.refreshing;
    if (result && slot.expires <= Date.now() + 30_000) {
      if (recover) return refresh(session, slot, false);
      throw new UpstreamFailure(
        failure(
          "refresh_unavailable",
          "The renewed access has already expired. Retry shortly.",
          503,
        ),
      );
    }
    return result;
  }
  async function authorized(path, session, slot, method, body, headers) {
    if (
      slot?.access &&
      (slot.refreshKey || slot.expires < Date.now() + 30_000) &&
      !(await refresh(session, slot))
    )
      return failure(
        "sign_in_required",
        "Sign in with Silicon IAM to continue.",
        401,
      );
    const usedAccess = slot?.access;
    let response = await upstream(path, slot, method, body, headers);
    if (response.status === 401 && slot?.refresh) {
      await response.body?.cancel();
      if (slot.access === usedAccess && !(await refresh(session, slot)))
        return failure(
          "sign_in_required",
          "Sign in with Silicon IAM to continue.",
          401,
        );
      if (!currentSlot(session, slot))
        return failure(
          "workspace_changed",
          "Reload the current workspace.",
          409,
        );
      if (
        ["GET", "HEAD"].includes(method || "GET") ||
        new Headers(headers).has("idempotency-key")
      )
        response = await upstream(path, slot, method, body, headers);
      else
        return failure(
          "retry_same_operation",
          "Your connection was renewed. Retry the same action.",
          409,
        );
    }
    return response;
  }
  async function login(session, slot, slt) {
    if (
      typeof slt !== "string" ||
      !(slot.key
        ? /^[!-~]{1,256}$/.test(slt) || /^oac_[!-~]{1,16380}$/.test(slt)
        : /^oac_[!-~]{1,16380}$/.test(slt))
    )
      return failure("invalid_token", "Enter a valid short-lived IAM code.");
    const fingerprint = createHash("sha256")
      .update(JSON.stringify([slot.key || "production", slot.org, slt]))
      .digest("hex");
    if (session.loginAttempt?.fingerprint !== fingerprint)
      session.loginAttempt = { fingerprint, key: id(), started: Date.now() };
    persist(session);
    const response = await upstream(
      "/api/v1/auth/login",
      { ...slot, org: undefined },
      "POST",
      {
        slt,
      },
      { "idempotency-key": session.loginAttempt.key },
    );
    if (!response.ok) return response;
    tokens(slot, await response.json(), session.loginAttempt.started);
    const me = await upstream("/api/v1/auth/me", slot);
    if (!me.ok) return me;
    slot.user = await me.json();
    if (!identity(slot.user)) throw new Error("Invalid account response");
    slot.org = slot.user.org_id;
    delete session.loginAttempt;
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
  const handle = async function handle(request) {
    let sessionId, session, releaseChange, changing;
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
      if (session) persist(session);
      if (sessionId)
        headers.set(
          "set-cookie",
          `${cookieName}=${sessionId}; Path=/; HttpOnly; SameSite=Lax; Max-Age=${Math.max(0, Math.floor(Math.min(COOKIE_TTL_MS, session.until - Date.now()) / 1000))}${secure ? "; Secure" : ""}`,
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
        if (s.until < Date.now()) {
          store?.delete(key);
          sessions.delete(key);
        }
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
          id: sessionId,
          context: id(),
          active: "production",
          production: { org: "tos" },
          until: Date.now() + 86400_000,
        };
        sessions.set(sessionId, session);
      }
      // Serialize account/environment changes; resource calls retain their bound slot.
      if (
        path.startsWith("/auth/") ||
        (path.startsWith("/api/session/") && request.method !== "GET")
      ) {
        const previous = session.changing;
        changing = new Promise((resolve) => {
          releaseChange = resolve;
        });
        session.changing = changing;
        if (previous) await previous;
      }
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
        if (Buffer.byteLength(text) > MAX_REQUEST_BODY_BYTES)
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
          const me = await authorized("/api/v1/auth/me", session, slot);
          if (!me.ok && slot.refresh) return finish(me);
          else if (!me.ok && me.status !== 401) return finish(me);
          else if (me.ok) {
            const user = await me.json();
            if (
              !identity(user) ||
              (slot.user &&
                (user.public_id !== slot.user.public_id ||
                  user.actor_type !== slot.user.actor_type))
            ) {
              clearSlot(slot);
              return finish(
                failure(
                  "identity_mismatch",
                  "Sign in again to verify your account.",
                  401,
                ),
              );
            }
            slot.user = user;
          }
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
          const failed = await login(
            session,
            slot,
            url.searchParams.get("slt"),
          );
          if (failed)
            error = "IAM could not finish sign-in. Please try a fresh code.";
          else {
            const previous = session.production;
            updateSession(session, {
              production: slot,
              until: Date.now() + SESSION_TTL_MS,
              active: "production",
              context: id(),
            });
            if (previous) previous.revoked = true;
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
        const previous = session[session.active];
        const slot = {
          org,
          ...(previous?.key
            ? { key: previous.key, environment: previous.environment }
            : {}),
        };
        const failed = await login(session, slot, body.slt);
        if (failed) return finish(failed);
        updateSession(session, {
          [session.active]: slot,
          until: Date.now() + SESSION_TTL_MS,
          context: id(),
        });
        if (previous) previous.revoked = true;
        return finish(json(snapshot(session)));
      }
      if (path === "/api/session/environment" && request.method === "POST") {
        if (!/^(?:ask_[A-Za-z0-9_-]{43}|[A-Za-z0-9]{32})$/.test(body.key || ""))
          return finish(
            failure(
              "invalid_environment_key",
              "Enter the IAM testing app_secret for Waveform (ask_…).",
            ),
          );
        const slot = { key: body.key, org: body.org || "tos" };
        const response = await upstream("/api/v1/testing-environment", slot);
        if (!response.ok) return finish(response);
        slot.environment = await response.json();
        const previous = session.test;
        updateSession(session, {
          test: slot,
          until: Date.now() + SESSION_TTL_MS,
          active: "test",
          context: id(),
        });
        if (previous) previous.revoked = true;
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
        updateSession(session, { active: body.plane, context: id() });
        return finish(json(snapshot(session)));
      }
      if (path === "/api/session/refresh" && request.method === "POST") {
        if (!(await refresh(session, session[session.active])))
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
        updateSession(session, {
          ...(session.active === "test"
            ? { test: undefined, active: "production" }
            : { production: { org: slot?.org || "tos" } }),
          context: id(),
          pending: undefined,
          loginAttempt: undefined,
        });
        if (slot) slot.revoked = true;
        if (slot?.refresh) {
          // Local logout is durable even when remote revocation is unavailable.
          await upstream(
            "/api/v1/auth/logout",
            slot,
            "POST",
            { token: slot.refresh },
            { "idempotency-key": id() },
          ).catch(() => {});
        }
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
      const rootOnly = path === "/api/v1/testing-environment";
      if (path === "/api/v1/testing-environment/clean" && !slot?.key)
        return finish(
          failure("test_environment_required", "Select a sandbox first.", 400),
        );
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
        session,
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
    } catch (error) {
      if (error instanceof UpstreamFailure) return finish(error.response);
      return finish(
        failure(
          "upstream_unavailable",
          "Waveform could not reach the service. If a speech request was submitted, its result may be uncertain; check job history before retrying.",
          502,
        ),
      );
    } finally {
      if (releaseChange) {
        if (session.changing === changing) delete session.changing;
        releaseChange();
      }
    }
  };
  handle.close = () => store?.close();
  return handle;
}
