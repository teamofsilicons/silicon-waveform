import { createSignal } from "solid-js";
export type Provider = "gemini" | "elevenlabs" | "openai" | "deepgram";
export type Operation = "tts" | "stt";
export interface Identity {
  principal_id: string;
  public_id: string;
  actor_type: string;
  org_id: string;
  org_role: string | null;
  scopes: string[];
}
export interface Environment {
  id: string;
  name: string;
  description?: string | null;
  created_at: string;
  last_activity_at?: string;
  deleted_at?: string | null;
}
export interface Session {
  context: string;
  authenticated: boolean;
  user: Identity | null;
  org: string;
  plane: "production" | "test";
  environment: Environment | null;
  productionAvailable: boolean;
  testAvailable: boolean;
}
export interface VoiceProfile {
  id: string;
  name: string;
  description: string;
  revision: number;
  auditioned: boolean;
  gemini_voice: string;
  openai_voice: string;
  elevenlabs: {
    voice_id: string;
    model_id: string;
    voice_settings: {
      stability: number;
      similarity_boost: number;
      style: number;
      use_speaker_boost: boolean;
      speed: number;
    };
  };
}
export interface Preferences {
  voice_profile: string;
  tts_order: Provider[];
  stt_order: Provider[];
  defaults: {
    tts_order: Provider[];
    stt_order: Provider[];
    voice_profile: string;
  };
}
export interface Job {
  voice_profile?: { id: string; revision: number } | null;
  id: string;
  operation: Operation;
  status: "running" | "completed" | "failed";
  first_line: string;
  duration_ms: number | null;
  provider: Provider | null;
  error_code: string | null;
  created_at: string;
  finished_at: string | null;
}
export interface SpeechResult {
  voice_profile?: { id: string; revision: number } | null;
  request_id: string;
  provider: Provider;
  duration_ms: number | null;
  file_url?: string;
  temporary_url?: string | null;
  transcript?: string;
  detected_language?: string | null;
  media_type?: string;
}
export interface Capabilities {
  tts: { output_format: string; languages: string[] };
  stt: { languages: string[]; accepted_media_types: string[] };
}
export class ApiError extends Error {
  constructor(
    message: string,
    public status: number,
    public code: string,
    public requestId?: string,
    public retryAfter?: string,
  ) {
    super(message);
  }
}
export async function api<T>(
  path: string,
  options: {
    method?: string;
    body?: unknown;
    headers?: Record<string, string>;
    signal?: AbortSignal;
  } = {},
): Promise<T> {
  const response = await fetch(path, {
    method: options.method || "GET",
    credentials: "same-origin",
    headers: {
      ...(session()?.context
        ? { "x-waveform-context": session()!.context }
        : {}),
      ...(options.body !== undefined
        ? { "content-type": "application/json" }
        : {}),
      ...options.headers,
    },
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
    signal: options.signal,
  });
  const body =
    response.status === 204
      ? undefined
      : await response.json().catch(() => undefined);
  if (!response.ok)
    throw new ApiError(
      body?.error?.message || `Request failed (${response.status}).`,
      response.status,
      body?.error?.code || "request_failed",
      body?.error?.request_id ||
        response.headers.get("x-request-id") ||
        undefined,
      response.headers.get("retry-after") || undefined,
    );
  return body as T;
}
export const [session, setSession] = createSignal<Session>();
export const [preferencesVersion, setPreferencesVersion] = createSignal(0);
export const [contextVersion, setContextVersion] = createSignal(0);
export function acceptSession(value: Session) {
  setSession(value);
  setContextVersion((v) => v + 1);
}
export async function sessionAction(action: string, body: unknown = {}) {
  const value = await api<Session>(`/api/session/${action}`, {
    method: "POST",
    body,
  });
  acceptSession(value);
  return value;
}
export const providerNames: Record<Provider, string> = {
  gemini: "Gemini",
  elevenlabs: "ElevenLabs",
  openai: "OpenAI",
  deepgram: "Deepgram",
};
export const models: Record<Operation, Partial<Record<Provider, string>>> = {
  tts: {
    gemini: "3.1 Flash TTS Preview",
    elevenlabs: "Multilingual v2",
    openai: "tts-1",
  },
  stt: {
    gemini: "3.5 Transcribe",
    openai: "gpt-transcribe",
    deepgram: "Nova-3 Multilingual",
  },
};
export const duration = (ms: number | null | undefined) =>
  ms == null ? "—" : `${(ms / 1000).toFixed(1)}s`;
export const date = (value?: string | null) =>
  value
    ? new Date(value).toLocaleString([], {
        dateStyle: "medium",
        timeStyle: "short",
      })
    : "—";
export function safeUrl(value?: string | null) {
  if (!value) return undefined;
  try {
    const url = new URL(value);
    return url.protocol === "https:" && !url.username && !url.password
      ? url.href
      : undefined;
  } catch {
    return undefined;
  }
}
