import { test } from "node:test";
import assert from "node:assert/strict";
import { publicRequestSignature, speechRequest } from "../src/speech-request.ts";

const draft = {
  operation: "tts",
  text: "Hello there.",
  url: "https://briefcase.teamofsilicons.com/audio",
  language: "",
  voice: "",
  order: ["gemini", "elevenlabs", "openai"],
  customOrder: false,
  autoFallback: false,
  providerOptions: {},
  requestKey: "",
};

test("TTS disables automatic fallback and preserves account defaults", () => {
  assert.deepEqual(speechRequest(draft), {
    text: "Hello there.",
    auto_fallback: false,
  });
});

test("only the selected TTS provider receives its controls", () => {
  assert.deepEqual(speechRequest({
    ...draft,
    customOrder: true,
    order: ["elevenlabs", "gemini", "openai"],
    providerOptions: {
      gemini: { scene: "A quiet room" },
      elevenlabs: { stability: 0, use_speaker_boost: false, seed: 0, speed: undefined },
    },
  }), {
    text: "Hello there.",
    auto_fallback: false,
    provider_order: ["elevenlabs", "gemini", "openai"],
    provider_options: { elevenlabs: { stability: 0, use_speaker_boost: false, seed: 0 } },
  });
});

test("automatic fallback does not send provider-specific controls", () => {
  const body = speechRequest({
    ...draft,
    autoFallback: true,
    providerOptions: { gemini: { scene: "A quiet room" } },
  });
  assert.equal(body.auto_fallback, true);
  assert.equal("provider_options" in body, false);
});

test("STT keeps server fallback behavior and supports a request key", () => {
  assert.deepEqual(speechRequest({
    ...draft,
    operation: "stt",
    language: "en",
    order: ["openai", "gemini", "deepgram"],
    customOrder: true,
    requestKey: "  private-test-key  ",
    providerOptions: { openai: { instructions: "Read slowly" } },
  }), {
    file_url: "https://briefcase.teamofsilicons.com/audio",
    language: "en",
    provider_order: ["openai", "gemini", "deepgram"],
    provider_keys: { openai: "private-test-key" },
  });
});

test("retry bookkeeping never contains a per-request provider key", () => {
  const body = speechRequest({ ...draft, requestKey: "private-test-key" });
  assert.deepEqual(body.provider_keys, { gemini: "private-test-key" });
  assert.equal(publicRequestSignature(body), publicRequestSignature(speechRequest(draft)));
  assert.equal(publicRequestSignature(body).includes("private-test-key"), false);
  assert.equal(publicRequestSignature(body).includes("provider_keys"), false);
});
