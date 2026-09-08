import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
const voiceProfiles = JSON.parse(
  readFileSync(
    new URL("../../src/domain/voice_profiles.json", import.meta.url),
    "utf8",
  ),
);
const defaults = {
  voice_profile: "kore",
  tts_order: ["gemini", "elevenlabs", "openai"],
  stt_order: ["gemini", "openai", "deepgram"],
};
export function fixture() {
  const requests = [],
    environments = [],
    planes = new Map();
  const testKey = "WaveformUiTestEnvironmentKey1234";
  function plane(key) {
    if (!planes.has(key))
      planes.set(key, {
        preferences: structuredClone(defaults),
        keys: new Map(),
        jobs: [],
      });
    return planes.get(key);
  }
  const now = () => new Date().toISOString();
  const current = {
    id: "11111111-1111-4111-8111-111111111111",
    name: "UI verification",
    created_at: now(),
    last_activity_at: now(),
  };
  const actor = (key) => ({
    principal_id: "22222222-2222-4222-8222-222222222222",
    public_id: key ? "test-carbon" : "waveform-tester",
    actor_type: "carbon",
    org_id: "tos",
    org_role: "owner",
    scopes: [],
  });
  async function fetcher(input, options = {}) {
    const url = new URL(input),
      path = url.pathname,
      method = options.method || "GET",
      headers = new Headers(options.headers),
      body = options.body ? JSON.parse(options.body) : {};
    requests.push({ path, query: url.search, method, headers, body });
    const key = headers.get("x-testing-environment-key") || "",
      p = plane(key);
    const error = (code, status = 400) =>
      Response.json(
        { error: { code, message: code.replaceAll("_", " ") } },
        { status },
      );
    if (path.startsWith("/health/")) return Response.json({ status: "ready" });
    if (path === "/api/v1/capabilities")
      return Response.json({
        tts: { output_format: "audio/mpeg", languages: ["en", "hi", "es"] },
        stt: {
          languages: ["en", "hi", "es"],
          accepted_media_types: ["audio/mpeg", "audio/wav", "video/mp4"],
        },
      });
    if (path === "/api/v1/auth/login") {
      if (body.slt !== "oac_waveform_ui_fixture")
        return error("invalid_token", 401);
      return Response.json({
        access_token: key ? "oat_test_fixture" : "oat_production_fixture",
        refresh_token: key ? "ort_test_fixture" : "ort_production_fixture",
        expires_in: 3600,
      });
    }
    if (path === "/api/v1/auth/refresh")
      return Response.json({
        access_token: key ? "oat_test_refreshed" : "oat_production_refreshed",
        refresh_token: key ? "ort_test_refreshed" : "ort_production_refreshed",
        expires_in: 3600,
      });
    if (path === "/api/v1/auth/logout")
      return new Response(null, { status: 204 });
    if (path === "/api/v1/testing-environment")
      return key === testKey ||
        environments.some((e) => e.key === key && !e.deleted_at)
        ? Response.json(environments.find((e) => e.key === key) || current)
        : error("invalid_environment_key", 401);
    if (path === "/api/v1/testing-environment/clean") {
      p.jobs = [];
      p.keys.clear();
      p.preferences = structuredClone(defaults);
      return Response.json({ cleaned: true });
    }
    if (!headers.has("authorization")) return error("sign_in_required", 401);
    if (path === "/api/v1/auth/me") return Response.json(actor(key));
    if (path === "/api/v1/voice-profiles")
      return Response.json({ items: voiceProfiles });
    if (path === "/api/v1/preferences") {
      if (method === "PATCH") {
        if (
          body.voice_profile &&
          !voiceProfiles.some((p) => p.id === body.voice_profile)
        )
          return error("invalid_voice_profile");
        p.preferences = {
          ...p.preferences,
          ...Object.fromEntries(
            Object.entries(body).filter(([, v]) => v != null),
          ),
        };
      }
      return Response.json({ ...p.preferences, defaults });
    }
    if (path === "/api/v1/provider-keys")
      return Response.json({
        items: [...p.keys].map(([provider]) => ({
          provider,
          configured: true,
          updated_at: now(),
        })),
      });
    if (path.startsWith("/api/v1/provider-keys/")) {
      const provider = path.split("/").at(-1);
      if (method === "PUT") p.keys.set(provider, body.api_key);
      else p.keys.delete(provider);
      return new Response(null, { status: 204 });
    }
    if (["/api/v1/tts", "/api/v1/stt"].includes(path)) {
      const operation = path.endsWith("tts") ? "tts" : "stt";
      const existing = p.jobs.find(
        (j) => j.key === headers.get("idempotency-key"),
      );
      if (existing) return Response.json(existing.result);
      const id = headers.get("x-request-id") || randomUUID();
      const provider = (body.provider_order ||
        p.preferences[operation + "_order"])[0];
      const result =
        operation === "tts"
          ? {
              request_id: id,
              voice_profile: {
                id: body.voice_profile || p.preferences.voice_profile,
                revision: 1,
              },
              provider,
              duration_ms: 2800,
              media_type: "audio/mpeg",
              file_url: `https://briefcase.teamofsilicons.com/files/${id}`,
              temporary_url: null,
            }
          : {
              request_id: id,
              provider,
              duration_ms: 3200,
              transcript:
                "Hey, this is the test environment of silicon waveform. To agents and humans.",
              detected_language: "en",
            };
      p.jobs.unshift({
        id,
        operation,
        status: "completed",
        voice_profile: result.voice_profile || null,
        first_line: body.text || result.transcript,
        duration_ms: result.duration_ms,
        provider,
        error_code: null,
        created_at: now(),
        finished_at: now(),
        key: headers.get("idempotency-key"),
        result,
      });
      return Response.json(result);
    }
    if (path === "/api/v1/jobs") {
      const filtered = p.jobs.filter(
        (j) =>
          !url.searchParams.get("operation") ||
          j.operation === url.searchParams.get("operation"),
      );
      const start = Number(url.searchParams.get("cursor") || 0);
      const limit = Number(url.searchParams.get("limit") || 20);
      return Response.json({
        items: filtered.slice(start, start + limit),
        next_cursor:
          filtered.length > start + limit ? String(start + limit) : null,
      });
    }
    if (path.startsWith("/api/v1/jobs/")) {
      const job = p.jobs.find((j) => j.id === path.split("/").at(-1));
      return job ? Response.json(job) : error("not_found", 404);
    }
    if (path === "/api/v1/testing-environments") {
      if (method === "POST") {
        const value = {
          id: randomUUID(),
          name: body.name,
          description: body.description,
          key: randomUUID().replaceAll("-", ""),
          created_at: now(),
          last_activity_at: now(),
          deleted_at: null,
        };
        environments.unshift(value);
        return Response.json(value);
      }
      return Response.json({ items: environments.map(({ key, ...e }) => e) });
    }
    const match = path.match(
      /^\/api\/v1\/testing-environments\/([^/]+)(?:\/(.+))?$/,
    );
    if (match) {
      const e = environments.find((x) => x.id === match[1]);
      if (!e) return error("not_found", 404);
      switch (match[2]) {
        case "key":
          return Response.json({ key: e.key });
        case "rotate-key":
          e.key = randomUUID().replaceAll("-", "");
          return Response.json({ key: e.key });
        case "delete":
          e.deleted_at = now();
          return Response.json({ deleted: true, recoverable_for_days: 30 });
        case "restore":
          e.deleted_at = null;
          return Response.json({ restored: true });
        default:
          return Response.json(e);
      }
    }
    return error("not_found", 404);
  }
  return { fetcher, requests, testKey, plane, environments };
}
