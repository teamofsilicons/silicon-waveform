import { For, Show } from "solid-js";
import { providerNames, type Provider, type TtsProviderOptions } from "./api";

interface Field {
  key: string;
  label: string;
  type?: "number" | "multiline" | "select" | "boolean";
  placeholder?: string;
  hint?: string;
  min?: number;
  max?: number;
  step?: number;
  choices?: { value: string; label: string }[];
}

const fields: Record<keyof TtsProviderOptions, Field[]> = {
  gemini: [
    { key: "voice", label: "Voice", placeholder: "Use voice profile" },
    {
      key: "scene",
      label: "Scene",
      type: "multiline",
      placeholder: "A quiet conversation in a small kitchen…",
      hint: "Prompt guidance for the setting, mood, and performance.",
    },
    { key: "audio_profile", label: "Audio profile", type: "multiline", placeholder: "Describe the speaker’s identity and vocal qualities" },
    { key: "director_notes", label: "Director notes", type: "multiline", placeholder: "Describe delivery, pacing, emotion, and accent" },
    { key: "sample_context", label: "Sample context", type: "multiline", placeholder: "Context or a performance example" },
  ],
  elevenlabs: [
    { key: "voice_id", label: "Voice ID", placeholder: "Use voice profile" },
    { key: "model_id", label: "Model ID", placeholder: "Use voice profile model" },
    { key: "stability", label: "Stability", type: "number", min: 0, max: 1, step: 0.01 },
    { key: "similarity_boost", label: "Similarity boost", type: "number", min: 0, max: 1, step: 0.01 },
    { key: "style", label: "Style", type: "number", min: 0, max: 1, step: 0.01 },
    { key: "speed", label: "Speed", type: "number", min: 0.7, max: 1.2, step: 0.01 },
    {
      key: "use_speaker_boost",
      label: "Speaker boost",
      type: "boolean",
      choices: [{ value: "true", label: "On" }, { value: "false", label: "Off" }],
    },
    { key: "seed", label: "Seed", type: "number", min: 0, max: 4294967295, step: 1 },
    { key: "previous_text", label: "Previous text", type: "multiline", placeholder: "Text spoken immediately before this passage" },
    { key: "next_text", label: "Next text", type: "multiline", placeholder: "Text spoken immediately after this passage" },
    {
      key: "apply_text_normalization",
      label: "Text normalization",
      type: "select",
      choices: [{ value: "auto", label: "Auto" }, { value: "on", label: "On" }, { value: "off", label: "Off" }],
    },
  ],
  openai: [
    { key: "voice", label: "Voice", placeholder: "Use voice profile" },
    {
      key: "model",
      label: "Model",
      type: "select",
      placeholder: "Default (tts-1)",
      choices: [
        { value: "tts-1", label: "tts-1" },
        { value: "tts-1-hd", label: "tts-1-hd" },
        { value: "gpt-4o-mini-tts", label: "gpt-4o-mini-tts" },
      ],
    },
    { key: "speed", label: "Speed", type: "number", min: 0.25, max: 4, step: 0.01 },
    {
      key: "instructions",
      label: "Delivery instructions",
      type: "multiline",
      placeholder: "Speak warmly, with a calm and measured pace…",
      hint: "Available with gpt-4o-mini-tts.",
    },
  ],
};

export default function ProviderControls(props: {
  provider: Provider;
  options: TtsProviderOptions;
  change: (options: TtsProviderOptions) => void;
  disabled?: boolean;
}) {
  const provider = () => props.provider as keyof TtsProviderOptions;
  const options = () =>
    (props.options[provider()] || {}) as Record<string, string | number | boolean | undefined>;
  function update(field: Field, raw: string) {
    const value = raw === ""
      ? undefined
      : field.type === "number"
        ? Number(raw)
        : field.type === "boolean"
          ? raw === "true"
          : raw;
    props.change({
      ...props.options,
      [provider()]: {
        ...options(),
        [field.key]: value,
        ...(provider() === "openai" && field.key === "model" && value !== "gpt-4o-mini-tts"
          ? { instructions: undefined }
          : {}),
      },
    });
  }
  return (
    <details class="provider-controls">
      <summary>{providerNames[props.provider]} voice controls <span class="label-note">optional</span></summary>
      <p class="hint">Set only what you need. Blank fields use your voice profile or provider defaults. These controls apply to this request.</p>
      <div class="provider-fields">
        <For each={fields[provider()] || []}>
          {(field) => (
            <Show when={field.key !== "instructions" || props.options.openai?.model === "gpt-4o-mini-tts"}>
              <label classList={{ "full-width": field.type === "multiline" }}>
                {field.label}
                <Show
                  when={field.type === "select" || field.type === "boolean"}
                  fallback={
                    <Show
                      when={field.type === "multiline"}
                      fallback={
                        <input
                          type={field.type === "number" ? "number" : "text"}
                          value={String(options()[field.key] ?? "")}
                          onInput={(e) => update(field, e.currentTarget.value)}
                          placeholder={field.placeholder || "Default"}
                          min={field.min}
                          max={field.max}
                          step={field.step}
                          maxlength={256}
                          disabled={props.disabled}
                        />
                      }
                    >
                      <textarea
                        rows={3}
                        value={String(options()[field.key] ?? "")}
                        onInput={(e) => update(field, e.currentTarget.value)}
                        placeholder={field.placeholder}
                        maxlength={4096}
                        disabled={props.disabled}
                      />
                    </Show>
                  }
                >
                  <select
                    value={String(options()[field.key] ?? "")}
                    onChange={(e) => update(field, e.currentTarget.value)}
                    disabled={props.disabled}
                  >
                    <option value="">{field.placeholder || "Default"}</option>
                    <For each={field.choices}>{(choice) => <option value={choice.value}>{choice.label}</option>}</For>
                  </select>
                </Show>
                <Show when={field.hint}><span class="hint">{field.hint}</span></Show>
                <Show when={field.type === "number"}><span class="hint">{field.min}–{field.max}</span></Show>
              </label>
            </Show>
          )}
        </For>
      </div>
    </details>
  );
}
