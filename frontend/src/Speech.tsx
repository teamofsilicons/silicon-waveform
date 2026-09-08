import {
  createEffect,
  createResource,
  createSignal,
  For,
  onCleanup,
  Show,
} from "solid-js";
import {
  api,
  ApiError,
  duration,
  providerNames,
  safeUrl,
  session,
  preferencesVersion,
  type Capabilities,
  type Operation,
  type Preferences,
  type Provider,
  type SpeechResult,
} from "./api";
import { Copy, Empty, Heading, Icon, Notice, Order } from "./ui";
import VoiceProfilePicker from "./VoiceProfilePicker";
import type { VoiceProfile } from "./api";
export default function Speech(props: {
  operation: Operation;
  signin: () => void;
}) {
  const [text, setText] = createSignal(""),
    [url, setUrl] = createSignal(""),
    [language, setLanguage] = createSignal("");
  const [custom, setCustom] = createSignal(false),
    [order, setOrder] = createSignal<Provider[]>([]);
  const [busy, setBusy] = createSignal(false),
    [error, setError] = createSignal<unknown>(),
    [result, setResult] = createSignal<SpeechResult>();
  const [requestId, setRequestId] = createSignal(""),
    [elapsed, setElapsed] = createSignal(0);
  const [caps] = createResource(() =>
    api<Capabilities>("/api/v1/capabilities"),
  );
  const [prefs] = createResource(
    () => session()?.authenticated && preferencesVersion() + 1,
    () => api<Preferences>("/api/v1/preferences"),
  );
  createEffect(() => {
    if (!prefs.error && prefs()) setOrder(prefs()![`${props.operation}_order`]);
  });
  const [voice, setVoice] = createSignal("");
  const [profiles] = createResource(
    () => session()?.authenticated && props.operation === "tts",
    () => api<{ items: VoiceProfile[] }>("/api/v1/voice-profiles"),
  );
  const count = () => [...text()].length;
  let attempt: { signature: string; key: string; id: string } | undefined;
  let mounted = true;
  onCleanup(() => {
    mounted = false;
  });
  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (busy()) return;
    if (!session()?.authenticated) {
      props.signin();
      return;
    }
    setError();
    setResult();
    if (props.operation === "tts" && (!text().trim() || count() > 4096)) {
      setError(new Error("Enter between 1 and 4,096 characters."));
      return;
    }
    if (props.operation === "stt" && !safeUrl(url().trim())) {
      setError(new Error("Enter an HTTPS permanent file link from Briefcase."));
      return;
    }
    if (language()) {
      try {
        Intl.getCanonicalLocales(language());
      } catch {
        setError(new Error("Use a valid language tag, such as en-US or hi."));
        return;
      }
    }
    const body =
      props.operation === "tts"
        ? {
            text: text(),
            ...(voice() ? { voice_profile: voice() } : {}),
            ...(language() ? { lang: language() } : {}),
            ...(custom() ? { provider_order: order() } : {}),
          }
        : {
            file_url: url().trim(),
            ...(language() ? { language: language() } : {}),
            ...(custom() ? { provider_order: order() } : {}),
          };
    const signature = JSON.stringify(body);
    if (!attempt || attempt.signature !== signature)
      attempt = {
        signature,
        key: crypto.randomUUID(),
        id: crypto.randomUUID(),
      };
    setRequestId(attempt.id);
    setBusy(true);
    setElapsed(0);
    const timer = setInterval(() => {
      if (mounted) setElapsed((s) => s + 1);
    }, 1000);
    try {
      const response = await api<SpeechResult>(`/api/v1/${props.operation}`, {
        method: "POST",
        body,
        headers: { "idempotency-key": attempt.key, "x-request-id": attempt.id },
      });
      if (mounted) {
        setResult(response);
        setRequestId(response.request_id);
      }
      attempt = undefined;
    } catch (err) {
      if (mounted) setError(err);
      if (
        err instanceof ApiError &&
        [400, 403, 404, 413, 415].includes(err.status)
      )
        attempt = undefined;
    } finally {
      clearInterval(timer);
      if (mounted) setBusy(false);
    }
  }
  return (
    <>
      <Heading
        eyebrow="SPEECH WORKSPACE"
        title={props.operation === "tts" ? "Text to speech" : "Speech to text"}
      >
        {props.operation === "tts"
          ? "Give your words a voice."
          : "Turn your audio into something you can read."}
      </Heading>
      <Show when={session()?.plane === "test"}>
        <div class="test-banner">
          <Icon name="test" />
          <span>
            Testing · {session()?.environment?.name}. Requests return the shared
            test fixture; no speech provider is charged.
          </span>
        </div>
      </Show>
      <div class="speech-layout">
        <section class="panel composer">
          <form onSubmit={submit}>
            <div class="panel-heading">
              <h2>{props.operation === "tts" ? "Your text" : "Your audio"}</h2>
              <span class="mono muted">
                {props.operation === "tts" ? "01 / COMPOSE" : "01 / SOURCE"}
              </span>
            </div>
            <Show
              when={props.operation === "tts"}
              fallback={
                <div class="source-fields">
                  <label>
                    Briefcase file link
                    <input
                      type="url"
                      placeholder="https://briefcase.teamofsilicons.com/…"
                      required
                      value={url()}
                      onInput={(e) => setUrl(e.currentTarget.value)}
                      disabled={busy()}
                    />
                  </label>
                  <p class="hint">
                    Upload your audio or video to Briefcase, then paste its
                    permanent link here. Your file permissions carry over.
                  </p>
                  <a
                    class="text-link"
                    href="https://briefcase.teamofsilicons.com"
                    target="_blank"
                    rel="noreferrer"
                  >
                    Open Briefcase <Icon name="external" size={14} />
                  </a>
                  <Show when={!caps.error && caps()}>
                    <p class="hint formats">
                      Up to 25 MiB ·{" "}
                      {caps()?.stt.accepted_media_types.join(", ")}
                    </p>
                  </Show>
                </div>
              }
            >
              <label class="sr-only" for="speech-text">
                Text to speak
              </label>
              <textarea
                id="speech-text"
                class="speech-text"
                placeholder="Write something worth hearing…"
                value={text()}
                onInput={(e) => setText(e.currentTarget.value)}
                disabled={busy()}
                required
              />
              <div class="text-count" classList={{ invalid: count() > 4096 }}>
                <span>Plain text</span>
                <span class="mono">{count().toLocaleString()} / 4,096</span>
              </div>
            </Show>
            <div class="composer-options">
              <label>
                Language <span class="label-note">optional</span>
                <input
                  list={`languages-${props.operation}`}
                  placeholder="Auto-detect"
                  value={language()}
                  onInput={(e) => setLanguage(e.currentTarget.value.trim())}
                  disabled={busy()}
                />
                <datalist id={`languages-${props.operation}`}>
                  <For
                    each={
                      !caps.error ? caps()?.[props.operation].languages : []
                    }
                  >
                    {(value) => <option value={value} />}
                  </For>
                </datalist>
              </label>
              <div class="option-description">
                {props.operation === "tts"
                  ? "Language hints apply to Gemini. Fallback providers infer the language from your text."
                  : "Leave blank to let the provider detect the language."}
              </div>
            </div>
            <Show when={props.operation === "tts" && session()?.authenticated}>
              <div class="composer-voice">
                <Notice error={profiles.error} />
                <Show
                  when={
                    !profiles.error && profiles() && !prefs.error && prefs()
                  }
                >
                  <VoiceProfilePicker
                    profiles={profiles()!.items}
                    value={voice()}
                    change={setVoice}
                    accountDefault={prefs()?.voice_profile}
                    disabled={busy() || prefs.loading}
                  />
                </Show>
              </div>
            </Show>
            <Notice error={error()} />
            <Show when={busy()}>
              <div class="processing" role="status">
                <span class="spinner" />{" "}
                {props.operation === "tts"
                  ? "Generating your audio"
                  : "Transcribing your file"}
                … <span class="mono">{elapsed()}s</span>
                <a href="#history">View job status</a>
              </div>
            </Show>
            <div class="composer-footer">
              <span class="hint">
                {props.operation === "tts"
                  ? "MP3 audio · Stored in Briefcase"
                  : "One file. A clear transcript."}
              </span>
              <button
                class="button primary"
                disabled={
                  busy() ||
                  (props.operation === "tts"
                    ? !text().trim() || count() > 4096
                    : !url().trim())
                }
                type="submit"
              >
                {busy()
                  ? "Working…"
                  : props.operation === "tts"
                    ? "Generate speech"
                    : "Transcribe audio"}
                <Icon name="arrow" />
              </button>
            </div>
            <Show when={requestId()}>
              <p class="request-foot mono">Request {requestId()}</p>
            </Show>
          </form>
        </section>
        <aside class="panel routing">
          <div class="panel-heading">
            <h2>Provider order</h2>
            <span class="subtle-icon">
              <Icon name="settings" />
            </span>
          </div>
          <p class="hint">
            Your first choice runs first. If it fails, Waveform tries the next
            provider.
          </p>
          <Show
            when={!prefs.error && prefs()}
            fallback={
              <>
                <Notice error={prefs.error} />
                <p class="muted">
                  {session()?.authenticated
                    ? "Loading your preferences…"
                    : "Sign in to see your saved provider order."}
                </p>
              </>
            }
          >
            <label class="check-label">
              <input
                type="checkbox"
                checked={custom()}
                onChange={(e) => {
                  setCustom(e.currentTarget.checked);
                  setOrder(prefs()![`${props.operation}_order`]);
                }}
                disabled={busy()}
              />
              Customize for this request
            </label>
            <Order
              operation={props.operation}
              value={order()}
              change={setOrder}
              disabled={!custom() || busy()}
            />
            <p class="hint">
              {custom()
                ? "Applies to this request only."
                : "Using your account preferences."}
            </p>
          </Show>
          <a class="text-link" href="#settings">
            Manage preferences <Icon name="arrow" size={15} />
          </a>
        </aside>
      </div>
      <section class="panel result-panel">
        <div class="panel-heading">
          <h2>
            {props.operation === "tts" ? "Generated audio" : "Transcript"}
          </h2>
          <span class="mono muted">02 / RESULT</span>
        </div>
        <Show
          when={result()}
          fallback={
            <Empty
              title={
                props.operation === "tts"
                  ? "Your voice starts here"
                  : "Your transcript will appear here"
              }
              icon={props.operation === "tts" ? "speech" : "text"}
            >
              {props.operation === "tts"
                ? "Generate speech to get your audio and its Briefcase link."
                : "Submit a Briefcase file to see its transcription."}
            </Empty>
          }
        >
          {(value) => (
            <div class="result-content">
              <div class="result-meta">
                <span class="badge completed">Completed</span>
                <span>{providerNames[value().provider]}</span>
                <Show when={value().voice_profile}>
                  {(profile) => <span>Voice · {profile().id}</span>}
                </Show>
                <span>{duration(value().duration_ms)}</span>
                <Show when={value().detected_language}>
                  <span>{value().detected_language}</span>
                </Show>
              </div>
              <Show
                when={value().transcript !== undefined}
                fallback={
                  <>
                    <Show when={safeUrl(value().temporary_url)}>
                      {(src) => (
                        <audio controls src={src()} preload="metadata" />
                      )}
                    </Show>
                    <p>Your audio is saved in Briefcase.</p>
                    <div class="actions">
                      <a
                        class="button primary"
                        href={safeUrl(value().file_url)}
                        target="_blank"
                        rel="noreferrer"
                      >
                        Open audio in Briefcase
                        <Icon name="external" size={15} />
                      </a>
                      <Copy
                        value={value().file_url || ""}
                        label="Copy file link"
                      />
                    </div>
                  </>
                }
              >
                <pre class="transcript">{value().transcript}</pre>
                <div class="actions">
                  <Copy
                    value={value().transcript || ""}
                    label="Copy transcript"
                  />
                  <a
                    class="button"
                    download="waveform-transcript.txt"
                    href={`data:text/plain;charset=utf-8,${encodeURIComponent(value().transcript || "")}`}
                  >
                    Download text
                  </a>
                </div>
              </Show>
              <small class="mono muted">{value().request_id}</small>
            </div>
          )}
        </Show>
      </section>
    </>
  );
}
