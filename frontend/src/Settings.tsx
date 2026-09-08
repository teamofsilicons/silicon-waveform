import { createResource, createSignal, For, Show } from "solid-js";
import {
  api,
  date,
  providerNames,
  session,
  setPreferencesVersion,
  type Preferences,
  type Provider,
} from "./api";
import { Empty, Heading, Modal, Notice, Order } from "./ui";
import VoiceProfilePicker from "./VoiceProfilePicker";
import type { VoiceProfile } from "./api";
export default function Settings(props: { signin: () => void }) {
  const [prefs, { refetch }] = createResource(
    () => session()?.authenticated,
    () => api<Preferences>("/api/v1/preferences"),
  );
  const [keys, { refetch: refreshKeys }] = createResource(
    () => session()?.authenticated,
    () =>
      api<{
        items: {
          provider: Provider;
          configured: boolean;
          updated_at: string;
        }[];
      }>("/api/v1/provider-keys"),
  );
  const [profiles] = createResource(
    () => session()?.authenticated,
    () => api<{ items: VoiceProfile[] }>("/api/v1/voice-profiles"),
  );
  const [voice, setVoice] = createSignal<string>();
  const [tts, setTts] = createSignal<Provider[]>(),
    [stt, setStt] = createSignal<Provider[]>(),
    [busy, setBusy] = createSignal(false),
    [error, setError] = createSignal<unknown>(),
    [notice, setNotice] = createSignal("");
  const [editing, setEditing] = createSignal<Provider>(),
    [removing, setRemoving] = createSignal<Provider>(),
    [secret, setSecret] = createSignal("");
  async function save() {
    setBusy(true);
    setError();
    setNotice("");
    try {
      await api("/api/v1/preferences", {
        method: "PATCH",
        body: {
          ...(voice() ? { voice_profile: voice() } : {}),
          tts_order: tts() || prefs()?.tts_order,
          stt_order: stt() || prefs()?.stt_order,
        },
      });
      await refetch();
      setVoice();
      setTts();
      setStt();
      setPreferencesVersion((v) => v + 1);
      setNotice("Preferences saved.");
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }
  async function saveKey(e: SubmitEvent) {
    e.preventDefault();
    setBusy(true);
    setError();
    setNotice("");
    try {
      await api(`/api/v1/provider-keys/${editing()}`, {
        method: "PUT",
        body: { api_key: secret().trim() },
      });
      setSecret("");
      setEditing();
      await refreshKeys();
      setNotice("Your key is connected.");
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }
  async function remove() {
    setBusy(true);
    setError();
    setNotice("");
    try {
      await api(`/api/v1/provider-keys/${removing()}`, {
        method: "DELETE",
        body: {},
      });
      setRemoving();
      await refreshKeys();
      setNotice("Your key was removed. Waveform will use its shared key.");
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }
  return (
    <>
      <Heading eyebrow="ACCOUNT" title="Provider settings">
        Your providers. Your preferred order.
      </Heading>
      <Notice error={error() || prefs.error || keys.error} message={notice()} />
      <Show
        when={session()?.authenticated}
        fallback={
          <section class="panel">
            <Empty icon="settings" title="Make Waveform yours">
              <button class="button primary" onClick={props.signin}>
                Sign in to manage providers
              </button>
            </Empty>
          </section>
        }
      >
        <Show when={!prefs.error && prefs()}>
          <section class="panel settings-panel">
            <div class="panel-heading">
              <div>
                <h2>Default provider order</h2>
                <p class="hint">
                  Saved for your account in this environment. Individual
                  requests can override it.
                </p>
              </div>
            </div>
            <div class="settings-voice">
              <h3>Default voice profile</h3>
              <p class="hint">
                Used for new speech requests unless you choose another profile
                for that generation.
              </p>
              <Notice error={profiles.error} />
              <Show when={!profiles.error && profiles()}>
                <VoiceProfilePicker
                  profiles={profiles()!.items}
                  value={voice() || prefs()!.voice_profile}
                  change={setVoice}
                  disabled={busy()}
                />
              </Show>
            </div>
            <div class="two-columns">
              <div>
                <h3>Text to speech</h3>
                <Order
                  operation="tts"
                  value={tts() || prefs()!.tts_order}
                  change={setTts}
                  disabled={busy()}
                />
              </div>
              <div>
                <h3>Speech to text</h3>
                <Order
                  operation="stt"
                  value={stt() || prefs()!.stt_order}
                  change={setStt}
                  disabled={busy()}
                />
              </div>
            </div>
            <div class="panel-footer">
              <button
                class="button"
                disabled={busy()}
                onClick={() => {
                  setVoice(prefs()!.defaults.voice_profile);
                  setTts([...prefs()!.defaults.tts_order]);
                  setStt([...prefs()!.defaults.stt_order]);
                }}
              >
                Use workspace defaults
              </button>
              <button
                class="button primary"
                disabled={busy() || (!tts() && !stt() && !voice())}
                onClick={() => void save()}
              >
                {busy() ? "Saving…" : "Save preferences"}
              </button>
            </div>
          </section>
        </Show>
        <section class="panel settings-panel">
          <div class="panel-heading">
            <div>
              <h2>Connected API keys</h2>
              <p class="hint">
                Waveform provides shared keys. Connect your own to use your
                provider accounts.
              </p>
            </div>
          </div>
          <div class="provider-rows">
            <For each={Object.keys(providerNames) as Provider[]}>
              {(provider) => {
                const configured = () =>
                  keys.error
                    ? undefined
                    : keys()?.items.find((k) => k.provider === provider);
                return (
                  <div class="provider-row">
                    <div class="provider-initial">
                      {providerNames[provider].slice(0, 1)}
                    </div>
                    <div class="grow">
                      <h3>{providerNames[provider]}</h3>
                      <p class="hint">
                        {configured()
                          ? `Your key · Updated ${date(configured()!.updated_at)}`
                          : "Using Waveform’s shared key"}
                      </p>
                    </div>
                    <span
                      class={`badge ${configured() ? "completed" : "neutral"}`}
                    >
                      {configured() ? "Connected" : "Shared"}
                    </span>
                    <button
                      class="button"
                      disabled={busy() || keys.loading || !!keys.error}
                      onClick={() => {
                        setError();
                        setSecret("");
                        setEditing(provider);
                      }}
                    >
                      {configured() ? "Replace key" : "Connect key"}
                    </button>
                    <Show when={configured()}>
                      <button
                        class="text-button danger"
                        aria-label={`Remove ${providerNames[provider]} key`}
                        disabled={busy()}
                        onClick={() => setRemoving(provider)}
                      >
                        Remove
                      </button>
                    </Show>
                  </div>
                );
              }}
            </For>
          </div>
          <p class="hint key-note">
            Keys are encrypted by the backend and are never returned after
            saving.
          </p>
        </section>
      </Show>
      <Show when={editing()}>
        {(provider) => (
          <Modal
            title={`Connect ${providerNames[provider()]} key`}
            close={() => {
              if (!busy()) {
                setEditing();
                setSecret("");
                setError();
              }
            }}
          >
            <form class="stack" onSubmit={saveKey}>
              <p class="muted">
                This key is used only for your account in the current
                environment.
              </p>
              <label>
                API key
                <input
                  type="password"
                  autocomplete="off"
                  value={secret()}
                  onInput={(e) => setSecret(e.currentTarget.value)}
                  required
                  maxlength={16384}
                />
              </label>
              <Notice error={error()} />
              <button
                class="button primary"
                disabled={busy() || !secret().trim()}
              >
                Save key
              </button>
            </form>
          </Modal>
        )}
      </Show>
      <Show when={removing()}>
        {(provider) => (
          <Modal
            title={`Remove ${providerNames[provider()]} key?`}
            close={() => !busy() && setRemoving()}
          >
            <div class="stack">
              <p>
                Future requests will use Waveform’s shared key for this
                provider.
              </p>
              <Notice error={error()} />
              <button
                class="button danger-button"
                disabled={busy()}
                onClick={() => void remove()}
              >
                Remove key
              </button>
            </div>
          </Modal>
        )}
      </Show>
    </>
  );
}
