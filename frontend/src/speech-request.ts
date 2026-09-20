import type { Operation, Provider, TtsProviderOptions } from "./api";

export interface SpeechDraft {
  operation: Operation;
  text: string;
  url: string;
  language: string;
  voice: string;
  order: Provider[];
  customOrder: boolean;
  autoFallback: boolean;
  providerOptions: TtsProviderOptions;
  requestKey: string;
}

export function speechRequest(draft: SpeechDraft) {
  const provider = draft.order[0];
  const options =
    provider && provider !== "deepgram"
      ? draft.providerOptions[provider]
      : undefined;
  const common = {
    ...(draft.customOrder ? { provider_order: draft.order } : {}),
    ...(draft.requestKey.trim() && provider
      ? { provider_keys: { [provider]: draft.requestKey.trim() } }
      : {}),
  };
  if (draft.operation === "stt")
    return {
      ...common,
      file_url: draft.url.trim(),
      ...(draft.language ? { language: draft.language } : {}),
    };

  const configured = Object.fromEntries(
    Object.entries(options || {}).filter(([, value]) => value !== undefined),
  );
  return {
    ...common,
    text: draft.text,
    auto_fallback: draft.autoFallback,
    ...(draft.voice ? { voice_profile: draft.voice } : {}),
    ...(draft.language ? { lang: draft.language } : {}),
    ...(!draft.autoFallback && Object.keys(configured).length
      ? { provider_options: { [provider]: configured } }
      : {}),
  };
}

/** Retry bookkeeping must never retain a provider key. Key-bearing requests use fresh IDs. */
export function publicRequestSignature(body: ReturnType<typeof speechRequest>) {
  const { provider_keys: _keys, ...publicBody } = body;
  return JSON.stringify(publicBody);
}
