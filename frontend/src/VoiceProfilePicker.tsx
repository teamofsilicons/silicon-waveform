import { For, Show } from "solid-js";
import type { VoiceProfile } from "./api";

export default function VoiceProfilePicker(props: {
  profiles: VoiceProfile[];
  value: string;
  change: (id: string) => void;
  accountDefault?: string;
  disabled?: boolean;
}) {
  const selected = () =>
    props.profiles.find((p) => p.id === (props.value || props.accountDefault));
  const defaultName = () =>
    props.profiles.find((p) => p.id === props.accountDefault)?.name;
  return (
    <div class="voice-picker">
      <label>
        Voice profile
        <select
          value={props.value}
          disabled={props.disabled || !props.profiles.length}
          onChange={(e) => props.change(e.currentTarget.value)}
        >
          <Show when={props.accountDefault}>
            <option value="">
              Account default · {defaultName() || props.accountDefault}
            </option>
          </Show>
          <For each={props.profiles}>
            {(p) => (
              <option value={p.id}>
                {p.name} · {p.description}
              </option>
            )}
          </For>
        </select>
      </label>
      <Show when={selected()}>
        {(profile) => (
          <div class="voice-description">
            <p class="hint">
              {profile().description}. Fallback uses this profile’s saved voice
              mappings.
            </p>
            <details>
              <summary>Provider voices</summary>
              <dl class="voice-mappings">
                <dt>Gemini</dt>
                <dd>{profile().gemini_voice}</dd>
                <dt>OpenAI</dt>
                <dd>{profile().openai_voice}</dd>
                <dt>ElevenLabs</dt>
                <dd>
                  <a
                    class="text-link"
                    href={`https://elevenlabs.io/app/voice-library?search=${encodeURIComponent(profile().elevenlabs.voice_id)}`}
                    target="_blank"
                    rel="noreferrer"
                  >
                    View mapped voice
                  </a>
                </dd>
              </dl>
              <Show when={!profile().auditioned}>
                <p class="hint">
                  Initial mapping · Listening review pending. Voices may differ
                  across providers.
                </p>
              </Show>
            </details>
          </div>
        )}
      </Show>
    </div>
  );
}
