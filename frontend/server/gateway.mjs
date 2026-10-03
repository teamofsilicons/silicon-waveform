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
  ["POST", /^\/api\/v1\/storage-authorizations$/],
  ["GET", /^\/api\/v1\/storage-authorizations\/[0-9a-f-]{36}$/],
  ["POST", /^\/api\/v1\/storage-authorizations\/[0-9a-f-]{36}\/complete$/],
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
      else {
        // Legacy slots lack a trustworthy account+organization context. Keep
        // test selectors, but require a new login for pre-migration bearers.
        for (const plane of ["production", "test"]) {
          const slot = session[plane];
          if (slot?.access && (!slot.contextId || !slot.org || !slot.user))
            session[plane] = {
              key: slot.key,
              environment: slot.environment,
              org: slot.org,
            };
        }
        sessions.set(session.id, session);
      }
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
  const slots = (session) =>
    [
      { plane: "production", slot: session.production },
      { plane: "test", slot: session.test },
      ...Object.values(session.saved || {}),
    ].filter((entry) => entry.slot);
  const currentSlot = (session, slot) =>
    !slot.revoked && slots(session).some((entry) => entry.slot === slot);
  const sameIdentity = (plane, a, b) =>
    !!a?.user &&
    !!b?.user &&
    a.user.actor_type === b.user.actor_type &&
    a.user.public_id === b.user.public_id &&
    a.org === b.org &&
    (plane === "production" ||
      (a.environment?.id === b.environment?.id && a.key === b.key));
  function installSlot(session, plane, next, popupReceipt) {
    const previous = session[plane],
      saved = { ...session.saved },
      retired = [];
    if (previous === next) {
      updateSession(session, { active: plane, context: id() });
      return;
    }
    if (previous?.refresh || previous?.key) {
      if (
        sameIdentity(plane, previous, next) ||
        (!previous.user && previous.key === next.key)
      )
        retired.push(previous);
      else {
        previous.contextId ||= id();
        saved[previous.contextId] = { plane, slot: previous };
      }
    }
    for (const [contextId, entry] of Object.entries(saved)) {
      if (
        entry.slot === next ||
        (entry.plane === plane && sameIdentity(plane, entry.slot, next))
      ) {
        if (entry.slot !== next) retired.push(entry.slot);
        delete saved[contextId];
      }
    }
    const context = id();
    updateSession(session, {
      saved,
      [plane]: next,
      active: plane,
      ...(popupReceipt ? { popupReceipt: { ...popupReceipt, context } } : {}),
      context,
      until: Date.now() + SESSION_TTL_MS,
    });
    for (const slot of retired) slot.revoked = true;
  }
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
    typeof value.org_id === "string" &&
    /^[a-z0-9][a-z0-9-]{0,254}$/.test(value.org_id);
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
  async function login(session, slot, slt, expectedKind) {
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
    const me = await upstream("/api/v1/auth/me", { ...slot, org: undefined });
    if (!me.ok) return me;
    slot.user = await me.json();
    if (!identity(slot.user)) throw new Error("Invalid account response");
    if (expectedKind && slot.user.actor_type !== expectedKind) {
      clearSlot(slot);
      return failure("identity_kind_mismatch", "Choose the same account type you selected in Waveform.", 403);
    }
    slot.org = slot.user.org_id;
    slot.contextId = id();
    delete session.loginAttempt;
    return null;
  }
  const popupLocation = (nonce, success) => {
    const target = new URL("/", origin);
    target.searchParams.set("iam_popup", "complete");
    target.searchParams.set("nonce", nonce);
    target.searchParams.set("result", success ? "ok" : "error");
    return target.href;
  };
  const storageIdentity = (session, slot, pending) =>
    pending && pending.until > Date.now() && pending.slotId === slot?.contextId &&
    pending.plane === session.active && pending.org === slot?.org &&
    pending.actor === slot?.user?.public_id && pending.kind === slot?.user?.actor_type &&
    !!slot?.access && currentSlot(session, slot);
  const storageContext = (session, slot, pending) =>
    storageIdentity(session, slot, pending) && !pending.cancelled && pending.context === session.context;
  async function completeStorage(session, slot, pending) {
    const response = await authorized(
      `/api/v1/storage-authorizations/${pending.authorizationId}/complete`, session, slot,
      "POST", { code: pending.code, state: pending.state },
      { "idempotency-key": pending.startKey + "-complete" },
    );
    if (!response.ok) {
      const error = await response.clone().json().catch(() => null);
      if (response.status === 412 || (response.status === 400 && ["invalid_storage_authorization", "storage_authorization_expired"].includes(error?.error?.code))) delete slot.storagePending;
      return response;
    }
    const value = await response.json();
    if (value.authorization_id !== pending.authorizationId || value.state !== pending.state || value.status !== "completed")
      return failure("approval_pending", "Approval could not be verified. Retry the original request.", 409);
    pending.completed = true;
    pending.codeDigest = createHash("sha256").update(pending.code).digest("hex");
    delete pending.code;
    slot.storageCompleted = pending;
    delete slot.storagePending;
    return null;
  }
  const storageView = pending => ({
    authorization_id: pending.authorizationId, state: pending.state,
    consent_url: pending.completed ? null : pending.consentUrl,
    redirect_url: pending.completed && pending.popupNonce ? popupLocation(pending.popupNonce, true) : pending.consentUrl,
    status: pending.completed ? "completed" : "pending",
    expires_at: new Date(pending.until).toISOString(), manual: pending.manual,
    code_saved: !!pending.code,
  });
  const snapshot = (s) => ({
    authenticated: !!s[s.active]?.access,
    user: s[s.active]?.user ?? null,
    org: s[s.active]?.org ?? "tos",
    plane: s.active,
    environment: s.test?.environment ?? null,
    productionAvailable: !!s.production?.access,
    testAvailable: !!s.test?.key,
    context: s.context,
    contextId: s[s.active]?.contextId,
    contexts: slots(s)
      .filter(({ slot }) => slot.access && !slot.revoked)
      .map(({ plane, slot }) => ({
        id: slot.contextId,
        plane,
        user: slot.user,
        org: slot.org,
        environment: slot.environment ?? null,
      })),
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
                  user.actor_type !== slot.user.actor_type ||
                  user.org_id !== slot.org))
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
      if (path === "/api/session/storage/start" && request.method === "POST") {
        const slot = session[session.active];
        if (!slot?.access || !identity(slot.user)) return finish(failure("sign_in_required", "Sign in to approve Briefcase access.", 401));
        if (body.popup_nonce !== undefined && !/^[a-f0-9]{64}$/.test(body.popup_nonce)) return finish(failure("invalid_popup", "Start approval from Waveform."));
        const reviewNonce = body.review_nonce || body.popup_nonce;
        if (reviewNonce !== undefined && !/^[a-f0-9]{64}$/.test(reviewNonce)) return finish(failure("invalid_review", "Start approval from Waveform."));
        if (session.storageCancelled?.some(entry => entry.nonce === reviewNonce && entry.until > Date.now())) return finish(failure("approval_cancelled", "This review was cancelled.", 409));
        let pending = slot.storagePending;
        if (storageIdentity(session, slot, pending)) {
          pending.popupNonce = body.popup_nonce;
          pending.context = session.context;
          pending.cancelled = false;
          pending.reviewNonce = reviewNonce;
        } else {
          pending = {
            startKey: id(), context: session.context, slotId: slot.contextId,
            plane: session.active, org: slot.org, actor: slot.user.public_id,
            kind: slot.user.actor_type, popupNonce: body.popup_nonce, manual: !body.popup_nonce, reviewNonce, until: Date.now() + 600_000,
          };
          slot.storagePending = pending;
        }
        // Persist the logical start before calling IAM through the backend. A
        // lost response or gateway restart must reuse this operation's key.
        persist(session);
        if (pending.code) {
          const failed = await completeStorage(session, slot, pending);
          if (failed) return finish(failed);
          return finish(json(storageView(pending)));
        }
        if (!pending.authorizationId) {
          const response = await authorized("/api/v1/storage-authorizations", session, slot, "POST", pending.manual ? {} : { redirect_uri: new URL("/auth/storage/callback", origin).href }, { "idempotency-key": pending.startKey });
          if (!response.ok) {
            const error = await response.clone().json().catch(() => null);
            if (response.status === 412 || (response.status === 400 && ["invalid_storage_authorization", "storage_authorization_expired"].includes(error?.error?.code))) delete slot.storagePending;
            return finish(response);
          }
          const value = await response.json();
          if (typeof value.authorization_id !== "string" || !/^[a-f0-9-]{36}$/.test(value.authorization_id) || typeof value.state !== "string" || !value.state || typeof value.consent_url !== "string" || value.status !== "pending") return finish(failure("invalid_approval", "IAM returned an invalid approval request.", 502));
          const consent = new URL(value.consent_url);
          if ((consent.protocol !== "https:" && !(consent.protocol === "http:" && ["localhost", "127.0.0.1", "[::1]"].includes(consent.hostname))) || consent.username || consent.password || consent.hash) return finish(failure("invalid_approval", "IAM returned an invalid approval address.", 502));
          if (body.popup_nonce) consent.searchParams.set("display", "popup");
          const until = Math.min(pending.until, Date.parse(value.expires_at));
          if (!Number.isFinite(until) || until <= Date.now()) return finish(failure("invalid_approval", "IAM returned an invalid expiry.", 502));
          Object.assign(pending, { authorizationId: value.authorization_id, state: value.state, consentUrl: consent.href, until });
        }
        return finish(json(storageView(pending)));
      }
      if (path === "/api/session/storage/cancel" && request.method === "POST") {
        const slot = session[session.active], pending = slot?.storagePending;
        // Cancel only this browser wait. Keep the durable request/key/code and
        // any existing grant; a late callback cannot finish a dismissed review.
        if (!/^[a-f0-9]{64}$/.test(body.review_nonce || "")) return finish(failure("invalid_review", "Cancel this review from Waveform."));
        session.storageCancelled = [...(session.storageCancelled || []).filter(entry => entry.until > Date.now()), { nonce: body.review_nonce, until: Date.now() + 600_000 }].slice(-32);
        if (storageIdentity(session, slot, pending) && body.review_nonce === pending.reviewNonce) pending.cancelled = true;
        return finish(json({ cancelled: true }));
      }
      if (path === "/api/session/storage/status" && request.method === "POST") {
        const slot = session[session.active], pending = slot?.storagePending || slot?.storageCompleted;
        if (!storageIdentity(session, slot, pending) || body.authorization_id !== pending.authorizationId || body.state !== pending.state)
          return finish(failure("approval_expired", "Resume approval in the original workspace.", 403));
        return finish(json(storageView(pending)));
      }
      if (path === "/api/session/storage/complete" && request.method === "POST") {
        const slot = session[session.active], pending = slot?.storagePending || slot?.storageCompleted;
        if (!storageContext(session, slot, pending) || body.authorization_id !== pending.authorizationId || body.state !== pending.state)
          return finish(failure("approval_expired", "Resume approval in the original workspace.", 403));
        if (body.code !== undefined && !/^obc_[!-~]{1,16380}$/.test(body.code)) return finish(failure("invalid_code", "Paste the one-use approval code from IAM."));
        if (pending.completed) {
          if (body.code && createHash("sha256").update(body.code).digest("hex") !== pending.codeDigest) return finish(failure("approval_code_changed", "Retry the original approval code.", 409));
          return finish(json(storageView(pending)));
        }
        if (pending.code && body.code && pending.code !== body.code) return finish(failure("approval_code_changed", "Retry the saved approval code.", 409));
        pending.code ||= body.code;
        if (!pending.code) return finish(failure("invalid_code", "Paste the one-use approval code from IAM."));
        persist(session);
        const failed = await completeStorage(session, slot, pending);
        if (failed) return finish(failed);
        return finish(json(storageView(pending)));
      }
      if (path === "/auth/storage/callback" && request.method === "GET") {
        const slot = session[session.active], pending = slot?.storagePending;
        if (!storageContext(session, slot, pending) || pending.state !== url.searchParams.get("state")) return finish(failure("approval_expired", "This approval no longer belongs to the current workspace. Return to Waveform and start again.", 403));
        const code = url.searchParams.get("code");
        if (url.searchParams.get("error") || !code || !/^obc_[!-~]{1,16380}$/.test(code)) {
          delete slot.storagePending;
          return finish(Response.redirect(popupLocation(pending.popupNonce, false), 303));
        }
        // Keep the one-use code encrypted before exchange so an uncertain
        // completion retries its original operation and authority after restart.
        if (pending.code && pending.code !== code) return finish(failure("approval_code_changed", "Retry the original approval callback.", 409));
        pending.code = code;
        persist(session);
        let success = false;
        try { success = !(await completeStorage(session, slot, pending)); } catch { /* The encrypted receipt remains retryable. */ }
        return finish(Response.redirect(popupLocation(pending.popupNonce, success), 303));
      }
      if (path === "/auth/cancel" && request.method === "POST") {
        if (!/^[a-f0-9]{64}$/.test(body.nonce || "")) return finish(failure("invalid_popup", "Cancel sign-in from Waveform."));
        session.loginCancelled = [...(session.loginCancelled || []).filter(entry => entry.until > Date.now()), { nonce: body.nonce, until: Date.now() + 600_000 }].slice(-32);
        if (session.pending?.popupNonce === body.nonce) delete session.pending;
        const receipt = session.popupReceipt;
        if (receipt?.nonce === body.nonce) {
          if (receipt.context === session.context) {
            const previous = slots(session).find(({ slot }) => slot.contextId === receipt.beforeId && !slot.revoked);
            if (previous) installSlot(session, previous.plane, previous.slot);
            else {
              const selected = session[session.active];
              const same = session.active === receipt.beforePlane && selected?.user?.public_id === receipt.beforeActor && selected?.user?.actor_type === receipt.beforeKind && selected?.org === receipt.beforeOrg;
              if (!same) installSlot(session, receipt.beforePlane, { org: receipt.beforeOrg || "tos" });
            }
          }
          delete session.popupReceipt;
        }
        return finish(json(snapshot(session)));
      }
      if (path === "/auth/start" && request.method === "GET") {
        const identityKind = url.searchParams.get("identity_kind");
        const popupNonce = url.searchParams.get("popup_nonce");
        if ((identityKind && !["carbon", "silicon"].includes(identityKind)) ||
            (popupNonce && (!identityKind || !/^[a-f0-9]{64}$/.test(popupNonce))))
          return finish(failure("invalid_login", "Choose Carbon or Silicon to sign in."));
        if (popupNonce && session.loginCancelled?.some(entry => entry.nonce === popupNonce && entry.until > Date.now())) return finish(failure("login_cancelled", "This sign-in was cancelled.", 409));
        const state = id();
        delete session.popupReceipt;
        session.pending = { state, identityKind, popupNonce, context: session.context, plane: session.active, until: Date.now() + 600_000 };
        const callback = new URL("/auth/callback", origin);
        callback.searchParams.set("state", state);
        const destination = new URL(
          url.searchParams.get("intent") === "signup" ? "/signup" : "/login",
          iam,
        );
        destination.searchParams.set("app_id", appId);
        if (identityKind) destination.searchParams.set("identity_kind", identityKind);
        if (popupNonce) destination.searchParams.set("display", "popup");
        // IAM chooses one account and organization; verify that exact authority.
        destination.searchParams.set("redirect_uri", callback.href);
        return finish(Response.redirect(destination, 303));
      }
      if (path === "/auth/callback" && request.method === "GET") {
        const pending = session.pending?.state === url.searchParams.get("state") ? session.pending : undefined;
        if (pending) delete session.pending;
        let error;
        if (
          !pending ||
          pending.until < Date.now() ||
          pending.state !== url.searchParams.get("state") ||
          pending.context !== session.context || pending.plane !== session.active
        )
          error = "Sign-in expired. Please start again.";
        else {
          const slot = {};
          const failed = await login(
            session,
            slot,
            url.searchParams.get("slt"),
            pending.identityKind,
          );
          if (failed)
            error = "IAM could not finish sign-in. Please try a fresh code.";
          else {
            const previous = session[session.active];
            installSlot(session, "production", slot, pending.popupNonce ? {
              nonce: pending.popupNonce, beforeId: previous?.contextId, beforePlane: session.active,
              beforeActor: previous?.user?.public_id, beforeKind: previous?.user?.actor_type, beforeOrg: previous?.org,
            } : undefined);
          }
        }
        const destination = new URL("/", origin);
        if (pending?.popupNonce) {
          destination.searchParams.set("iam_popup", "complete");
          destination.searchParams.set("nonce", pending.popupNonce);
          destination.searchParams.set("result", error ? "error" : "ok");
        } else if (error) destination.searchParams.set("auth_error", error);
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
        installSlot(session, session.active, slot);
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
        slot.contextId = id();
        installSlot(session, "test", slot);
        return finish(json(snapshot(session)));
      }
      if (path === "/api/session/context" && request.method === "POST") {
        const selected = slots(session).find(
          ({ slot }) =>
            slot.contextId === body.context_id && slot.access && !slot.revoked,
        );
        if (!selected)
          return finish(
            failure(
              "context_not_found",
              "That saved workspace is unavailable.",
              404,
            ),
          );
        installSlot(session, selected.plane, selected.slot);
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
      if (
        request.headers.has("x-org-id") &&
        request.headers.get("x-org-id") !== slot?.org
      )
        return finish(
          failure(
            "organization_context_mismatch",
            "Sign in to this organization separately.",
            409,
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
